use std::collections::HashMap;

use bytes::Bytes;
use seameet_core::events::RoomEvent;
use seameet_core::frame::{EncodedAudio, PcmFrame, VideoFrame};
use seameet_core::track::{TrackHandle, TrackId, VideoTrackKind};
use seameet_core::traits::{Decoder, Encoder, Processor};
use seameet_core::{ParticipantId, SeaMeetError};
use seameet_rtp::RtpSender;
use tokio::sync::{broadcast, mpsc};
use tracing::debug;

use crate::inbound::InboundPipeline;
use crate::outbound::OutboundPipeline;
use crate::peer_connection::{PeerConnection, PeerEvent};

/// Aggregates an inbound and outbound pipeline for a single participant.
pub struct Peer<
    D: Decoder<EncodedAudio, PcmFrame>,
    E: Encoder<PcmFrame, EncodedAudio>,
> {
    /// The participant this peer represents.
    pub id: ParticipantId,
    /// Inbound pipeline (receives and decodes RTP).
    pub inbound: InboundPipeline<D>,
    /// Outbound pipeline (encodes and sends RTP).
    pub outbound: OutboundPipeline<E>,
    /// Audio broadcast sender (owned so we can hand out receivers).
    audio_tx: broadcast::Sender<PcmFrame>,
    /// Camera video broadcast sender.
    video_tx: broadcast::Sender<VideoFrame>,
    /// Event bus for room events (screen share, etc.).
    event_tx: mpsc::UnboundedSender<RoomEvent>,
    /// Screen share tracks — one per active share.
    screen_tracks: HashMap<TrackId, TrackHandle>,
}

impl<D, E> Peer<D, E>
where
    D: Decoder<EncodedAudio, PcmFrame> + Send + 'static,
    E: Encoder<PcmFrame, EncodedAudio> + Send + 'static,
{
    /// Creates a new peer with the given pipelines.
    pub fn new(
        id: ParticipantId,
        decoder: D,
        outbound: OutboundPipeline<E>,
    ) -> Self {
        let (audio_tx, _) = broadcast::channel(64);
        let (video_tx, _) = broadcast::channel(16);
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let inbound = InboundPipeline::new(decoder, audio_tx.clone(), video_tx.clone());
        Self {
            id,
            inbound,
            outbound,
            audio_tx,
            video_tx,
            event_tx,
            screen_tracks: HashMap::new(),
        }
    }

    /// Creates a peer backed by a [`PeerConnection`], wiring RTP events
    /// from the connection into the inbound pipeline and outbound RTP
    /// through the connection's UDP socket.
    pub fn from_peer_connection(
        id: ParticipantId,
        decoder: D,
        encoder: E,
        pc: &PeerConnection,
        processor: Option<Box<dyn Processor>>,
    ) -> (Self, broadcast::Receiver<PeerEvent>) {
        let (rtp_tx, _rtp_rx) = mpsc::channel::<Bytes>(256);
        let sender = RtpSender::new(rand::random(), rtp_tx);
        let mut outbound = OutboundPipeline::new(encoder, sender);
        if let Some(p) = processor {
            outbound.add_processor(p);
        }

        let (audio_tx, _) = broadcast::channel(64);
        let (video_tx, _) = broadcast::channel(16);
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let inbound = InboundPipeline::new(decoder, audio_tx.clone(), video_tx.clone());
        let events = pc.events();

        let peer = Self {
            id,
            inbound,
            outbound,
            audio_tx,
            video_tx,
            event_tx,
            screen_tracks: HashMap::new(),
        };
        (peer, events)
    }

    /// Returns a receiver for decoded audio frames from this peer.
    pub fn audio_rx(&self) -> broadcast::Receiver<PcmFrame> {
        self.audio_tx.subscribe()
    }

    /// Returns a clone of the audio broadcast sender.
    pub fn audio_tx_clone(&self) -> broadcast::Sender<PcmFrame> {
        self.audio_tx.clone()
    }

    /// Returns a receiver for decoded camera video frames from this peer.
    pub fn camera_rx(&self) -> broadcast::Receiver<VideoFrame> {
        self.video_tx.subscribe()
    }

    /// Returns a receiver for decoded video frames (alias for camera_rx for backwards compat).
    pub fn video_rx(&self) -> broadcast::Receiver<VideoFrame> {
        self.video_tx.subscribe()
    }

    /// Returns a clone of the video broadcast sender.
    pub fn video_tx_clone(&self) -> broadcast::Sender<VideoFrame> {
        self.video_tx.clone()
    }

    /// Returns a receiver for a specific screen share track.
    /// Returns `None` if the track doesn't exist.
    pub fn screen_rx(&self, _track_id: TrackId) -> Option<broadcast::Receiver<VideoFrame>> {
        // Screen tracks don't have their own broadcast channels in this
        // simplified implementation — they share the video_tx.
        // A full implementation would use per-track channels.
        if self.screen_tracks.contains_key(&_track_id) {
            Some(self.video_tx.subscribe())
        } else {
            None
        }
    }

    /// Returns the currently active screen track IDs.
    pub fn active_screen_tracks(&self) -> Vec<TrackId> {
        self.screen_tracks.keys().copied().collect()
    }

    /// Called when a screen share starts for this participant.
    pub fn on_screen_share_started(&mut self, track_id: TrackId) -> Result<(), SeaMeetError> {
        let handle = TrackHandle::with_kind(track_id, VideoTrackKind::Screen);
        self.screen_tracks.insert(track_id, handle);
        let _ = self.event_tx.send(RoomEvent::ScreenShareStarted {
            participant: self.id,
            track_id,
        });
        debug!(participant = %self.id, ?track_id, "screen share started");
        Ok(())
    }

    /// Called when a screen share stops for this participant.
    pub fn on_screen_share_stopped(&mut self, track_id: TrackId) -> Result<(), SeaMeetError> {
        if let Some(handle) = self.screen_tracks.remove(&track_id) {
            handle.close();
        }
        let _ = self.event_tx.send(RoomEvent::ScreenShareStopped {
            participant: self.id,
            track_id,
        });
        debug!(participant = %self.id, ?track_id, "screen share stopped");
        Ok(())
    }

    /// Runs with a [`PeerConnection`] event stream, forwarding received RTP
    /// packets into the inbound pipeline.
    pub async fn run_with_events(
        mut self,
        mut peer_events: broadcast::Receiver<PeerEvent>,
        mut stop: broadcast::Receiver<()>,
    ) -> Result<(), SeaMeetError> {
        debug!(participant = %self.id, "peer started (with PeerConnection)");

        loop {
            tokio::select! {
                result = peer_events.recv() => {
                    match result {
                        Ok(PeerEvent::RtpReceived(pkt)) => {
                            self.inbound.push_rtp(pkt);
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            if let Err(e) = self.inbound.drain(now_ms) {
                                debug!("inbound drain error: {e}");
                            }
                        }
                        Ok(PeerEvent::Disconnected) => {
                            debug!(participant = %self.id, "peer disconnected");
                            return Ok(());
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            debug!(skipped = n, "peer event lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            return Ok(());
                        }
                    }
                }
                _ = stop.recv() => {
                    debug!("peer stopped by signal");
                    return Ok(());
                }
            }
        }
    }

    /// Runs both inbound and outbound pipelines until stopped.
    pub async fn run(self, mut stop: broadcast::Receiver<()>) -> Result<(), SeaMeetError> {
        debug!(participant = %self.id, "peer started");

        let (stop_tx, _) = broadcast::channel(1);
        let inbound_stop = stop_tx.subscribe();
        let outbound_stop = stop_tx.subscribe();
        let audio_rx = self.audio_tx.subscribe();

        let inbound = self.inbound;
        let outbound = self.outbound;

        let inbound_handle = tokio::spawn(async move {
            inbound.run(inbound_stop).await
        });

        let outbound_handle = tokio::spawn(async move {
            outbound.run(audio_rx, outbound_stop).await
        });

        let _ = stop.recv().await;
        let _ = stop_tx.send(());

        let _ = inbound_handle.await;
        let _ = outbound_handle.await;

        debug!("peer stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbound::rtp_channel;
    use seameet_codec::AudioPassthrough;
    use seameet_core::traits::Encoder;
    use seameet_rtp::RtpPacket;

    #[tokio::test]
    async fn test_peer_audio_roundtrip() {
        let id = ParticipantId::random();
        let (sender, _rtp_rx) = rtp_channel(1);
        let outbound = OutboundPipeline::new(AudioPassthrough, sender);
        let mut peer = Peer::new(id, AudioPassthrough, outbound);

        let mut audio_rx = peer.audio_rx();

        let original = PcmFrame {
            samples: vec![0.25, -0.5, 0.75],
            sample_rate: 48000,
            channels: 1,
        };
        let mut enc = AudioPassthrough;
        let encoded = enc.encode(original.clone()).expect("encode");
        let rtp = RtpPacket::new_audio(encoded.0, 0, 1, 0);
        peer.inbound.push_rtp(rtp);
        peer.inbound.drain(1000).expect("drain");

        let received = audio_rx.try_recv().expect("recv");
        assert_eq!(received.samples, original.samples);
        assert_eq!(received.sample_rate, 48000);
        assert_eq!(received.channels, 1);
    }

    #[test]
    fn test_peer_screen_share_started_emits_event() {
        let id = ParticipantId::random();
        let (sender, _rtp_rx) = rtp_channel(1);
        let outbound = OutboundPipeline::new(AudioPassthrough, sender);
        let mut peer = Peer::new(id, AudioPassthrough, outbound);

        // Take the event receiver before emitting.
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        peer.event_tx = event_tx;

        peer.on_screen_share_started(TrackId(42)).expect("start");

        let evt = event_rx.try_recv().expect("event");
        match evt {
            RoomEvent::ScreenShareStarted {
                participant,
                track_id,
            } => {
                assert_eq!(participant, id);
                assert_eq!(track_id, TrackId(42));
            }
            other => panic!("expected ScreenShareStarted, got {other:?}"),
        }
    }

    #[test]
    fn test_peer_screen_share_stopped_emits_event() {
        let id = ParticipantId::random();
        let (sender, _rtp_rx) = rtp_channel(1);
        let outbound = OutboundPipeline::new(AudioPassthrough, sender);
        let mut peer = Peer::new(id, AudioPassthrough, outbound);

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        peer.event_tx = event_tx;

        peer.on_screen_share_started(TrackId(10)).expect("start");
        let _ = event_rx.try_recv(); // consume Started

        peer.on_screen_share_stopped(TrackId(10)).expect("stop");
        let evt = event_rx.try_recv().expect("event");
        match evt {
            RoomEvent::ScreenShareStopped {
                participant,
                track_id,
            } => {
                assert_eq!(participant, id);
                assert_eq!(track_id, TrackId(10));
            }
            other => panic!("expected ScreenShareStopped, got {other:?}"),
        }
    }

    #[test]
    fn test_peer_multiple_screen_tracks() {
        let id = ParticipantId::random();
        let (sender, _rtp_rx) = rtp_channel(1);
        let outbound = OutboundPipeline::new(AudioPassthrough, sender);
        let mut peer = Peer::new(id, AudioPassthrough, outbound);

        peer.on_screen_share_started(TrackId(1)).expect("start 1");
        peer.on_screen_share_started(TrackId(2)).expect("start 2");

        assert_eq!(peer.screen_tracks.len(), 2);
        assert!(peer.screen_tracks.contains_key(&TrackId(1)));
        assert!(peer.screen_tracks.contains_key(&TrackId(2)));
    }

    #[test]
    fn test_peer_screen_rx_unknown_track() {
        let id = ParticipantId::random();
        let (sender, _rtp_rx) = rtp_channel(1);
        let outbound = OutboundPipeline::new(AudioPassthrough, sender);
        let peer = Peer::new(id, AudioPassthrough, outbound);

        assert!(peer.screen_rx(TrackId(999)).is_none());
    }
}
