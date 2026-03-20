use crate::error::SeaMeetError;
use crate::events::RoomEvent;
use crate::frame::{PcmFrame, VideoFrame};

/// A source of raw audio samples.
pub trait AudioSource: Send {
    /// Reads PCM samples into `buf` and returns the number of samples written.
    fn read_samples(&mut self, buf: &mut [f32]) -> usize;

    /// Returns the sample rate in Hz.
    fn sample_rate(&self) -> u32;

    /// Returns the number of audio channels.
    fn channels(&self) -> u16;
}

/// A source of raw video frames.
pub trait VideoSource: Send {
    /// Reads the next video frame, if available.
    fn read_frame(&mut self) -> Option<VideoFrame>;
}

/// Hint returned by a [`Processor`] after processing a frame.
#[derive(Debug)]
pub enum ProcessorHint {
    /// The frame should be forwarded downstream as-is.
    Forward,
    /// The frame should be dropped.
    Drop,
    /// The frame should be dropped and a [`RoomEvent`] emitted instead.
    Emit(RoomEvent),
}

/// Processes audio and video frames in a media pipeline.
pub trait Processor: Send {
    /// Processes an audio frame in-place and returns a processing hint.
    fn process_audio(&mut self, frame: &mut PcmFrame) -> ProcessorHint;

    /// Processes a video frame in-place and returns a processing hint.
    fn process_video(&mut self, frame: &mut VideoFrame) -> ProcessorHint;
}

/// Encodes input `I` into output `O`.
pub trait Encoder<I, O>: Send {
    /// Encodes the given input.
    fn encode(&mut self, input: I) -> Result<O, SeaMeetError>;
}

/// Decodes input `I` into output `O`.
pub trait Decoder<I, O>: Send {
    /// Decodes the given input.
    fn decode(&mut self, input: I) -> Result<O, SeaMeetError>;
}

/// A no-op processor that always forwards frames unchanged.
#[derive(Debug, Clone, Default)]
pub struct Passthrough;

impl Processor for Passthrough {
    fn process_audio(&mut self, _frame: &mut PcmFrame) -> ProcessorHint {
        ProcessorHint::Forward
    }

    fn process_video(&mut self, _frame: &mut VideoFrame) -> ProcessorHint {
        ProcessorHint::Forward
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn passthrough_forwards_audio() {
        let mut proc = Passthrough;
        let mut frame = PcmFrame {
            samples: vec![0.1, 0.2],
            sample_rate: 48000,
            channels: 1,
        };
        assert!(matches!(proc.process_audio(&mut frame), ProcessorHint::Forward));
    }

    #[test]
    fn passthrough_forwards_video() {
        let mut proc = Passthrough;
        let mut frame = VideoFrame {
            data: Arc::new(vec![0u8; 100]),
            width: 640,
            height: 480,
            pts: Duration::ZERO,
        };
        assert!(matches!(proc.process_video(&mut frame), ProcessorHint::Forward));
    }

    struct DummyAudioSource;

    impl AudioSource for DummyAudioSource {
        fn read_samples(&mut self, buf: &mut [f32]) -> usize {
            let n = buf.len().min(10);
            for s in &mut buf[..n] {
                *s = 0.0;
            }
            n
        }

        fn sample_rate(&self) -> u32 {
            44100
        }

        fn channels(&self) -> u16 {
            2
        }
    }

    #[test]
    fn dummy_audio_source() {
        let mut src = DummyAudioSource;
        let mut buf = [0.0f32; 5];
        let n = src.read_samples(&mut buf);
        assert_eq!(n, 5);
        assert_eq!(src.sample_rate(), 44100);
        assert_eq!(src.channels(), 2);
    }

    struct DummyVideoSource {
        sent: bool,
    }

    impl VideoSource for DummyVideoSource {
        fn read_frame(&mut self) -> Option<VideoFrame> {
            if self.sent {
                return None;
            }
            self.sent = true;
            Some(VideoFrame {
                data: Arc::new(vec![0u8; 640 * 480 * 3]),
                width: 640,
                height: 480,
                pts: Duration::ZERO,
            })
        }
    }

    #[test]
    fn dummy_video_source() {
        let mut src = DummyVideoSource { sent: false };
        assert!(src.read_frame().is_some());
        assert!(src.read_frame().is_none());
    }
}
