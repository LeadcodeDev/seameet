use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use seameet_core::{ParticipantId, SeaMeetError};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::message::SdpMessage;

/// Per-participant outbound channel.
pub type WsSink = mpsc::UnboundedSender<String>;

// ── Domain types ────────────────────────────────────────────────────────

/// A connected participant within the signaling system.
#[derive(Debug, Clone)]
pub struct Member {
    /// The participant's unique identifier.
    pub id: ParticipantId,
    /// Outbound channel to send messages to this member's WebSocket.
    sink: WsSink,
}

impl Member {
    /// Creates a new member.
    pub fn new(id: ParticipantId, sink: WsSink) -> Self {
        Self { id, sink }
    }

    /// Sends a serialized message to this member. Returns `true` on success.
    pub fn send(&self, msg: &str) -> bool {
        self.sink.send(msg.to_owned()).is_ok()
    }

    /// Returns a clone of the underlying sink.
    pub fn sink(&self) -> WsSink {
        self.sink.clone()
    }
}

/// A named signaling room containing members.
#[derive(Debug)]
pub struct Room {
    members: HashMap<ParticipantId, Member>,
}

impl Room {
    /// Creates an empty room.
    pub fn new() -> Self {
        Self {
            members: HashMap::new(),
        }
    }

    /// Adds a member to the room. Returns `true` if this is the first member (initiator).
    pub fn join(&mut self, member: Member) -> bool {
        let is_first = self.members.is_empty();
        self.members.insert(member.id, member);
        is_first
    }

    /// Removes a member from the room. Returns `true` if the room is now empty.
    pub fn leave(&mut self, id: &ParticipantId) -> bool {
        self.members.remove(id);
        self.members.is_empty()
    }

    /// Returns `true` if the room has no members.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Returns the member with the given id, if present.
    pub fn get(&self, id: &ParticipantId) -> Option<&Member> {
        self.members.get(id)
    }

    /// Sends a message to all members except `exclude`.
    pub fn broadcast(&self, msg: &str, exclude: &ParticipantId) {
        for member in self.members.values() {
            if member.id != *exclude {
                member.send(msg);
            }
        }
    }

    /// Sends a message to a specific member by id.
    pub fn send_to(&self, id: &ParticipantId, msg: &str) {
        if let Some(member) = self.members.get(id) {
            member.send(msg);
        }
    }

    /// Returns the sinks of all members except `exclude`.
    pub fn peer_sinks(&self, exclude: &ParticipantId) -> Vec<WsSink> {
        self.members
            .values()
            .filter(|m| m.id != *exclude)
            .map(|m| m.sink.clone())
            .collect()
    }
}

impl Default for Room {
    fn default() -> Self {
        Self::new()
    }
}

// ── Signaling state ─────────────────────────────────────────────────────

/// Shared signaling state: rooms indexed by name, with a reverse index
/// from participant to their joined rooms for efficient disconnect cleanup.
pub struct SignalingState {
    /// Room name → room instance.
    rooms: HashMap<String, Room>,
    /// Participant → (sink, list of room names they belong to).
    connections: HashMap<ParticipantId, (WsSink, Vec<String>)>,
}

impl SignalingState {
    /// Creates empty state.
    pub fn new() -> Self {
        Self {
            rooms: HashMap::new(),
            connections: HashMap::new(),
        }
    }

    /// Registers a participant in a room.
    /// Returns `true` if this is the first participant (initiator).
    pub fn join(&mut self, id: ParticipantId, room_id: &str, sink: WsSink) -> bool {
        let room = self.rooms.entry(room_id.to_owned()).or_default();
        let is_first = room.join(Member::new(id, sink.clone()));

        let conn = self
            .connections
            .entry(id)
            .or_insert_with(|| (sink, Vec::new()));
        if !conn.1.contains(&room_id.to_owned()) {
            conn.1.push(room_id.to_owned());
        }

        is_first
    }

    /// Removes a participant from a specific room.
    /// Returns `true` if the room is now empty (and was removed).
    pub fn leave(&mut self, id: &ParticipantId, room_id: &str) -> bool {
        let room_empty = if let Some(room) = self.rooms.get_mut(room_id) {
            let empty = room.leave(id);
            if empty {
                self.rooms.remove(room_id);
            }
            empty
        } else {
            true
        };

        if let Some((_, rooms)) = self.connections.get_mut(id) {
            rooms.retain(|r| r != room_id);
            if rooms.is_empty() {
                self.connections.remove(id);
            }
        }

        room_empty
    }

    /// Removes a participant from ALL rooms (called on WebSocket disconnect).
    /// Returns the list of affected room names with whether each is now empty.
    pub fn disconnect(&mut self, id: &ParticipantId) -> Vec<(String, bool)> {
        let room_ids: Vec<String> = self
            .connections
            .get(id)
            .map(|(_, rooms)| rooms.clone())
            .unwrap_or_default();

        let mut affected = Vec::with_capacity(room_ids.len());
        for rid in room_ids {
            let empty = self.leave(id, &rid);
            affected.push((rid, empty));
        }

        self.connections.remove(id);
        affected
    }

    /// Returns the room with the given name, if it exists.
    pub fn room(&self, room_id: &str) -> Option<&Room> {
        self.rooms.get(room_id)
    }

    /// Returns the sink of a specific participant (for unicast routing).
    pub fn sink(&self, id: &ParticipantId) -> Option<WsSink> {
        self.connections.get(id).map(|(tx, _)| tx.clone())
    }

    /// Returns the sinks of all other participants in a room.
    pub fn peers(&self, room_id: &str, exclude: &ParticipantId) -> Vec<WsSink> {
        self.rooms
            .get(room_id)
            .map(|r| r.peer_sinks(exclude))
            .unwrap_or_default()
    }
}

impl Default for SignalingState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Server ──────────────────────────────────────────────────────────────

/// In-memory WebSocket signaling server.
///
/// Routes [`SdpMessage`]s between participants across named rooms.
/// A single WebSocket connection can join multiple rooms.
pub struct RoomServer {
    listener: TcpListener,
    state: Arc<Mutex<SignalingState>>,
}

impl RoomServer {
    /// Binds the server to the given address (e.g. `"127.0.0.1:0"`).
    pub async fn bind(addr: &str) -> Result<Self, SeaMeetError> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            listener,
            state: Arc::new(Mutex::new(SignalingState::new())),
        })
    }

    /// Returns the local address the server is listening on.
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, SeaMeetError> {
        self.listener.local_addr().map_err(SeaMeetError::Io)
    }

    /// Starts accepting connections. Returns a [`JoinHandle`] for the server task.
    pub fn run(self) -> JoinHandle<()> {
        let state = self.state;
        let listener = self.listener;
        tokio::spawn(async move {
            loop {
                let (stream, addr) = match listener.accept().await {
                    Ok(v) => v,
                    Err(e) => {
                        error!("accept error: {e}");
                        continue;
                    }
                };
                debug!(%addr, "new TCP connection");
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let ws = match tokio_tungstenite::accept_async(stream).await {
                        Ok(ws) => ws,
                        Err(e) => {
                            warn!(%addr, "WebSocket handshake failed: {e}");
                            return;
                        }
                    };
                    Self::handle_connection(ws, state).await;
                });
            }
        })
    }

    /// Handles a single WebSocket connection lifecycle.
    async fn handle_connection(
        ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        state: Arc<Mutex<SignalingState>>,
    ) {
        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        let writer = tokio::spawn(async move {
            while let Some(text) = rx.recv().await {
                if sink.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
        });

        let mut participant_id: Option<ParticipantId> = None;

        while let Some(result) = stream.next().await {
            let msg = match result {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(_) => break,
            };

            let sdp: SdpMessage = match serde_json::from_str(&msg) {
                Ok(m) => m,
                Err(e) => {
                    warn!("invalid message: {e}");
                    continue;
                }
            };

            if participant_id.is_none() {
                if let SdpMessage::Join { participant, .. } = &sdp {
                    participant_id = Some(*participant);
                } else {
                    continue;
                }
            }

            let pid = participant_id.expect("set above");
            dispatch(&sdp, &msg, pid, &tx, &state).await;
        }

        // Connection closed — clean up all rooms.
        if let Some(pid) = participant_id {
            info!(participant = %pid, "disconnected");
            let affected = {
                let mut st = state.lock().await;
                st.disconnect(&pid)
            };

            for (room_id, _) in affected {
                let leave = SdpMessage::Leave {
                    participant: pid,
                    room_id: room_id.clone(),
                };
                if let Ok(json) = serde_json::to_string(&leave) {
                    let st = state.lock().await;
                    let peers = st.peers(&room_id, &pid);
                    drop(st);
                    for peer_tx in peers {
                        let _ = peer_tx.send(json.clone());
                    }
                }
            }
        }

        drop(tx);
        let _ = writer.await;
    }
}

/// Dispatches a single signaling message.
pub async fn dispatch(
    sdp: &SdpMessage,
    raw: &str,
    pid: ParticipantId,
    self_tx: &WsSink,
    state: &Arc<Mutex<SignalingState>>,
) {
    match sdp {
        SdpMessage::Join {
            participant,
            room_id,
        } => {
            let mut st = state.lock().await;
            let initiator = st.join(*participant, room_id, self_tx.clone());

            let ready = SdpMessage::Ready {
                room_id: room_id.clone(),
                initiator,
            };
            if let Ok(json) = serde_json::to_string(&ready) {
                let _ = self_tx.send(json);
            }

            if !initiator {
                let peers = st.peers(room_id, participant);
                drop(st);
                if let Ok(json) = serde_json::to_string(sdp) {
                    for peer_tx in peers {
                        let _ = peer_tx.send(json.clone());
                    }
                }
            }

            info!(participant = %participant, room = room_id, "joined");
        }
        SdpMessage::Leave {
            participant,
            room_id,
        } => {
            let mut st = state.lock().await;
            let peers = st.peers(room_id, participant);
            st.leave(participant, room_id);
            drop(st);

            if let Ok(json) = serde_json::to_string(sdp) {
                for peer_tx in peers {
                    let _ = peer_tx.send(json.clone());
                }
            }
            info!(participant = %participant, room = room_id, "left");
        }
        SdpMessage::Offer { room_id, to, .. } => {
            let st = state.lock().await;
            if let Some(target) = to {
                if let Some(peer_tx) = st.sink(target) {
                    let _ = peer_tx.send(raw.to_owned());
                }
            } else {
                let peers = st.peers(room_id, &pid);
                drop(st);
                for peer_tx in peers {
                    let _ = peer_tx.send(raw.to_owned());
                }
            }
        }
        SdpMessage::Answer { to, .. } | SdpMessage::IceCandidate { to, .. } => {
            let st = state.lock().await;
            if let Some(peer_tx) = st.sink(to) {
                let _ = peer_tx.send(raw.to_owned());
            }
        }
        SdpMessage::ScreenShareStarted { room_id, .. }
        | SdpMessage::ScreenShareStopped { room_id, .. } => {
            let st = state.lock().await;
            let peers = st.peers(room_id, &pid);
            drop(st);
            for peer_tx in peers {
                let _ = peer_tx.send(raw.to_owned());
            }
        }
        _ => {}
    }
}
