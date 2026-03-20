use std::sync::Arc;
use std::time::Duration;

/// A raw PCM audio frame.
#[derive(Debug, Clone)]
pub struct PcmFrame {
    /// Interleaved audio samples.
    pub samples: Vec<f32>,
    /// Sample rate in Hz (e.g. 48000).
    pub sample_rate: u32,
    /// Number of audio channels.
    pub channels: u16,
}

/// A raw video frame.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Raw pixel data.
    pub data: Arc<Vec<u8>>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Presentation timestamp.
    pub pts: Duration,
}

/// An encoded audio payload.
#[derive(Debug, Clone)]
pub struct EncodedAudio(pub Vec<u8>);

/// An encoded video payload.
#[derive(Debug, Clone)]
pub struct EncodedVideo {
    /// Encoded bitstream data.
    pub data: Vec<u8>,
    /// Whether this frame is a keyframe (IDR).
    pub is_keyframe: bool,
    /// Presentation timestamp.
    pub pts: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_frame_clone() {
        let frame = PcmFrame {
            samples: vec![0.0, 0.5, -0.5],
            sample_rate: 48000,
            channels: 2,
        };
        let cloned = frame.clone();
        assert_eq!(cloned.samples, frame.samples);
        assert_eq!(cloned.sample_rate, 48000);
        assert_eq!(cloned.channels, 2);
    }

    #[test]
    fn video_frame_arc_shared() {
        let data = Arc::new(vec![0u8; 1920 * 1080 * 3]);
        let frame = VideoFrame {
            data: Arc::clone(&data),
            width: 1920,
            height: 1080,
            pts: Duration::from_millis(33),
        };
        let cloned = frame.clone();
        assert!(Arc::ptr_eq(&frame.data, &cloned.data));
    }

    #[test]
    fn encoded_audio_debug() {
        let ea = EncodedAudio(vec![1, 2, 3]);
        assert!(format!("{:?}", ea).contains("EncodedAudio"));
    }

    #[test]
    fn encoded_video_keyframe() {
        let ev = EncodedVideo {
            data: vec![0xAB],
            is_keyframe: true,
            pts: Duration::from_secs(1),
        };
        assert!(ev.is_keyframe);
        assert_eq!(ev.pts, Duration::from_secs(1));
    }
}
