use std::collections::{HashMap, HashSet};
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
type WsSink = mpsc::UnboundedSender<String>;

/// Shared signaling state with double index (rooms ↔ participants).
pub struct SignalingState {
    /// Room → list of participant IDs in that room.
    rooms: HashMap<String, Vec<ParticipantId>>,
    /// Participant → (outbound channel, set of rooms joined).
    participants: HashMap<ParticipantId, (WsSink, HashSet<String>)>,
}

impl SignalingState {
    /// Creates empty state.
    fn new() -> Self {
        Self {
            rooms: HashMap::new(),
            participants: HashMap::new(),
        }
    }

    /// Registers a participant in a room.
    /// Returns `true` if this is the first participant (initiator).
    pub fn join(&mut self, participant: ParticipantId, room_id: &str, sink: WsSink) -> bool {
        let room = self.rooms.entry(room_id.to_owned()).or_default();
        let is_first = room.is_empty();

        if !room.contains(&participant) {
            room.push(participant);
        }

        let entry = self
            .participants
            .entry(participant)
            .or_insert_with(|| (sink, HashSet::new()));
        entry.1.insert(room_id.to_owned());

        is_first
    }

    /// Removes a participant from a specific room.
    /// Returns `true` if the room is now empty.
    pub fn leave(&mut self, participant: &ParticipantId, room_id: &str) -> bool {
        let room_empty = if let Some(room) = self.rooms.get_mut(room_id) {
            room.retain(|id| id != participant);
            if room.is_empty() {
                self.rooms.remove(room_id);
                true
            } else {
                false
            }
        } else {
            true
        };

        if let Some((_, rooms)) = self.participants.get_mut(participant) {
            rooms.remove(room_id);
            if rooms.is_empty() {
                self.participants.remove(participant);
            }
        }

        room_empty
    }

    /// Removes a participant from ALL rooms (called on WebSocket disconnect).
    /// Returns the list of affected rooms with whether each is now empty.
    pub fn disconnect(&mut self, participant: &ParticipantId) -> Vec<(String, bool)> {
        let room_ids: Vec<String> = self
            .participants
            .get(participant)
            .map(|(_, rooms)| rooms.iter().cloned().collect())
            .unwrap_or_default();

        let mut affected = Vec::with_capacity(room_ids.len());
        for rid in room_ids {
            let empty = self.leave(participant, &rid);
            affected.push((rid, empty));
        }

        // Ensure participant is fully removed.
        self.participants.remove(participant);

        affected
    }

    /// Returns the sinks of all other participants in a room.
    pub fn peers(&self, room_id: &str, exclude: &ParticipantId) -> Vec<WsSink> {
        let Some(room) = self.rooms.get(room_id) else {
            return Vec::new();
        };
        room.iter()
            .filter(|id| *id != exclude)
            .filter_map(|id| self.participants.get(id).map(|(tx, _)| tx.clone()))
            .collect()
    }

    /// Returns the sink of a specific participant, if connected.
    fn sink(&self, participant: &ParticipantId) -> Option<WsSink> {
        self.participants.get(participant).map(|(tx, _)| tx.clone())
    }
}

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

        // Outbound channel for this connection.
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

            // First message must be a Join to identify the participant.
            if participant_id.is_none() {
                if let SdpMessage::Join { participant, .. } = &sdp {
                    participant_id = Some(*participant);
                } else {
                    continue;
                }
            }

            let pid = participant_id.expect("set above");

            match &sdp {
                SdpMessage::Join {
                    participant,
                    room_id,
                } => {
                    let mut st = state.lock().await;
                    let initiator = st.join(*participant, room_id, tx.clone());

                    // Send Ready to the joining peer.
                    let ready = SdpMessage::Ready {
                        room_id: room_id.clone(),
                        initiator,
                    };
                    if let Ok(json) = serde_json::to_string(&ready) {
                        let _ = tx.send(json);
                    }

                    // If not initiator, forward the Join to existing peers.
                    if !initiator {
                        let peers = st.peers(room_id, participant);
                        drop(st);
                        if let Ok(json) = serde_json::to_string(&sdp) {
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

                    if let Ok(json) = serde_json::to_string(&sdp) {
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
                            let _ = peer_tx.send(msg.clone());
                        }
                    } else {
                        let peers = st.peers(room_id, &pid);
                        drop(st);
                        for peer_tx in peers {
                            let _ = peer_tx.send(msg.clone());
                        }
                    }
                }
                SdpMessage::Answer { to, .. } => {
                    let st = state.lock().await;
                    if let Some(peer_tx) = st.sink(to) {
                        let _ = peer_tx.send(msg.clone());
                    }
                }
                SdpMessage::IceCandidate { to, .. } => {
                    let st = state.lock().await;
                    if let Some(peer_tx) = st.sink(to) {
                        let _ = peer_tx.send(msg.clone());
                    }
                }
                _ => {}
            }
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
