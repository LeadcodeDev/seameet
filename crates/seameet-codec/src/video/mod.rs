use seameet_core::frame::{EncodedVideo, VideoFrame};
use seameet_core::traits::{Decoder, Encoder};
use seameet_core::SeaMeetError;
use std::sync::Arc;

/// Configuration for the VP8 encoder.
#[derive(Debug, Clone)]
pub struct Vp8EncoderConfig {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Target bitrate in kbps.
    pub bitrate_kbps: u32,
    /// Emit a keyframe every N frames.
    pub keyframe_interval: u32,
}

impl Default for Vp8EncoderConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 480,
            bitrate_kbps: 1000,
            keyframe_interval: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// Stub implementation (no vp8-ffi feature)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "vp8-ffi"))]
mod inner {
    use super::*;

    /// VP8 video encoder.
    ///
    /// Without the `vp8-ffi` feature this is a **stub** that produces a
    /// minimal 1×1 black keyframe. Enable `vp8-ffi` for real encoding.
    #[derive(Debug)]
    pub struct Vp8Encoder {
        config: Vp8EncoderConfig,
        frame_count: u32,
    }

    impl Vp8Encoder {
        /// Creates a new [`Vp8Encoder`] with the given configuration.
        pub fn new(config: Vp8EncoderConfig) -> Result<Self, SeaMeetError> {
            if config.width == 0 || config.height == 0 {
                return Err(SeaMeetError::Codec(
                    "VP8 width and height must be > 0".into(),
                ));
            }
            Ok(Self {
                config,
                frame_count: 0,
            })
        }

        /// Returns the encoder configuration.
        pub fn config(&self) -> &Vp8EncoderConfig {
            &self.config
        }
    }

    impl Encoder<VideoFrame, EncodedVideo> for Vp8Encoder {
        fn encode(&mut self, input: VideoFrame) -> Result<EncodedVideo, SeaMeetError> {
            tracing::warn!(
                "using VP8 stub encoder — enable feature `vp8-ffi` for real encoding"
            );
            let is_keyframe = self.frame_count % self.config.keyframe_interval == 0;
            self.frame_count += 1;
            // Stub: return a minimal payload.
            Ok(EncodedVideo {
                data: vec![0u8; 4],
                is_keyframe,
                pts: input.pts,
            })
        }
    }

    /// VP8 video decoder.
    ///
    /// Without the `vp8-ffi` feature this is a **stub** that produces a 1×1
    /// black frame. Enable `vp8-ffi` for real decoding.
    #[derive(Debug)]
    pub struct Vp8Decoder {
        config: Vp8EncoderConfig,
    }

    impl Vp8Decoder {
        /// Creates a new [`Vp8Decoder`] with the given configuration.
        pub fn new(config: Vp8EncoderConfig) -> Result<Self, SeaMeetError> {
            Ok(Self { config })
        }
    }

    impl Decoder<EncodedVideo, VideoFrame> for Vp8Decoder {
        fn decode(&mut self, input: EncodedVideo) -> Result<VideoFrame, SeaMeetError> {
            tracing::warn!(
                "using VP8 stub decoder — enable feature `vp8-ffi` for real decoding"
            );
            let size = (self.config.width * self.config.height * 3) as usize;
            Ok(VideoFrame {
                data: Arc::new(vec![0u8; size]),
                width: self.config.width,
                height: self.config.height,
                pts: input.pts,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Real FFI implementation (vp8-ffi feature)
// ---------------------------------------------------------------------------
#[cfg(feature = "vp8-ffi")]
mod inner {
    use super::*;

    /// VP8 video encoder backed by `libvpx` via the `vpx` crate.
    #[derive(Debug)]
    pub struct Vp8Encoder {
        config: Vp8EncoderConfig,
        // In a real implementation this would hold vpx::Encoder state.
    }

    impl Vp8Encoder {
        /// Creates a new [`Vp8Encoder`] with the given configuration.
        pub fn new(config: Vp8EncoderConfig) -> Result<Self, SeaMeetError> {
            if config.width == 0 || config.height == 0 {
                return Err(SeaMeetError::Codec(
                    "VP8 width and height must be > 0".into(),
                ));
            }
            Ok(Self { config })
        }

        /// Returns the encoder configuration.
        pub fn config(&self) -> &Vp8EncoderConfig {
            &self.config
        }
    }

    impl Encoder<VideoFrame, EncodedVideo> for Vp8Encoder {
        fn encode(&mut self, input: VideoFrame) -> Result<EncodedVideo, SeaMeetError> {
            // Real vpx encoding would go here.
            Err(SeaMeetError::Codec(
                "vp8-ffi encode not yet implemented".into(),
            ))
        }
    }

    /// VP8 video decoder backed by `libvpx` via the `vpx` crate.
    #[derive(Debug)]
    pub struct Vp8Decoder {
        config: Vp8EncoderConfig,
    }

    impl Vp8Decoder {
        /// Creates a new [`Vp8Decoder`] with the given configuration.
        pub fn new(config: Vp8EncoderConfig) -> Result<Self, SeaMeetError> {
            Ok(Self { config })
        }
    }

    impl Decoder<EncodedVideo, VideoFrame> for Vp8Decoder {
        fn decode(&mut self, input: EncodedVideo) -> Result<VideoFrame, SeaMeetError> {
            // Real vpx decoding would go here.
            Err(SeaMeetError::Codec(
                "vp8-ffi decode not yet implemented".into(),
            ))
        }
    }
}

pub use inner::{Vp8Decoder, Vp8Encoder};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_frame() -> VideoFrame {
        VideoFrame {
            data: Arc::new(vec![0u8; 640 * 480 * 3]),
            width: 640,
            height: 480,
            pts: Duration::from_millis(33),
        }
    }

    #[test]
    fn test_vp8_stub_encode_decode() {
        let config = Vp8EncoderConfig::default();
        let mut encoder = Vp8Encoder::new(config.clone()).expect("encoder");
        let encoded = encoder.encode(test_frame()).expect("encode");
        assert!(!encoded.data.is_empty());
        assert_eq!(encoded.pts, Duration::from_millis(33));

        let mut decoder = Vp8Decoder::new(config).expect("decoder");
        let decoded = decoder.decode(encoded).expect("decode");
        assert_eq!(decoded.width, 640);
        assert_eq!(decoded.height, 480);
    }

    #[test]
    fn test_vp8_keyframe_interval() {
        let config = Vp8EncoderConfig {
            keyframe_interval: 3,
            ..Default::default()
        };
        let mut encoder = Vp8Encoder::new(config).expect("encoder");

        let results: Vec<bool> = (0..6)
            .map(|_| {
                encoder
                    .encode(test_frame())
                    .expect("encode")
                    .is_keyframe
            })
            .collect();

        // Keyframes at indices 0, 3.
        assert_eq!(results, vec![true, false, false, true, false, false]);
    }

    #[test]
    fn test_vp8_config_validation() {
        let bad = Vp8EncoderConfig {
            width: 0,
            ..Default::default()
        };
        assert!(Vp8Encoder::new(bad).is_err());
    }
}
