//! Audio and video codecs for the SeaMeet real-time communication framework.
//!
//! # Feature flags
//!
//! | Feature    | Description |
//! |------------|-------------|
//! | `opus-ffi` | Enables real Opus encoding/decoding via `libopus` (`audiopus` crate). Without this feature, stub implementations produce silence. |
//! | `vp8-ffi`  | Enables real VP8 encoding/decoding via `libvpx` (`vpx` crate). Without this feature, stub implementations produce minimal black frames. |
//!
//! For end-to-end testing without any native dependencies, use
//! [`AudioPassthrough`] which round-trips PCM samples through a trivial
//! byte serialization.

pub mod audio;
pub mod passthrough;
pub mod video;

pub use audio::{OpusDecoder, OpusEncoder, OpusEncoderConfig};
pub use passthrough::AudioPassthrough;
pub use video::{Vp8Decoder, Vp8Encoder, Vp8EncoderConfig};
