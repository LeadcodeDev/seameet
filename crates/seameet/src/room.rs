use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use seameet_codec::AudioPassthrough;
use seameet_core::events::RoomEvent;
use seameet_core::frame::{PcmFrame, VideoFrame};
use seameet_core::traits::Processor;
use seameet_core::{ParticipantId, SeaMeetError};
use seameet_pipeline::peer_connection::PeerConnection;
use seameet_pipeline::Peer;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::Stream;
use tracing::{debug, info};

/// Configuration for a [`Room`].
#[derive(Debug, Clone)]
pub struct RoomConfig {
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
            max_participants: usize::MAX,
            ice_servers: Vec::new(),
            audio_frame_ms: 20,
            video_fps: 30,
        }
    }
}

/// Handle returned when a participant is added to a [`Room`].
pub struct RoomHandle {
    /// The participant's identifier.
    pub participant_id: ParticipantId,
    /// Low-level access to the peer connection (escape hatch).
    pub peer_connection: Arc<Mutex<PeerConnection>>,
    /// Decoded audio frames received from this participant.
    pub audio_rx: broadcast::Receiver<PcmFrame>,
    /// Decoded video frames received from this participant.
    pub video_rx: broadcast::Receiver<VideoFrame>,
}

/// Internal state for a connected participant.
struct ParticipantState {
    _handle: JoinHandle<()>,
}

/// High-level room facade — manages participants, signaling, and media pipelines.
pub struct Room {
    config: RoomConfig,
    participants: Arc<Mutex<HashMap<ParticipantId, ParticipantState>>>,
    stop_tx: broadcast::Sender<()>,
    event_tx: mpsc::UnboundedSender<RoomEvent>,
    event_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<RoomEvent>>>>,
    created_at: Instant,
}

impl Room {
    /// Creates a new empty room with the given configuration.
    pub fn new(config: RoomConfig) -> Self {
        let (stop_tx, _) = broadcast::channel(4);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        info!("room created");
        Self {
            config,
            participants: Arc::new(Mutex::new(HashMap::new())),
            stop_tx,
            event_tx,
            event_rx: Arc::new(Mutex::new(Some(event_rx))),
            created_at: Instant::now(),
        }
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
        let mut participants = self.participants.lock().await;
        if participants.len() >= self.config.max_participants {
            return Err(SeaMeetError::RoomFull);
        }

        // Bind a UDP socket for this participant's peer connection.
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        let pc = PeerConnection::new(socket).await?;
        let pc = Arc::new(Mutex::new(pc));

        // Build the peer (inbound + outbound) with AudioPassthrough codec.
        let (peer, peer_events) = {
            let pc_lock = pc.lock().await;
            Peer::from_peer_connection(
                id,
                AudioPassthrough,
                AudioPassthrough,
                &pc_lock,
                Some(Box::new(processor)),
            )
        };

        let audio_rx = peer.audio_rx();
        let video_rx = peer.video_rx();

        // Emit ParticipantJoined event.
        let _ = self.event_tx.send(RoomEvent::ParticipantJoined(id));

        // Spawn the peer task.
        let stop = self.stop_tx.subscribe();
        let event_tx = self.event_tx.clone();
        let participants_ref = Arc::clone(&self.participants);

        let handle = tokio::spawn(async move {
            let result = peer.run_with_events(peer_events, stop).await;
            if let Err(e) = &result {
                debug!(participant = %id, "peer error: {e}");
            }
            // Emit ParticipantLeft.
            let _ = event_tx.send(RoomEvent::ParticipantLeft(id));

            // Remove from participants map.
            let mut parts = participants_ref.lock().await;
            parts.remove(&id);
            if parts.is_empty() {
                let _ = event_tx.send(RoomEvent::RoomEmpty);
            }
        });

        participants.insert(id, ParticipantState { _handle: handle });

        info!(participant = %id, count = participants.len(), "participant joined");

        Ok(RoomHandle {
            participant_id: id,
            peer_connection: pc,
            audio_rx,
            video_rx,
        })
    }

    /// Returns a stream of room events (participant joins, leaves, speech, etc.).
    ///
    /// Can only be called once — subsequent calls return an empty stream.
    pub fn events(&self) -> impl Stream<Item = RoomEvent> {
        // Try to take the receiver; if already taken, return an empty stream.
        let rx = self.event_rx.try_lock().ok().and_then(|mut guard| guard.take());
        match rx {
            Some(rx) => UnboundedReceiverStream::new(rx),
            None => UnboundedReceiverStream::new(mpsc::unbounded_channel().1),
        }
    }

    /// Returns the number of currently connected participants.
    pub async fn participant_count(&self) -> usize {
        self.participants.lock().await.len()
    }

    /// Closes the room, stopping all participant tasks.
    pub async fn close(&self) -> Result<(), SeaMeetError> {
        info!(
            duration = ?self.created_at.elapsed(),
            "closing room"
        );
        let _ = self.stop_tx.send(());

        // Collect participant IDs before emitting RoomEnded.
        let ids: Vec<ParticipantId> = {
            let parts = self.participants.lock().await;
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
