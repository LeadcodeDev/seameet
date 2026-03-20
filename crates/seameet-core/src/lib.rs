//! Core types and traits for the SeaMeet real-time communication framework.

pub mod error;
pub mod events;
pub mod frame;
pub mod participant;
pub mod track;
pub mod traits;

pub use error::{Result, SeaMeetError};
pub use events::RoomEvent;
pub use frame::{EncodedAudio, EncodedVideo, PcmFrame, VideoFrame};
pub use participant::ParticipantId;
pub use track::{TrackDirection, TrackHandle, TrackId};
pub use traits::{AudioSource, Decoder, Encoder, Passthrough, Processor, ProcessorHint, VideoSource};
