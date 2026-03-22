use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use seameet_core::events::RoomEvent;
use seameet_core::frame::{PcmFrame, VideoFrame};
use seameet_core::traits::Processor;
use seameet_core::track::TrackId;
use seameet_core::{ParticipantId, SeaMeetError};
use seameet_pipeline::peer_connection::{PeerCmd, PeerConnection, PeerEvent};
use seameet_pipeline::{MediaKind, Mid};
use seameet_rtp::PT_AUDIO;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::Stream;
use tracing::{debug, info, warn};
use uuid::Uuid;

// ── Forwarding types ────────────────────────────────────────────────────

/// RTP data broadcast across the room for forwarding to other participants.
#[derive(Clone)]
#[allow(dead_code)]
struct ForwardedRtp {
    source: ParticipantId,
    /// Original payload type from the source peer.
    pt: u8,
    /// Extended sequence number from str0m.
    seq_no: u64,
    timestamp: u32,
    marker: bool,
    payload: Vec<u8>,
    is_audio: bool,
}

/// Tracks the outbound m-line mapping for a single source peer.
///
/// Each participant maintains one `SourceSlot` per remote peer whose media
/// it is forwarding. The slot records which m-lines (audio + video) are
/// assigned to that source peer, along with monotonically increasing
/// sequence numbers.
struct SourceSlot {
    audio_mid: Mid,
    video_mid: Mid,
    audio_seq: u64,
    video_seq: u64,
    audio_pt: u8,
    video_pt: u8,
}

/// Finds or lazily creates a [`SourceSlot`] for `source_pid`.
///
/// When a participant first receives media from a new source peer, we
/// allocate free audio + video m-lines from `all_mids`, excluding the
/// participant's own receive mids and mids already assigned to other
/// source peers.
fn get_or_create_slot<'a>(
    source_pid: ParticipantId,
    source_slots: &'a mut HashMap<ParticipantId, SourceSlot>,
    all_mids: &[(Mid, MediaKind)],
    own_audio_mid: Option<Mid>,
    own_video_mid: Option<Mid>,
    audio_pt: u8,
    video_pt: u8,
) -> Option<&'a mut SourceSlot> {
    if source_slots.contains_key(&source_pid) {
        return source_slots.get_mut(&source_pid);
    }

    let used_mids: HashSet<Mid> = source_slots
        .values()
        .flat_map(|s| [s.audio_mid, s.video_mid])
        .collect();

    let free_audio = all_mids.iter().find(|(mid, kind)| {
        matches!(kind, MediaKind::Audio)
            && Some(*mid) != own_audio_mid
            && !used_mids.contains(mid)
    });
    let free_video = all_mids.iter().find(|(mid, kind)| {
        matches!(kind, MediaKind::Video)
            && Some(*mid) != own_video_mid
            && !used_mids.contains(mid)
    });

    let (Some((a_mid, _)), Some((v_mid, _))) = (free_audio, free_video) else {
        warn!(source = %source_pid, "no free mids for source slot");
        return None;
    };
    let a_mid = *a_mid;
    let v_mid = *v_mid;

    info!(
        source = %source_pid,
        ?a_mid, ?v_mid,
        "created source slot"
    );

    source_slots.insert(
        source_pid,
        SourceSlot {
            audio_mid: a_mid,
            video_mid: v_mid,
            audio_seq: 0,
            video_seq: 0,
            audio_pt,
            video_pt,
        },
    );
    source_slots.get_mut(&source_pid)
}

// ── Room configuration ──────────────────────────────────────────────────

/// Configuration for a [`Room`].
#[derive(Debug, Clone)]
pub struct RoomConfig {
    /// Optional human-readable name. Defaults to `None`.
    pub name: Option<String>,
    /// Maximum number of simultaneous participants. Defaults to `usize::MAX`.
    pub max_participants: usize,
    /// STUN/TURN server URLs for ICE.
    pub ice_servers: Vec<String>,
    /// Audio frame duration in milliseconds. Defaults to 20.
    pub audio_frame_ms: u32,
    /// Video frame rate. Defaults to 30.
    pub video_fps: u32,
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            name: None,
            max_participants: usize::MAX,
            ice_servers: Vec::new(),
            audio_frame_ms: 20,
            video_fps: 30,
        }
    }
}

// ── RoomHandle ──────────────────────────────────────────────────────────

/// Handle returned when a participant is added to a [`Room`].
///
/// Provides methods to perform signaling operations (accept SDP offers,
/// add ICE candidates) and subscribe to decoded audio/video streams.
///
/// Cloning a handle shares the same underlying command channel and broadcast
/// senders — each clone can independently subscribe to audio/video streams.
#[derive(Clone)]
pub struct RoomHandle {
    /// The participant's identifier.
    pub participant_id: ParticipantId,
    /// Command channel to the running PeerConnection.
    cmd_tx: mpsc::UnboundedSender<PeerCmd>,
    /// Audio broadcast sender — call [`audio_rx`](Self::audio_rx) to subscribe.
    audio_tx: broadcast::Sender<PcmFrame>,
    /// Video broadcast sender — call [`video_rx`](Self::video_rx) to subscribe.
    video_tx: broadcast::Sender<VideoFrame>,
}

impl std::fmt::Debug for RoomHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomHandle")
            .field("participant_id", &self.participant_id)
            .finish_non_exhaustive()
    }
}

impl RoomHandle {
    /// Returns a new receiver for decoded audio frames from this participant.
    pub fn audio_rx(&self) -> broadcast::Receiver<PcmFrame> {
        self.audio_tx.subscribe()
    }

    /// Returns a new receiver for decoded video frames from this participant.
    pub fn video_rx(&self) -> broadcast::Receiver<VideoFrame> {
        self.video_tx.subscribe()
    }

    /// Accepts a remote SDP offer and returns the local SDP answer.
    ///
    /// This sends the offer to the running PeerConnection via the command
    /// channel and awaits the answer.
    pub async fn accept_offer(&self, sdp: &str) -> Result<String, SeaMeetError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(PeerCmd::AcceptOffer {
                sdp: sdp.to_owned(),
                reply: reply_tx,
            })
            .map_err(|_| SeaMeetError::PeerConnection("peer connection closed".into()))?;
        reply_rx
            .await
            .map_err(|_| SeaMeetError::PeerConnection("peer connection dropped reply".into()))?
    }

    /// Adds a remote ICE candidate to the running PeerConnection.
    pub fn add_ice_candidate(&self, candidate: &str) -> Result<(), SeaMeetError> {
        self.cmd_tx
            .send(PeerCmd::AddIceCandidate(candidate.to_owned()))
            .map_err(|_| SeaMeetError::PeerConnection("peer connection closed".into()))
    }
}

// ── ParticipantEntry ────────────────────────────────────────────────────

/// Internal state for a connected participant.
struct ParticipantEntry {
    handle: RoomHandle,
    stop_tx: broadcast::Sender<()>,
    _pc_task: JoinHandle<()>,
    _fwd_task: JoinHandle<()>,
    /// Active screen share track IDs for this participant.
    screen_tracks: HashSet<TrackId>,
}

// ── Room ────────────────────────────────────────────────────────────────

/// High-level room — manages participants, signaling, and media pipelines.
///
/// Each room has a unique [`Uuid`] identifier and an optional human-readable
/// name. Participants are stored in a `HashMap` protected by an `RwLock` so
/// that reads (`has_participant`, `get_participant`, `fetch_participants`) do
/// not block each other.
///
/// When participants are added, the room spawns tasks that drive each
/// participant's PeerConnection and forward RTP between all peers (SFU
/// pattern). Audio is decoded via the inbound pipeline and published on
/// the audio broadcast channel. Video is forwarded as raw RTP packets.
pub struct Room {
    /// Unique room identifier, generated at creation.
    id: Uuid,
    /// Optional human-readable name.
    name: Option<String>,
    config: RoomConfig,
    participants: Arc<RwLock<HashMap<ParticipantId, ParticipantEntry>>>,
    global_stop_tx: broadcast::Sender<()>,
    event_tx: mpsc::UnboundedSender<RoomEvent>,
    event_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<RoomEvent>>>,
    /// Room-wide RTP forwarding broadcast.
    rtp_broadcast_tx: broadcast::Sender<ForwardedRtp>,
    created_at: Instant,
}

impl Room {
    /// Creates a new empty room with the given configuration.
    pub fn new(config: RoomConfig) -> Self {
        let (global_stop_tx, _) = broadcast::channel(4);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (rtp_broadcast_tx, _) = broadcast::channel(256);
        let id = Uuid::new_v4();
        let name = config.name.clone();
        info!(
            room_id = %id,
            room_name = name.as_deref().unwrap_or("<unnamed>"),
            "room created"
        );
        Self {
            id,
            name,
            config,
            participants: Arc::new(RwLock::new(HashMap::new())),
            global_stop_tx,
            event_tx,
            event_rx: std::sync::Mutex::new(Some(event_rx)),
            rtp_broadcast_tx,
            created_at: Instant::now(),
        }
    }

    /// Returns the unique room identifier.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Returns the room name, if set.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Adds a participant to the room.
    ///
    /// Creates a PeerConnection, spawns a driving task (handles UDP +
    /// commands), and a forwarding task (broadcasts received RTP to other
    /// participants and writes incoming forwarded RTP to this participant's
    /// PeerConnection via SourceSlots).
    ///
    /// * `id` — unique participant identifier.
    /// * `_signaling` — the signaling backend (reserved for future SDP
    ///   renegotiation; the peer connection is created internally).
    /// * `_processor` — a media processor applied to outbound audio/video
    ///   (reserved for future outbound pipeline integration).
    pub async fn add_participant<S, P>(
        &self,
        id: ParticipantId,
        _signaling: S,
        _processor: P,
    ) -> Result<RoomHandle, SeaMeetError>
    where
        S: seameet_signaling::SignalingBackend + Send + 'static,
        P: Processor + Send + 'static,
    {
        // Check capacity under a brief read lock.
        {
            let participants = self
                .participants
                .read()
                .map_err(|_| SeaMeetError::PeerConnection("lock poisoned".into()))?;
            if participants.len() >= self.config.max_participants {
                return Err(SeaMeetError::RoomFull);
            }
        }

        // Bind a UDP socket for this participant's peer connection.
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        let pc = PeerConnection::new(socket).await?;

        // Subscribe to PeerConnection events before run() consumes it.
        let peer_events = pc.events();

        // Create command channel.
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        // Broadcast channels for decoded media from this participant.
        let (audio_tx, _) = broadcast::channel(64);
        let (video_tx, _) = broadcast::channel(16);

        let room_handle = RoomHandle {
            participant_id: id,
            cmd_tx: cmd_tx.clone(),
            audio_tx: audio_tx.clone(),
            video_tx: video_tx.clone(),
        };

        // Per-participant stop signal.
        let (stop_tx, stop_rx) = broadcast::channel(1);

        // Also subscribe to the global stop.
        let global_stop_rx = self.global_stop_tx.subscribe();

        // Emit ParticipantJoined event.
        let _ = self.event_tx.send(RoomEvent::ParticipantJoined(id));

        // ── Spawn PeerConnection driving task ──
        let merged_stop_pc = merge_stop(stop_tx.subscribe(), self.global_stop_tx.subscribe());
        let pc_task = tokio::spawn(async move {
            let result = pc.run_with_commands(cmd_rx, merged_stop_pc).await;
            if let Err(e) = &result {
                debug!(participant = %id, "peer connection error: {e}");
            }
        });

        // ── Spawn forwarding task ──
        let rtp_broadcast_tx = self.rtp_broadcast_tx.clone();
        let rtp_broadcast_rx = self.rtp_broadcast_tx.subscribe();
        let fwd_cmd_tx = cmd_tx.clone();
        let event_tx = self.event_tx.clone();
        let participants_ref = Arc::clone(&self.participants);
        let merged_stop_fwd = merge_stop(stop_rx, global_stop_rx);

        let fwd_task = tokio::spawn(async move {
            run_forwarding_task(
                id,
                peer_events,
                rtp_broadcast_tx,
                rtp_broadcast_rx,
                fwd_cmd_tx,
                audio_tx,
                merged_stop_fwd,
            )
            .await;

            // Emit ParticipantLeft.
            let _ = event_tx.send(RoomEvent::ParticipantLeft(id));

            // Remove from participants map.
            if let Ok(mut parts) = participants_ref.write() {
                parts.remove(&id);
                if parts.is_empty() {
                    let _ = event_tx.send(RoomEvent::RoomEmpty);
                }
            }
        });

        // Insert under a brief write lock.
        {
            let mut participants = self
                .participants
                .write()
                .map_err(|_| SeaMeetError::PeerConnection("lock poisoned".into()))?;
            participants.insert(
                id,
                ParticipantEntry {
                    handle: room_handle.clone(),
                    stop_tx,
                    _pc_task: pc_task,
                    _fwd_task: fwd_task,
                    screen_tracks: HashSet::new(),
                },
            );
            info!(
                room_id = %self.id,
                participant = %id,
                count = participants.len(),
                "participant joined"
            );
        }

        Ok(room_handle)
    }

    /// Returns `true` if a participant with this identifier is currently in the room.
    pub fn has_participant(&self, id: &ParticipantId) -> bool {
        let participants = self.participants.read().expect("lock not poisoned");
        participants.contains_key(id)
    }

    /// Returns the handle of a participant by their identifier.
    ///
    /// Returns `Err(SeaMeetError::ParticipantNotFound(id))` if the participant
    /// is not in the room.
    pub fn get_participant(&self, id: &ParticipantId) -> Result<RoomHandle, SeaMeetError> {
        let participants = self.participants.read().expect("lock not poisoned");
        participants
            .get(id)
            .map(|e| e.handle.clone())
            .ok_or(SeaMeetError::ParticipantNotFound(*id))
    }

    /// Returns the handles of all currently connected participants.
    /// The order is not guaranteed.
    pub fn fetch_participants(&self) -> Vec<RoomHandle> {
        let participants = self.participants.read().expect("lock not poisoned");
        participants.values().map(|e| e.handle.clone()).collect()
    }

    /// Removes a participant from the room.
    ///
    /// Sends the stop signal to the participant's task, emits
    /// `RoomEvent::ParticipantLeft`, and `RoomEvent::RoomEmpty` if this was
    /// the last participant. The lock is released immediately — the method
    /// does **not** wait for the task to terminate.
    ///
    /// This is a silent no-op if the participant is not in the room.
    pub async fn remove_participant(&self, id: &ParticipantId) -> Result<(), SeaMeetError> {
        let entry = {
            let mut participants = self
                .participants
                .write()
                .map_err(|_| SeaMeetError::PeerConnection("lock poisoned".into()))?;
            participants.remove(id)
        };
        // Lock is released here.

        if let Some(entry) = entry {
            // Send stop signal (non-blocking).
            let _ = entry.stop_tx.send(());

            // Emit events.
            let _ = self.event_tx.send(RoomEvent::ParticipantLeft(*id));

            let is_empty = {
                let participants = self.participants.read().expect("lock not poisoned");
                participants.is_empty()
            };
            if is_empty {
                let _ = self.event_tx.send(RoomEvent::RoomEmpty);
            }

            debug!(room_id = %self.id, participant = %id, "participant removed");
        }

        Ok(())
    }

    /// Returns a stream of room events (participant joins, leaves, speech, etc.).
    ///
    /// Can only be called once — subsequent calls return an empty stream.
    pub fn events(&self) -> impl Stream<Item = RoomEvent> {
        let rx = self.event_rx.lock().ok().and_then(|mut guard| guard.take());
        match rx {
            Some(rx) => UnboundedReceiverStream::new(rx),
            None => UnboundedReceiverStream::new(mpsc::unbounded_channel().1),
        }
    }

    /// Returns the number of currently connected participants.
    pub fn participant_count(&self) -> usize {
        self.participants.read().expect("lock not poisoned").len()
    }

    /// Closes the room, stopping all participant tasks.
    pub async fn close(&self) -> Result<(), SeaMeetError> {
        info!(
            room_id = %self.id,
            duration = ?self.created_at.elapsed(),
            "closing room"
        );
        let _ = self.global_stop_tx.send(());

        // Collect participant IDs before emitting RoomEnded.
        let ids: Vec<ParticipantId> = {
            let parts = self.participants.read().expect("lock not poisoned");
            parts.keys().copied().collect()
        };

        let _ = self.event_tx.send(RoomEvent::RoomEnded {
            participants: ids,
            duration: self.created_at.elapsed(),
        });

        // Give tasks a moment to finish.
        tokio::task::yield_now().await;

        Ok(())
    }

    /// Returns all active screen share tracks in the room, across all participants.
    pub fn active_screen_shares(&self) -> Vec<(ParticipantId, TrackId)> {
        let participants = self.participants.read().expect("lock not poisoned");
        participants
            .iter()
            .flat_map(|(pid, entry)| {
                entry.screen_tracks.iter().map(move |tid| (*pid, *tid))
            })
            .collect()
    }

    /// Registers a screen share for a participant (called from signaling handler).
    pub fn notify_screen_share_started(
        &self,
        participant: &ParticipantId,
        track_id: TrackId,
    ) {
        if let Ok(mut participants) = self.participants.write() {
            if let Some(entry) = participants.get_mut(participant) {
                entry.screen_tracks.insert(track_id);
            }
        }
        let _ = self.event_tx.send(RoomEvent::ScreenShareStarted {
            participant: *participant,
            track_id,
        });
    }

    /// Unregisters a screen share for a participant.
    pub fn notify_screen_share_stopped(
        &self,
        participant: &ParticipantId,
        track_id: TrackId,
    ) {
        if let Ok(mut participants) = self.participants.write() {
            if let Some(entry) = participants.get_mut(participant) {
                entry.screen_tracks.remove(&track_id);
            }
        }
        let _ = self.event_tx.send(RoomEvent::ScreenShareStopped {
            participant: *participant,
            track_id,
        });
    }
}

// ── Forwarding task ─────────────────────────────────────────────────────

/// Runs the per-participant forwarding task.
///
/// Responsibilities:
/// 1. Receives `PeerEvent`s from the PeerConnection (RTP received, media
///    added, connected, etc.)
/// 2. Decodes audio via the inbound pipeline and publishes `PcmFrame`s
/// 3. Broadcasts all received RTP as `ForwardedRtp` to the room
/// 4. Subscribes to `ForwardedRtp` from other participants and forwards
///    to this participant's PeerConnection via `PeerCmd::WriteRtp`
/// 5. Manages `SourceSlot`s for m-line mapping
async fn run_forwarding_task(
    id: ParticipantId,
    mut peer_events: broadcast::Receiver<PeerEvent>,
    rtp_broadcast_tx: broadcast::Sender<ForwardedRtp>,
    mut rtp_broadcast_rx: broadcast::Receiver<ForwardedRtp>,
    cmd_tx: mpsc::UnboundedSender<PeerCmd>,
    audio_tx: broadcast::Sender<PcmFrame>,
    mut stop: broadcast::Receiver<()>,
) {
    debug!(participant = %id, "forwarding task started");

    // Track media lines discovered during SDP negotiation.
    let mut all_mids: Vec<(Mid, MediaKind)> = Vec::new();
    let mut own_audio_mid: Option<Mid> = None;
    let mut own_video_mid: Option<Mid> = None;
    let mut source_slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();

    // Inbound audio pipeline (passthrough decode for now).
    let mut audio_decoder = seameet_codec::AudioPassthrough;

    loop {
        tokio::select! {
            result = peer_events.recv() => {
                match result {
                    Ok(PeerEvent::RtpReceived { packet, seq_no }) => {
                        let is_audio = packet.payload_type == PT_AUDIO;

                        // Decode audio and publish to the audio broadcast channel.
                        if is_audio {
                            use seameet_core::traits::Decoder;
                            use seameet_core::frame::EncodedAudio;
                            let encoded = EncodedAudio(packet.payload.clone());
                            if let Ok(frame) = audio_decoder.decode(encoded) {
                                let _ = audio_tx.send(frame);
                            }
                        }

                        // Broadcast to other participants for forwarding.
                        let _ = rtp_broadcast_tx.send(ForwardedRtp {
                            source: id,
                            pt: packet.payload_type,
                            seq_no,
                            timestamp: packet.timestamp,
                            marker: packet.marker,
                            payload: packet.payload,
                            is_audio,
                        });
                    }
                    Ok(PeerEvent::MediaAdded { mid, kind }) => {
                        if !all_mids.iter().any(|(m, _)| *m == mid) {
                            all_mids.push((mid, kind));
                        }
                        // First audio/video mids are our own receive mids.
                        match kind {
                            MediaKind::Audio if own_audio_mid.is_none() => {
                                own_audio_mid = Some(mid);
                                debug!(participant = %id, ?mid, "own audio mid");
                            }
                            MediaKind::Video if own_video_mid.is_none() => {
                                own_video_mid = Some(mid);
                                debug!(participant = %id, ?mid, "own video mid");
                            }
                            _ => {}
                        }
                    }
                    Ok(PeerEvent::Connected) => {
                        debug!(participant = %id, "peer connected");
                        // Request initial keyframes from own video stream.
                        if let Some(mid) = own_video_mid {
                            let _ = cmd_tx.send(PeerCmd::RequestKeyframe { mid });
                        }
                    }
                    Ok(PeerEvent::KeyframeRequest) => {
                        // Remote wants a keyframe — request PLI from own video.
                        if let Some(mid) = own_video_mid {
                            let _ = cmd_tx.send(PeerCmd::RequestKeyframe { mid });
                        }
                    }
                    Ok(PeerEvent::Disconnected) => {
                        debug!(participant = %id, "peer disconnected");
                        return;
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!(participant = %id, skipped = n, "peer events lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        debug!(participant = %id, "peer events closed");
                        return;
                    }
                }
            }
            result = rtp_broadcast_rx.recv() => {
                match result {
                    Ok(fwd) if fwd.source != id => {
                        // Forward RTP from another participant to this one.
                        let created_new = !source_slots.contains_key(&fwd.source);

                        if let Some(slot) = get_or_create_slot(
                            fwd.source,
                            &mut source_slots,
                            &all_mids,
                            own_audio_mid,
                            own_video_mid,
                            PT_AUDIO,
                            seameet_rtp::PT_VIDEO,
                        ) {
                            let (mid, seq, pt) = if fwd.is_audio {
                                let seq = slot.audio_seq;
                                slot.audio_seq += 1;
                                (slot.audio_mid, seq, slot.audio_pt)
                            } else {
                                let seq = slot.video_seq;
                                slot.video_seq += 1;
                                (slot.video_mid, seq, slot.video_pt)
                            };

                            let _ = cmd_tx.send(PeerCmd::WriteRtp {
                                mid,
                                pt,
                                seq,
                                timestamp: fwd.timestamp,
                                marker: fwd.marker,
                                payload: fwd.payload,
                            });
                        }

                        // Request keyframe when we first create a slot for a new source.
                        if created_new && source_slots.contains_key(&fwd.source) {
                            if let Some(mid) = own_video_mid {
                                let _ = cmd_tx.send(PeerCmd::RequestKeyframe { mid });
                            }
                        }
                    }
                    Ok(_) => {} // Own packets — skip.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!(participant = %id, skipped = n, "rtp broadcast lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return;
                    }
                }
            }
            _ = stop.recv() => {
                debug!(participant = %id, "forwarding task stopped");
                return;
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Merges two broadcast stop receivers into one: returns a receiver that
/// fires when either signal arrives.
fn merge_stop(
    mut a: broadcast::Receiver<()>,
    mut b: broadcast::Receiver<()>,
) -> broadcast::Receiver<()> {
    let (tx, rx) = broadcast::channel(1);
    tokio::spawn(async move {
        tokio::select! {
            _ = a.recv() => {}
            _ = b.recv() => {}
        }
        let _ = tx.send(());
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use seameet_core::Passthrough;
    use std::time::Duration;
    use tokio_stream::StreamExt;

    /// A dummy signaling backend for tests.
    struct DummySignaling;

    impl seameet_signaling::SignalingBackend for DummySignaling {
        async fn send(&self, _msg: seameet_signaling::SdpMessage) -> Result<(), SeaMeetError> {
            Ok(())
        }

        async fn recv(&mut self) -> Result<seameet_signaling::SdpMessage, SeaMeetError> {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Err(SeaMeetError::Signaling("dummy closed".into()))
        }

        async fn close(&self) -> Result<(), SeaMeetError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_room_has_id_and_name() {
        let room = Room::new(RoomConfig {
            name: Some("test-room".into()),
            ..Default::default()
        });
        assert!(!room.id().is_nil());
        assert_eq!(room.name(), Some("test-room"));
    }

    #[tokio::test]
    async fn test_room_default_name_is_none() {
        let room = Room::new(RoomConfig::default());
        assert!(room.name().is_none());
    }

    #[tokio::test]
    async fn test_has_participant_true() {
        let room = Room::new(RoomConfig::default());
        let id = ParticipantId::random();
        let _h = room
            .add_participant(id, DummySignaling, Passthrough)
            .await
            .expect("add");
        assert!(room.has_participant(&id));
    }

    #[tokio::test]
    async fn test_has_participant_false() {
        let room = Room::new(RoomConfig::default());
        let id = ParticipantId::random();
        assert!(!room.has_participant(&id));
    }

    #[tokio::test]
    async fn test_get_participant_ok() {
        let room = Room::new(RoomConfig::default());
        let id = ParticipantId::random();
        let _h = room
            .add_participant(id, DummySignaling, Passthrough)
            .await
            .expect("add");
        let handle = room.get_participant(&id).expect("get");
        assert_eq!(handle.participant_id, id);
    }

    #[tokio::test]
    async fn test_get_participant_not_found() {
        let room = Room::new(RoomConfig::default());
        let id = ParticipantId::random();
        let result = room.get_participant(&id);
        assert!(result.is_err());
        match result.unwrap_err() {
            SeaMeetError::ParticipantNotFound(pid) => assert_eq!(pid, id),
            other => panic!("expected ParticipantNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_fetch_participants_empty() {
        let room = Room::new(RoomConfig::default());
        assert!(room.fetch_participants().is_empty());
    }

    #[tokio::test]
    async fn test_fetch_participants_count() {
        let room = Room::new(RoomConfig::default());
        for _ in 0..3 {
            let id = ParticipantId::random();
            let _ = room
                .add_participant(id, DummySignaling, Passthrough)
                .await
                .expect("add");
        }
        assert_eq!(room.fetch_participants().len(), 3);
    }

    #[tokio::test]
    async fn test_remove_participant_emits_left() {
        let room = Room::new(RoomConfig::default());
        let mut events = room.events();
        let id = ParticipantId::random();
        let _ = room
            .add_participant(id, DummySignaling, Passthrough)
            .await
            .expect("add");

        // Consume ParticipantJoined.
        let _ = tokio::time::timeout(Duration::from_secs(1), events.next()).await;

        room.remove_participant(&id).await.expect("remove");

        let evt = tokio::time::timeout(Duration::from_secs(2), events.next())
            .await
            .expect("timeout")
            .expect("event");

        match evt {
            RoomEvent::ParticipantLeft(pid) => assert_eq!(pid, id),
            other => panic!("expected ParticipantLeft, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_remove_participant_emits_room_empty() {
        let room = Room::new(RoomConfig::default());
        let mut events = room.events();
        let id = ParticipantId::random();
        let _ = room
            .add_participant(id, DummySignaling, Passthrough)
            .await
            .expect("add");

        // Consume ParticipantJoined.
        let _ = tokio::time::timeout(Duration::from_secs(1), events.next()).await;

        room.remove_participant(&id).await.expect("remove");

        // Collect events — should see ParticipantLeft then RoomEmpty.
        let mut saw_empty = false;
        for _ in 0..5 {
            match tokio::time::timeout(Duration::from_millis(500), events.next()).await {
                Ok(Some(RoomEvent::RoomEmpty)) => {
                    saw_empty = true;
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(saw_empty, "expected RoomEmpty after removing last participant");
    }

    #[tokio::test]
    async fn test_remove_participant_noop() {
        let room = Room::new(RoomConfig::default());
        let id = ParticipantId::random();
        let result = room.remove_participant(&id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_remove_participant_releases_lock() {
        let room = Arc::new(Room::new(RoomConfig::default()));
        let id = ParticipantId::random();
        let _ = room
            .add_participant(id, DummySignaling, Passthrough)
            .await
            .expect("add");

        let room2 = Arc::clone(&room);

        let reader = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            room2.has_participant(&id)
        });

        room.remove_participant(&id).await.expect("remove");

        let result = tokio::time::timeout(Duration::from_secs(1), reader)
            .await
            .expect("should not deadlock")
            .expect("task should not panic");

        let _ = result;
    }

    #[tokio::test]
    async fn test_room_active_screen_shares_empty() {
        let room = Room::new(RoomConfig::default());
        assert!(room.active_screen_shares().is_empty());
    }

    #[tokio::test]
    async fn test_room_active_screen_shares_count() {
        let room = Room::new(RoomConfig::default());
        let id1 = ParticipantId::random();
        let id2 = ParticipantId::random();
        let _ = room.add_participant(id1, DummySignaling, Passthrough).await.expect("add1");
        let _ = room.add_participant(id2, DummySignaling, Passthrough).await.expect("add2");

        room.notify_screen_share_started(&id1, TrackId(10));
        room.notify_screen_share_started(&id2, TrackId(20));

        let shares = room.active_screen_shares();
        assert_eq!(shares.len(), 2);
    }

    #[tokio::test]
    async fn test_room_screen_share_events_sequence() {
        let room = Room::new(RoomConfig::default());
        let mut events = room.events();
        let id = ParticipantId::random();
        let _ = room.add_participant(id, DummySignaling, Passthrough).await.expect("add");

        // Consume ParticipantJoined.
        let _ = tokio::time::timeout(Duration::from_secs(1), events.next()).await;

        room.notify_screen_share_started(&id, TrackId(42));
        room.notify_screen_share_stopped(&id, TrackId(42));

        let evt1 = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await.expect("t1").expect("e1");
        assert!(matches!(evt1, RoomEvent::ScreenShareStarted { track_id, .. } if track_id == TrackId(42)));

        let evt2 = tokio::time::timeout(Duration::from_secs(1), events.next())
            .await.expect("t2").expect("e2");
        assert!(matches!(evt2, RoomEvent::ScreenShareStopped { track_id, .. } if track_id == TrackId(42)));
    }
}
