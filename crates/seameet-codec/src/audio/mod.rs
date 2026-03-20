use seameet_core::frame::{EncodedAudio, PcmFrame};
use seameet_core::traits::{Decoder, Encoder};
use seameet_core::SeaMeetError;

/// Valid Opus sample rates.
const VALID_SAMPLE_RATES: [u32; 5] = [8000, 12000, 16000, 24000, 48000];

/// Configuration for the Opus encoder.
#[derive(Debug, Clone)]
pub struct OpusEncoderConfig {
    /// Sample rate in Hz. Must be one of: 8000, 12000, 16000, 24000, 48000.
    pub sample_rate: u32,
    /// Number of audio channels (1 = mono, 2 = stereo).
    pub channels: u16,
    /// Target bitrate in bits per second (e.g. 64000).
    pub bitrate: u32,
    /// Frame duration in milliseconds (e.g. 20).
    pub frame_ms: u32,
}

impl Default for OpusEncoderConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            bitrate: 64000,
            frame_ms: 20,
        }
    }
}

/// Validates that the given config has a supported sample rate and channel count.
fn validate_config(config: &OpusEncoderConfig) -> Result<(), SeaMeetError> {
    if !VALID_SAMPLE_RATES.contains(&config.sample_rate) {
        return Err(SeaMeetError::Codec(format!(
            "unsupported sample rate: {}. Valid rates: {:?}",
            config.sample_rate, VALID_SAMPLE_RATES
        )));
    }
    if config.channels == 0 || config.channels > 2 {
        return Err(SeaMeetError::Codec(format!(
            "unsupported channel count: {}. Must be 1 or 2",
            config.channels
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Stub implementation (no opus-ffi feature)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "opus-ffi"))]
mod inner {
    use super::*;

    /// Opus audio encoder.
    ///
    /// Without the `opus-ffi` feature this is a **stub** that produces minimal
    /// valid Opus silence frames. Enable `opus-ffi` for real encoding.
    #[derive(Debug)]
    pub struct OpusEncoder {
        config: OpusEncoderConfig,
    }

    impl OpusEncoder {
        /// Creates a new [`OpusEncoder`] with the given configuration.
        pub fn new(config: OpusEncoderConfig) -> Result<Self, SeaMeetError> {
            validate_config(&config)?;
            Ok(Self { config })
        }

        /// Returns the encoder configuration.
        pub fn config(&self) -> &OpusEncoderConfig {
            &self.config
        }
    }

    impl Encoder<PcmFrame, EncodedAudio> for OpusEncoder {
        fn encode(&mut self, _input: PcmFrame) -> Result<EncodedAudio, SeaMeetError> {
            tracing::warn!("using Opus stub encoder — enable feature `opus-ffi` for real encoding");
            // 3 bytes of zeroes is a valid Opus silence frame (TOC byte 0x00 = SILK, NB, 10ms).
            Ok(EncodedAudio(vec![0u8; 3]))
        }
    }

    /// Opus audio decoder.
    ///
    /// Without the `opus-ffi` feature this is a **stub** that produces silent
    /// PCM output. Enable `opus-ffi` for real decoding.
    #[derive(Debug)]
    pub struct OpusDecoder {
        config: OpusEncoderConfig,
    }

    impl OpusDecoder {
        /// Creates a new [`OpusDecoder`] with the given configuration.
        pub fn new(config: OpusEncoderConfig) -> Result<Self, SeaMeetError> {
            validate_config(&config)?;
            Ok(Self { config })
        }
    }

    impl Decoder<EncodedAudio, PcmFrame> for OpusDecoder {
        fn decode(&mut self, _input: EncodedAudio) -> Result<PcmFrame, SeaMeetError> {
            tracing::warn!("using Opus stub decoder — enable feature `opus-ffi` for real decoding");
            let frame_samples =
                (self.config.sample_rate / 1000 * self.config.frame_ms) as usize
                    * self.config.channels as usize;
            Ok(PcmFrame {
                samples: vec![0.0; frame_samples],
                sample_rate: self.config.sample_rate,
                channels: self.config.channels,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Real FFI implementation (opus-ffi feature)
// ---------------------------------------------------------------------------
#[cfg(feature = "opus-ffi")]
mod inner {
    use super::*;
    use audiopus::coder::{
        Decoder as RawDecoder, Encoder as RawEncoder,
    };
    use audiopus::{Application, Channels, SampleRate};

    /// Opus audio encoder backed by `libopus` via `audiopus`.
    #[derive(Debug)]
    pub struct OpusEncoder {
        config: OpusEncoderConfig,
        encoder: RawEncoder,
    }

    impl OpusEncoder {
        /// Creates a new [`OpusEncoder`] with the given configuration.
        pub fn new(config: OpusEncoderConfig) -> Result<Self, SeaMeetError> {
            validate_config(&config)?;
            let sr = to_sample_rate(config.sample_rate)?;
            let ch = to_channels(config.channels)?;
            let mut encoder = RawEncoder::new(sr, ch, Application::Audio)
                .map_err(|e| SeaMeetError::Codec(format!("opus encoder init: {e}")))?;
            encoder
                .set_bitrate(audiopus::Bitrate::BitsPerSecond(config.bitrate as i32))
                .map_err(|e| SeaMeetError::Codec(format!("opus set bitrate: {e}")))?;
            Ok(Self { config, encoder })
        }

        /// Returns the encoder configuration.
        pub fn config(&self) -> &OpusEncoderConfig {
            &self.config
        }
    }

    impl Encoder<PcmFrame, EncodedAudio> for OpusEncoder {
        fn encode(&mut self, input: PcmFrame) -> Result<EncodedAudio, SeaMeetError> {
            let mut out = vec![0u8; 4000];
            let n = self
                .encoder
                .encode(&input.samples, &mut out)
                .map_err(|e| SeaMeetError::Codec(format!("opus encode: {e}")))?;
            out.truncate(n);
            Ok(EncodedAudio(out))
        }
    }

    /// Opus audio decoder backed by `libopus` via `audiopus`.
    #[derive(Debug)]
    pub struct OpusDecoder {
        config: OpusEncoderConfig,
        decoder: RawDecoder,
    }

    impl OpusDecoder {
        /// Creates a new [`OpusDecoder`] with the given configuration.
        pub fn new(config: OpusEncoderConfig) -> Result<Self, SeaMeetError> {
            validate_config(&config)?;
            let sr = to_sample_rate(config.sample_rate)?;
            let ch = to_channels(config.channels)?;
            let decoder = RawDecoder::new(sr, ch)
                .map_err(|e| SeaMeetError::Codec(format!("opus decoder init: {e}")))?;
            Ok(Self { config, decoder })
        }
    }

    impl Decoder<EncodedAudio, PcmFrame> for OpusDecoder {
        fn decode(&mut self, input: EncodedAudio) -> Result<PcmFrame, SeaMeetError> {
            let frame_samples =
                (self.config.sample_rate / 1000 * self.config.frame_ms) as usize
                    * self.config.channels as usize;
            let mut pcm = vec![0.0f32; frame_samples];

            // Empty payload triggers PLC (packet loss concealment).
            let data = if input.0.is_empty() {
                None
            } else {
                Some(input.0.as_slice())
            };
            let n = self
                .decoder
                .decode_float(data, &mut pcm, false)
                .map_err(|e| SeaMeetError::Codec(format!("opus decode: {e}")))?;
            pcm.truncate(n * self.config.channels as usize);

            Ok(PcmFrame {
                samples: pcm,
                sample_rate: self.config.sample_rate,
                channels: self.config.channels,
            })
        }
    }

    fn to_sample_rate(sr: u32) -> Result<SampleRate, SeaMeetError> {
        match sr {
            8000 => Ok(SampleRate::Hz8000),
            12000 => Ok(SampleRate::Hz12000),
            16000 => Ok(SampleRate::Hz16000),
            24000 => Ok(SampleRate::Hz24000),
            48000 => Ok(SampleRate::Hz48000),
            _ => Err(SeaMeetError::Codec(format!("unsupported sample rate: {sr}"))),
        }
    }

    fn to_channels(ch: u16) -> Result<Channels, SeaMeetError> {
        match ch {
            1 => Ok(Channels::Mono),
            2 => Ok(Channels::Stereo),
            _ => Err(SeaMeetError::Codec(format!("unsupported channels: {ch}"))),
        }
    }
}

pub use inner::{OpusDecoder, OpusEncoder};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opus_stub_encode_decode() {
        let config = OpusEncoderConfig::default();
        let mut encoder = OpusEncoder::new(config.clone()).expect("encoder");
        let frame = PcmFrame {
            samples: vec![0.0; 960],
            sample_rate: 48000,
            channels: 1,
        };
        let encoded = encoder.encode(frame).expect("encode");
        assert!(!encoded.0.is_empty());

        let mut decoder = OpusDecoder::new(config).expect("decoder");
        let decoded = decoder.decode(encoded).expect("decode");
        assert_eq!(decoded.sample_rate, 48000);
        assert_eq!(decoded.channels, 1);
    }

    #[test]
    fn test_opus_config_validation() {
        let bad_rate = OpusEncoderConfig {
            sample_rate: 44100,
            ..Default::default()
        };
        assert!(OpusEncoder::new(bad_rate).is_err());

        let bad_channels = OpusEncoderConfig {
            channels: 0,
            ..Default::default()
        };
        assert!(OpusEncoder::new(bad_channels).is_err());

        let bad_channels_3 = OpusEncoderConfig {
            channels: 3,
            ..Default::default()
        };
        assert!(OpusEncoder::new(bad_channels_3).is_err());
    }

    #[cfg(feature = "opus-ffi")]
    #[test]
    fn test_opus_ffi_encode_decode() {
        let config = OpusEncoderConfig {
            sample_rate: 48000,
            channels: 1,
            bitrate: 64000,
            frame_ms: 20,
        };
        let mut encoder = OpusEncoder::new(config.clone()).expect("encoder");
        // 20ms at 48kHz mono = 960 samples.
        let frame = PcmFrame {
            samples: vec![0.0; 960],
            sample_rate: 48000,
            channels: 1,
        };
        let encoded = encoder.encode(frame).expect("encode");
        assert!(!encoded.0.is_empty());

        let mut decoder = OpusDecoder::new(config).expect("decoder");
        let decoded = decoder.decode(encoded).expect("decode");
        assert!(!decoded.samples.is_empty());
        assert_eq!(decoded.sample_rate, 48000);
    }
}
