use seameet_core::frame::{EncodedAudio, PcmFrame, VideoFrame};
use seameet_core::traits::Decoder;
use seameet_core::SeaMeetError;
use seameet_rtp::{JitterBuffer, RtpPacket, PT_AUDIO};
use tokio::sync::broadcast;
use tracing::debug;

/// Default jitter buffer capacity in packets.
const JITTER_CAPACITY: usize = 128;

/// Maximum acceptable lateness in milliseconds.
const MAX_LATE_MS: u32 = 150;

/// Interval for logging jitter statistics (in poll cycles).
const STATS_LOG_INTERVAL: u64 = 500;

/// Events emitted by the inbound pipeline.
#[derive(Debug, Clone)]
pub enum InboundEvent {
    /// A PLI (Picture Loss Indication) was received from the remote peer.
    PliReceived,
}

/// Receives RTP packets, reorders them through a jitter buffer, decodes
/// audio, and publishes decoded frames to broadcast channels.
pub struct InboundPipeline<D: Decoder<EncodedAudio, PcmFrame>> {
    decoder: D,
    jitter: JitterBuffer,
    audio_tx: broadcast::Sender<PcmFrame>,
    _video_tx: broadcast::Sender<VideoFrame>,
    event_tx: broadcast::Sender<InboundEvent>,
    poll_count: u64,
}

impl<D: Decoder<EncodedAudio, PcmFrame>> InboundPipeline<D> {
    /// Creates a new inbound pipeline.
    pub fn new(
        decoder: D,
        audio_tx: broadcast::Sender<PcmFrame>,
        video_tx: broadcast::Sender<VideoFrame>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(16);
        Self {
            decoder,
            jitter: JitterBuffer::new(JITTER_CAPACITY, MAX_LATE_MS),
            audio_tx,
            _video_tx: video_tx,
            event_tx,
            poll_count: 0,
        }
    }

    /// Returns a receiver for inbound events (e.g. PLI notifications).
    pub fn event_rx(&self) -> broadcast::Receiver<InboundEvent> {
        self.event_tx.subscribe()
    }

    /// Pushes an incoming RTP packet into the jitter buffer.
    pub fn push_rtp(&mut self, packet: RtpPacket) {
        self.jitter.push(packet);
    }

    /// Notifies the pipeline that a PLI was received.
    pub fn notify_pli(&self) {
        let _ = self.event_tx.send(InboundEvent::PliReceived);
    }

    /// Drains all ready packets from the jitter buffer, decodes audio,
    /// and publishes frames. Returns the number of frames emitted.
    pub fn drain(&mut self, now_ms: u64) -> Result<usize, SeaMeetError> {
        let mut count = 0;
        while let Some(pkt) = self.jitter.poll(now_ms) {
            if pkt.payload_type == PT_AUDIO {
                let encoded = EncodedAudio(pkt.payload);
                let frame = self.decoder.decode(encoded)?;
                let _ = self.audio_tx.send(frame);
                count += 1;
            }
            // Video frames would be reassembled from marker-delimited
            // fragments here. For now, non-audio packets are ignored by
            // the audio decoder path.
        }

        self.poll_count += 1;
        if self.poll_count % STATS_LOG_INTERVAL == 0 {
            let stats = self.jitter.stats();
            debug!(
                received = stats.received,
                lost = stats.lost,
                reordered = stats.reordered,
                late = stats.late,
                "jitter buffer stats"
            );
        }

        Ok(count)
    }

    /// Runs the pipeline, polling the jitter buffer on a timer until stopped.
    pub async fn run(
        mut self,
        mut stop: broadcast::Receiver<()>,
    ) -> Result<(), SeaMeetError> {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(10));
        let start = tokio::time::Instant::now();
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let now_ms = start.elapsed().as_millis() as u64;
                    self.drain(now_ms)?;
                }
                _ = stop.recv() => {
                    debug!("inbound pipeline stopped");
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seameet_codec::AudioPassthrough;
    use seameet_core::traits::Encoder;
    use seameet_rtp::RtpPacket;

    #[test]
    fn test_inbound_reorders_packets() {
        let (audio_tx, mut audio_rx) = broadcast::channel(16);
        let (video_tx, _) = broadcast::channel(16);
        let mut pipeline = InboundPipeline::new(AudioPassthrough, audio_tx, video_tx);

        // Create 5 audio frames and their RTP packets.
        let mut encoder = AudioPassthrough;
        let mut packets = Vec::new();
        for seq in 0..5u16 {
            let frame = PcmFrame {
                samples: vec![seq as f32; 10],
                sample_rate: 48000,
                channels: 1,
            };
            let encoded = encoder.encode(frame).expect("encode");
            packets.push(RtpPacket::new_audio(
                encoded.0,
                seq as u32 * 960,
                1,
                seq,
            ));
        }

        // Insert in shuffled order: 3, 0, 4, 1, 2
        let order = [3, 0, 4, 1, 2];
        for &i in &order {
            pipeline.push_rtp(packets[i as usize].clone());
        }

        // Drain — should decode in sequence order 0..5.
        pipeline.drain(5000).expect("drain");

        let mut received = Vec::new();
        while let Ok(frame) = audio_rx.try_recv() {
            received.push(frame.samples[0] as u16);
        }

        assert_eq!(received, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_inbound_pli_event() {
        let (audio_tx, _) = broadcast::channel(4);
        let (video_tx, _) = broadcast::channel(4);
        let pipeline = InboundPipeline::new(AudioPassthrough, audio_tx, video_tx);
        let mut event_rx = pipeline.event_rx();
        pipeline.notify_pli();
        let evt = event_rx.try_recv().expect("event");
        assert!(matches!(evt, InboundEvent::PliReceived));
    }
}
