use bytes::Bytes;
use seameet_core::events::RoomEvent;
use seameet_core::frame::{EncodedAudio, EncodedVideo, PcmFrame};
use seameet_core::traits::{Encoder, ProcessorHint};
use seameet_core::SeaMeetError;
use seameet_rtp::RtpSender;
use tokio::sync::{broadcast, mpsc};
use tracing::debug;

use crate::processor::ProcessorChain;

/// Default video clock rate (90 kHz as per RTP spec).
const VIDEO_CLOCK_RATE: u32 = 90_000;

/// Default video frame rate.
const DEFAULT_FPS: u32 = 30;

/// Encodes audio frames and sends them as RTP packets.
pub struct OutboundPipeline<E: Encoder<PcmFrame, EncodedAudio>> {
    encoder: E,
    sender: RtpSender,
    processor_chain: ProcessorChain,
    event_tx: mpsc::UnboundedSender<RoomEvent>,
    event_rx: Option<mpsc::UnboundedReceiver<RoomEvent>>,
    /// Current audio RTP timestamp.
    audio_ts: u32,
    /// Audio timestamp increment per frame.
    audio_ts_step: u32,
    /// Current video RTP timestamp.
    video_ts: u32,
    /// Video timestamp increment per frame.
    video_ts_step: u32,
}

impl<E: Encoder<PcmFrame, EncodedAudio>> OutboundPipeline<E> {
    /// Creates a new outbound pipeline.
    ///
    /// * `encoder` — audio encoder (e.g. `AudioPassthrough`, `OpusEncoder`).
    /// * `sender` — RTP sender for packetization and transmission.
    pub fn new(encoder: E, sender: RtpSender) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let audio_ts: u32 = rand::random();
        let video_ts: u32 = rand::random();
        Self {
            encoder,
            sender,
            processor_chain: ProcessorChain::new(),
            event_tx,
            event_rx: Some(event_rx),
            audio_ts,
            audio_ts_step: 960, // 48000 / 1000 * 20 = 960 (20ms at 48kHz)
            video_ts,
            video_ts_step: VIDEO_CLOCK_RATE / DEFAULT_FPS,
        }
    }

    /// Sets the audio timestamp step based on sample rate and frame duration.
    pub fn set_audio_timing(&mut self, sample_rate: u32, frame_ms: u32) {
        self.audio_ts_step = sample_rate / 1000 * frame_ms;
    }

    /// Sets the video frame rate for timestamp computation.
    pub fn set_video_fps(&mut self, fps: u32) {
        if fps > 0 {
            self.video_ts_step = VIDEO_CLOCK_RATE / fps;
        }
    }

    /// Takes the receiver for room events emitted by the processor chain.
    ///
    /// Can only be called once; subsequent calls return `None`.
    pub fn take_event_rx(&mut self) -> Option<mpsc::UnboundedReceiver<RoomEvent>> {
        self.event_rx.take()
    }

    /// Appends a processor to the chain.
    pub fn add_processor(&mut self, processor: Box<dyn seameet_core::traits::Processor>) {
        self.processor_chain.push(processor);
    }

    /// Returns a reference to the underlying RTP sender.
    pub fn rtp_sender(&self) -> &RtpSender {
        &self.sender
    }

    /// Encodes and sends a single audio frame.
    pub async fn send_audio(&mut self, mut frame: PcmFrame) -> Result<(), SeaMeetError> {
        match self.processor_chain.process_audio(&mut frame) {
            ProcessorHint::Forward => {}
            ProcessorHint::Drop => return Ok(()),
            ProcessorHint::Emit(event) => {
                let _ = self.event_tx.send(event);
                return Ok(());
            }
        }

        let encoded = self.encoder.encode(frame)?;
        let ts = self.audio_ts;
        self.audio_ts = self.audio_ts.wrapping_add(self.audio_ts_step);
        self.sender.send_audio(encoded.0, ts).await
    }

    /// Sends an encoded video frame (already encoded externally).
    pub async fn send_video(&mut self, frame: EncodedVideo) -> Result<(), SeaMeetError> {
        let ts = self.video_ts;
        self.video_ts = self.video_ts.wrapping_add(self.video_ts_step);
        self.sender.send_video(frame, ts).await
    }

    /// Runs the pipeline, consuming audio frames from a broadcast receiver
    /// until stopped.
    pub async fn run(
        mut self,
        mut audio_rx: broadcast::Receiver<PcmFrame>,
        mut stop: broadcast::Receiver<()>,
    ) -> Result<(), SeaMeetError> {
        loop {
            tokio::select! {
                result = audio_rx.recv() => {
                    match result {
                        Ok(frame) => self.send_audio(frame).await?,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            debug!(skipped = n, "outbound audio lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            debug!("outbound audio channel closed");
                            return Ok(());
                        }
                    }
                }
                _ = stop.recv() => {
                    debug!("outbound pipeline stopped");
                    return Ok(());
                }
            }
        }
    }
}

/// Returns the outbound RTP channel receiver for inspecting sent packets.
///
/// Helper for creating an `RtpSender` + channel pair in tests.
pub fn rtp_channel(ssrc: u32) -> (RtpSender, mpsc::Receiver<Bytes>) {
    let (tx, rx) = mpsc::channel(256);
    (RtpSender::new(ssrc, tx), rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use seameet_codec::AudioPassthrough;
    use seameet_rtp::RtpPacket;
    use std::time::Duration;

    #[tokio::test]
    async fn test_outbound_rtp_timestamp_increments() {
        let (sender, mut rx) = rtp_channel(42);
        let mut pipeline = OutboundPipeline::new(AudioPassthrough, sender);
        // Force deterministic initial timestamp.
        pipeline.audio_ts = 0;
        pipeline.audio_ts_step = 960;

        for _ in 0..3 {
            let frame = PcmFrame {
                samples: vec![0.0; 960],
                sample_rate: 48000,
                channels: 1,
            };
            pipeline.send_audio(frame).await.expect("send");
        }

        let mut timestamps = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            let pkt = RtpPacket::from_bytes(&bytes).expect("decode");
            timestamps.push(pkt.timestamp);
        }

        assert_eq!(timestamps, vec![0, 960, 1920]);
    }

    #[tokio::test]
    async fn test_outbound_video_fragmentation() {
        let (sender, mut rx) = rtp_channel(99);
        let mut pipeline = OutboundPipeline::new(AudioPassthrough, sender);

        let frame = EncodedVideo {
            data: vec![0xBB; 3000],
            is_keyframe: true,
            pts: Duration::ZERO,
        };
        pipeline.send_video(frame).await.expect("send");

        let mut packets = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            let pkt = RtpPacket::from_bytes(&bytes).expect("decode");
            packets.push(pkt);
        }

        // 3000 / 1200 = 2.5 → 3 packets.
        assert_eq!(packets.len(), 3);
        // Last must have marker.
        assert!(packets.last().expect("last").marker);
        for pkt in &packets[..packets.len() - 1] {
            assert!(!pkt.marker);
        }
        let total: usize = packets.iter().map(|p| p.payload.len()).sum();
        assert_eq!(total, 3000);
    }

    #[tokio::test]
    async fn test_outbound_processor_drop() {
        use seameet_core::frame::VideoFrame;
        use seameet_core::traits::Processor;

        struct DropAll;
        impl Processor for DropAll {
            fn process_audio(&mut self, _: &mut PcmFrame) -> ProcessorHint {
                ProcessorHint::Drop
            }
            fn process_video(&mut self, _: &mut VideoFrame) -> ProcessorHint {
                ProcessorHint::Drop
            }
        }

        let (sender, mut rx) = rtp_channel(1);
        let mut pipeline = OutboundPipeline::new(AudioPassthrough, sender);
        pipeline.add_processor(Box::new(DropAll));

        let frame = PcmFrame {
            samples: vec![1.0; 10],
            sample_rate: 48000,
            channels: 1,
        };
        pipeline.send_audio(frame).await.expect("send");

        // No packets should have been sent.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_outbound_processor_emit() {
        use seameet_core::events::RoomEvent;
        use seameet_core::frame::VideoFrame;
        use seameet_core::traits::Processor;
        use seameet_core::ParticipantId;
        use std::any::Any;

        let pid = ParticipantId::random();

        struct EmitProc(ParticipantId);
        impl Processor for EmitProc {
            fn process_audio(&mut self, _: &mut PcmFrame) -> ProcessorHint {
                ProcessorHint::Emit(RoomEvent::Custom {
                    participant: self.0,
                    payload: Box::new(99u32) as Box<dyn Any + Send>,
                })
            }
            fn process_video(&mut self, _: &mut VideoFrame) -> ProcessorHint {
                ProcessorHint::Forward
            }
        }

        let (sender, mut rtp_rx) = rtp_channel(1);
        let mut pipeline = OutboundPipeline::new(AudioPassthrough, sender);
        pipeline.add_processor(Box::new(EmitProc(pid)));
        let mut event_rx = pipeline.take_event_rx().expect("event_rx");

        let frame = PcmFrame {
            samples: vec![0.0; 10],
            sample_rate: 48000,
            channels: 1,
        };
        pipeline.send_audio(frame).await.expect("send");

        // No RTP packet sent (frame was consumed by Emit).
        assert!(rtp_rx.try_recv().is_err());

        // Event should have been emitted.
        let evt = event_rx.try_recv().expect("event");
        match evt {
            RoomEvent::Custom { participant, .. } => assert_eq!(participant, pid),
            other => panic!("expected Custom, got {other:?}"),
        }
    }
}
