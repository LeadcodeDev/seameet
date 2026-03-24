use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use seameet_core::ParticipantId;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::message::SdpMessage;
use crate::transport::IncomingConnection;

/// Per-participant outbound channel.
pub type WsSink = mpsc::UnboundedSender<String>;

// ── Domain types ────────────────────────────────────────────────────────

/// A connected participant within the signaling system.
#[derive(Debug, Clone)]
pub struct Member {
    /// The participant's unique identifier.
    pub id: ParticipantId,
    /// Outbound channel to send messages to this member.
    sink: WsSink,
    /// Optional human-readable name.
    pub display_name: Option<String>,
}

impl Member {
    /// Creates a new member.
    pub fn new(id: ParticipantId, sink: WsSink) -> Self {
        Self {
            id,
            sink,
            display_name: None,
        }
    }

    /// Creates a new member with a display name.
    pub fn with_display_name(id: ParticipantId, sink: WsSink, display_name: Option<String>) -> Self {
        Self {
            id,
            sink,
            display_name,
        }
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

    /// Removes members whose WebSocket channel is closed (receiver dropped).
    /// Returns the IDs of pruned members.
    pub fn prune_stale(&mut self) -> Vec<ParticipantId> {
        let stale: Vec<ParticipantId> = self
            .members
            .values()
            .filter(|m| m.sink.is_closed())
            .map(|m| m.id)
            .collect();
        for id in &stale {
            self.members.remove(id);
        }
        stale
    }

    /// Returns the IDs of all members except `exclude`.
    pub fn peer_ids(&self, exclude: &ParticipantId) -> Vec<ParticipantId> {
        self.members
            .keys()
            .filter(|id| *id != exclude)
            .copied()
            .collect()
    }

    /// Returns the sinks of all members except `exclude`.
    pub fn peer_sinks(&self, exclude: &ParticipantId) -> Vec<WsSink> {
        self.members
            .values()
            .filter(|m| m.id != *exclude)
            .map(|m| m.sink.clone())
            .collect()
    }

    /// Returns display names of all members except `exclude`, keyed by participant ID string.
    pub fn display_names(&self, exclude: &ParticipantId) -> HashMap<String, String> {
        self.members
            .values()
            .filter(|m| m.id != *exclude)
            .filter_map(|m| {
                m.display_name
                    .as_ref()
                    .map(|name| (m.id.to_string(), name.clone()))
            })
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
    /// Per-participant connection generation set by `run_connection`.
    /// Each WebSocket connection gets a unique generation from a static
    /// atomic counter; a stale disconnect can detect that a newer
    /// connection replaced it and skip cleanup.
    connection_gens: HashMap<ParticipantId, u64>,
}

impl SignalingState {
    /// Creates empty state.
    pub fn new() -> Self {
        Self {
            rooms: HashMap::new(),
            connections: HashMap::new(),
            connection_gens: HashMap::new(),
        }
    }

    /// Registers a participant in a room with an optional display name.
    /// Returns `true` if this is the first participant (initiator).
    pub fn join(
        &mut self,
        id: ParticipantId,
        room_id: &str,
        sink: WsSink,
        display_name: Option<String>,
    ) -> bool {
        let room = self.rooms.entry(room_id.to_owned()).or_default();
        let is_first = room.join(Member::with_display_name(id, sink.clone(), display_name));

        let conn = self
            .connections
            .entry(id)
            .or_insert_with(|| (sink.clone(), Vec::new()));
        // Always update the sink to the latest connection so that a stale
        // disconnect cannot remove the new member from rooms.
        conn.0 = sink;
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

    /// Removes a participant from ALL rooms (called on disconnect).
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
        self.connection_gens.remove(id);
        affected
    }

    /// Stores the connection generation for a participant.
    pub fn set_connection_gen(&mut self, id: ParticipantId, gen: u64) {
        self.connection_gens.insert(id, gen);
    }

    /// Returns the current connection generation for a participant.
    pub fn connection_gen(&self, id: &ParticipantId) -> Option<u64> {
        self.connection_gens.get(id).copied()
    }

    /// Returns the room with the given name, if it exists.
    pub fn room(&self, room_id: &str) -> Option<&Room> {
        self.rooms.get(room_id)
    }

    /// Returns a mutable reference to the room, if it exists.
    pub fn room_mut(&mut self, room_id: &str) -> Option<&mut Room> {
        self.rooms.get_mut(room_id)
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

// ── Hooks ───────────────────────────────────────────────────────────────

/// Extension hooks for the signaling engine.
///
/// Implementors can intercept messages before the default dispatch logic
/// runs, and react to participant disconnections. This is the primary
/// extension point for SFU-style servers that need to handle Offer/Answer
/// themselves while letting the engine manage room membership.
pub trait SignalingHooks: Send + Sync + 'static {
    /// Called before the default dispatch for each incoming message.
    ///
    /// Return `true` to suppress the default dispatch (the hook handled it).
    /// Return `false` to let the engine dispatch normally.
    fn on_message(
        &self,
        sdp: &SdpMessage,
        raw: &str,
        pid: ParticipantId,
        self_tx: &mpsc::UnboundedSender<String>,
        state: &Arc<RwLock<SignalingState>>,
    ) -> impl Future<Output = bool> + Send;

    /// Called when a participant disconnects from all rooms.
    fn on_disconnect(
        &self,
        pid: ParticipantId,
        affected_rooms: &[(String, bool)],
        state: &Arc<RwLock<SignalingState>>,
    ) -> impl Future<Output = ()> + Send;
}

/// No-op hooks for a plain signaling relay server.
pub struct NoopHooks;

impl SignalingHooks for NoopHooks {
    async fn on_message(
        &self,
        _sdp: &SdpMessage,
        _raw: &str,
        _pid: ParticipantId,
        _self_tx: &mpsc::UnboundedSender<String>,
        _state: &Arc<RwLock<SignalingState>>,
    ) -> bool {
        false
    }

    async fn on_disconnect(
        &self,
        _pid: ParticipantId,
        _affected_rooms: &[(String, bool)],
        _state: &Arc<RwLock<SignalingState>>,
    ) {
    }
}

// ── Dispatch ────────────────────────────────────────────────────────────

/// Dispatches a single signaling message through the engine's default logic.
pub async fn dispatch(
    sdp: &SdpMessage,
    raw: &str,
    pid: ParticipantId,
    self_tx: &WsSink,
    state: &Arc<RwLock<SignalingState>>,
) {
    match sdp {
        SdpMessage::Join {
            participant,
            room_id,
            display_name,
        } => {
            let mut st = state.write().await;

            // Prune zombie members whose WS connection closed but whose
            // disconnect handler hasn't run yet (race on tab refresh).
            let pruned_pids: Vec<ParticipantId> = if let Some(room) = st.room_mut(room_id) {
                let pruned = room.prune_stale();
                for stale_pid in &pruned {
                    info!(participant = %stale_pid, room = room_id, "pruned stale member on join");
                }
                pruned
            } else {
                vec![]
            };

            // Broadcast PeerLeft for pruned zombies BEFORE PeerJoined, so that
            // existing peers free their transceiver slots before allocating new ones.
            if !pruned_pids.is_empty() {
                let active_sinks = st.room(room_id)
                    .map(|r| r.peer_sinks(participant))
                    .unwrap_or_default();
                for stale_pid in &pruned_pids {
                    let peer_left = SdpMessage::PeerLeft {
                        participant: *stale_pid,
                        room_id: room_id.clone(),
                    };
                    if let Ok(json) = serde_json::to_string(&peer_left) {
                        for sink in &active_sinks {
                            let _ = sink.send(json.clone());
                        }
                    }
                }
            }

            // If this participant is already in the room (rapid reconnect),
            // broadcast PeerLeft so other peers clean up the stale entry.
            if let Some(room) = st.room(room_id) {
                if room.get(participant).is_some() {
                    tracing::warn!(%participant, %room_id, "re-join detected, broadcasting PeerLeft for stale entry");
                    let peer_left = SdpMessage::PeerLeft {
                        participant: *participant,
                        room_id: room_id.clone(),
                    };
                    if let Ok(json) = serde_json::to_string(&peer_left) {
                        room.broadcast(&json, participant);
                    }
                }
            }

            // Collect existing peer IDs and display names *before* joining.
            let existing_peers: Vec<ParticipantId> = st
                .room(room_id)
                .map(|r| r.peer_ids(participant))
                .unwrap_or_default();
            let existing_display_names: HashMap<String, String> = st
                .room(room_id)
                .map(|r| r.display_names(participant))
                .unwrap_or_default();

            let initiator = st.join(*participant, room_id, self_tx.clone(), display_name.clone());

            // Send Ready with the list of existing peers and their display names.
            let ready = SdpMessage::Ready {
                room_id: room_id.clone(),
                initiator,
                peers: existing_peers,
                display_names: existing_display_names,
            };
            if let Ok(json) = serde_json::to_string(&ready) {
                let _ = self_tx.send(json);
            }

            // Notify existing peers about the new joiner.
            if !initiator {
                let peer_sinks = st.peers(room_id, participant);
                drop(st);
                let peer_joined = SdpMessage::PeerJoined {
                    participant: *participant,
                    room_id: room_id.clone(),
                    display_name: display_name.clone(),
                };
                if let Ok(json) = serde_json::to_string(&peer_joined) {
                    for peer_tx in peer_sinks {
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
            let mut st = state.write().await;
            let peers = st.peers(room_id, participant);
            st.leave(participant, room_id);
            drop(st);

            let peer_left = SdpMessage::PeerLeft {
                participant: *participant,
                room_id: room_id.clone(),
            };
            if let Ok(json) = serde_json::to_string(&peer_left) {
                for peer_tx in peers {
                    let _ = peer_tx.send(json.clone());
                }
            }
            info!(participant = %participant, room = room_id, "left");
        }
        SdpMessage::Offer { room_id, to, .. } => {
            let st = state.read().await;
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
            let st = state.read().await;
            if let Some(peer_tx) = st.sink(to) {
                let _ = peer_tx.send(raw.to_owned());
            }
        }
        // RequestRenegotiation is server→client only; ignore if received from client.
        SdpMessage::RequestRenegotiation { .. } => {}
        SdpMessage::ScreenShareStarted { room_id, .. }
        | SdpMessage::ScreenShareStopped { room_id, .. }
        | SdpMessage::MuteAudio { room_id, .. }
        | SdpMessage::UnmuteAudio { room_id, .. }
        | SdpMessage::MuteVideo { room_id, .. }
        | SdpMessage::UnmuteVideo { room_id, .. }
        | SdpMessage::VideoConfigChanged { room_id, .. } => {
            let st = state.read().await;
            let peers = st.peers(room_id, &pid);
            drop(st);
            for peer_tx in peers {
                let _ = peer_tx.send(raw.to_owned());
            }
        }
        _ => {}
    }
}

// ── Connection lifecycle ────────────────────────────────────────────────

/// Runs the full lifecycle of a single connection through the signaling engine.
///
/// This is the core loop: read → parse → hooks → dispatch → cleanup.
/// It is transport-agnostic — the caller provides an [`IncomingConnection`]
/// obtained from any [`TransportListener`](crate::transport::TransportListener).
pub async fn run_connection<H: SignalingHooks>(
    conn: IncomingConnection,
    state: Arc<RwLock<SignalingState>>,
    hooks: Arc<H>,
) {
    let IncomingConnection {
        mut reader,
        writer: tx,
        writer_handle,
    } = conn;

    let mut participant_id: Option<ParticipantId> = None;

    // Each connection gets a unique generation from a process-wide counter.
    // When the same participant reconnects, the new connection overwrites
    // the generation; the old connection's cleanup detects this and skips.
    static NEXT_CONN_GEN: AtomicU64 = AtomicU64::new(1);
    let my_gen = NEXT_CONN_GEN.fetch_add(1, Ordering::Relaxed);

    while let Some(msg) = reader.recv().await {
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
                // Register our generation before hooks/dispatch process the Join.
                let mut st = state.write().await;
                st.set_connection_gen(*participant, my_gen);
                drop(st);
            } else {
                continue;
            }
        }

        let pid = participant_id.expect("set above");

        // Let hooks intercept the message first.
        let handled = hooks.on_message(&sdp, &msg, pid, &tx, &state).await;
        if !handled {
            dispatch(&sdp, &msg, pid, &tx, &state).await;
        }
    }

    // Abort the writer task so its channel receiver is dropped immediately.
    // This makes `sink.is_closed()` return true for any clone of our WsSink,
    // allowing `prune_stale` to detect the dead connection reliably.
    if let Some(handle) = writer_handle {
        handle.abort();
    }

    // Connection closed — clean up all rooms.
    if let Some(pid) = participant_id {
        info!(participant = %pid, "disconnected");

        // If a newer connection already replaced ours, skip cleanup entirely
        // to avoid destroying the new session's state.
        let is_stale = {
            let st = state.read().await;
            st.connection_gen(&pid) != Some(my_gen)
        };

        if is_stale {
            info!(participant = %pid, gen = my_gen, "skipping stale disconnect (peer reconnected)");
            return;
        }

        let affected = {
            let mut st = state.write().await;
            st.disconnect(&pid)
        };

        if affected.is_empty() {
            warn!(participant = %pid, "disconnect: no affected rooms");
        }

        // Broadcast PeerLeft to remaining peers.
        for (room_id, _) in &affected {
            let peer_left = SdpMessage::PeerLeft {
                participant: pid,
                room_id: room_id.clone(),
            };
            if let Ok(json) = serde_json::to_string(&peer_left) {
                let st = state.read().await;
                let peers = st.peers(room_id, &pid);
                drop(st);
                info!(
                    participant = %pid,
                    room = %room_id,
                    peer_count = peers.len(),
                    "broadcasting PeerLeft"
                );
                for peer_tx in peers {
                    let _ = peer_tx.send(json.clone());
                }
            }
        }

        // Notify hooks about the disconnect.
        hooks.on_disconnect(pid, &affected, &state).await;
    }
}
