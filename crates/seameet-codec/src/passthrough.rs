use seameet_core::frame::{EncodedAudio, PcmFrame};
use seameet_core::traits::{Decoder, Encoder};
use seameet_core::SeaMeetError;

/// A no-op audio codec that passes raw PCM data through without transformation.
///
/// Samples are serialized as little-endian `f32` bytes for the "encoded" form
/// and deserialized back. Useful for end-to-end testing without FFI codecs.
#[derive(Debug, Clone, Default)]
pub struct AudioPassthrough;

impl Encoder<PcmFrame, EncodedAudio> for AudioPassthrough {
    fn encode(&mut self, input: PcmFrame) -> Result<EncodedAudio, SeaMeetError> {
        let mut bytes = Vec::with_capacity(
            input.samples.len() * 4 + 6, // samples + sample_rate(4) + channels(2)
        );
        bytes.extend_from_slice(&input.sample_rate.to_le_bytes());
        bytes.extend_from_slice(&input.channels.to_le_bytes());
        for &s in &input.samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        Ok(EncodedAudio(bytes))
    }
}

impl Decoder<EncodedAudio, PcmFrame> for AudioPassthrough {
    fn decode(&mut self, input: EncodedAudio) -> Result<PcmFrame, SeaMeetError> {
        let data = &input.0;
        if data.len() < 6 {
            return Err(SeaMeetError::Codec(
                "AudioPassthrough: payload too short".into(),
            ));
        }
        let sample_rate = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let channels = u16::from_le_bytes([data[4], data[5]]);
        let sample_bytes = &data[6..];
        if sample_bytes.len() % 4 != 0 {
            return Err(SeaMeetError::Codec(
                "AudioPassthrough: sample data not aligned to f32".into(),
            ));
        }
        let samples: Vec<f32> = sample_bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        Ok(PcmFrame {
            samples,
            sample_rate,
            channels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_passthrough_audio_roundtrip() {
        let mut codec = AudioPassthrough;
        let original = PcmFrame {
            samples: vec![0.1, -0.5, 0.0, 1.0, -1.0],
            sample_rate: 48000,
            channels: 2,
        };
        let encoded = codec.encode(original.clone()).expect("encode");
        let decoded = codec.decode(encoded).expect("decode");

        assert_eq!(decoded.samples, original.samples);
        assert_eq!(decoded.sample_rate, original.sample_rate);
        assert_eq!(decoded.channels, original.channels);
    }

    #[test]
    fn test_passthrough_empty_samples() {
        let mut codec = AudioPassthrough;
        let original = PcmFrame {
            samples: vec![],
            sample_rate: 16000,
            channels: 1,
        };
        let encoded = codec.encode(original.clone()).expect("encode");
        let decoded = codec.decode(encoded).expect("decode");
        assert!(decoded.samples.is_empty());
        assert_eq!(decoded.sample_rate, 16000);
    }

    #[test]
    fn test_passthrough_decode_too_short() {
        let mut codec = AudioPassthrough;
        let result = codec.decode(EncodedAudio(vec![0, 1]));
        assert!(result.is_err());
    }
}
