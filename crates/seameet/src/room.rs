use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use seameet_codec::AudioPassthrough;
use seameet_core::events::RoomEvent;
use seameet_core::frame::{PcmFrame, VideoFrame};
use seameet_core::traits::Processor;
use seameet_core::{ParticipantId, SeaMeetError};
use seameet_pipeline::peer_connection::PeerConnection;
use seameet_pipeline::Peer;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, Mutex as TokioMutex};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::Stream;
use tracing::{debug, info};
use uuid::Uuid;

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

/// Handle returned when a participant is added to a [`Room`].
///
/// Cloning a handle shares the same underlying peer connection and broadcast
/// senders — each clone can independently subscribe to audio/video streams.
#[derive(Clone)]
pub struct RoomHandle {
    /// The participant's identifier.
    pub participant_id: ParticipantId,
    /// Low-level access to the peer connection (escape hatch).
    pub peer_connection: Arc<TokioMutex<PeerConnection>>,
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
}

/// Internal state for a connected participant.
struct ParticipantEntry {
    handle: RoomHandle,
    stop_tx: broadcast::Sender<()>,
    _task: JoinHandle<()>,
}

/// High-level room — manages participants, signaling, and media pipelines.
///
/// Each room has a unique [`Uuid`] identifier and an optional human-readable
/// name. Participants are stored in a `HashMap` protected by an `RwLock` so
/// that reads (`has_participant`, `get_participant`, `fetch_participants`) do
/// not block each other.
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
    created_at: Instant,
}

impl Room {
    /// Creates a new empty room with the given configuration.
    pub fn new(config: RoomConfig) -> Self {
        let (global_stop_tx, _) = broadcast::channel(4);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
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
    /// * `id` — unique participant identifier.
    /// * `signaling` — the signaling backend for SDP/ICE exchange (used for
    ///   future negotiation; the peer connection is created internally).
    /// * `processor` — a media processor applied to outbound audio/video.
    pub async fn add_participant<S, P>(
        &self,
        id: ParticipantId,
        _signaling: S,
        processor: P,
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
        let pc = Arc::new(TokioMutex::new(pc));

        // Build the peer (inbound + outbound) with AudioPassthrough codec.
        let (peer, peer_events, audio_tx, video_tx) = {
            let pc_lock = pc.lock().await;
            let (peer, peer_events) = Peer::from_peer_connection(
                id,
                AudioPassthrough,
                AudioPassthrough,
                &pc_lock,
                Some(Box::new(processor)),
            );
            let audio_tx = peer.audio_tx_clone();
            let video_tx = peer.video_tx_clone();
            (peer, peer_events, audio_tx, video_tx)
        };

        let room_handle = RoomHandle {
            participant_id: id,
            peer_connection: Arc::clone(&pc),
            audio_tx,
            video_tx,
        };

        // Per-participant stop signal.
        let (stop_tx, stop_rx) = broadcast::channel(1);

        // Also subscribe to the global stop.
        let global_stop_rx = self.global_stop_tx.subscribe();

        // Emit ParticipantJoined event.
        let _ = self.event_tx.send(RoomEvent::ParticipantJoined(id));

        // Spawn the peer task.
        let event_tx = self.event_tx.clone();
        let participants_ref = Arc::clone(&self.participants);

        let task = tokio::spawn(async move {
            // Merge per-participant and global stop signals.
            let merged_stop = merge_stop(stop_rx, global_stop_rx);
            let result = peer.run_with_events(peer_events, merged_stop).await;
            if let Err(e) = &result {
                debug!(participant = %id, "peer error: {e}");
            }
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
                    _task: task,
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
}

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
}
