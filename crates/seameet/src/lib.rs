//! # seameet
//!
//! Real-time audio/video peer for WebRTC.
//! Rust receives, processes, and re-emits media between browser participants.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use seameet::{Room, RoomConfig, RoomEvent, Passthrough, WsSignaling, ParticipantId};
//! use tokio_stream::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), seameet::SeaMeetError> {
//!     let room = Room::new(RoomConfig::default());
//!     let mut events = room.events();
//!
//!     // Connect participant A
//!     let signaling_a = WsSignaling::connect("ws://localhost:8080").await?;
//!     let handle_a = room.add_participant(
//!         ParticipantId::random(),
//!         signaling_a,
//!         Passthrough,
//!     ).await?;
//!
//!     // Listen to room events
//!     while let Some(event) = events.next().await {
//!         match event {
//!             RoomEvent::RoomEnded { .. } => break,
//!             RoomEvent::SpeechStarted { participant } => {
//!                 println!("{participant} started speaking");
//!             }
//!             _ => {}
//!         }
//!     }
//!     Ok(())
//! }
//! ```
//!
//! ## Participant management
//!
//! ```rust,no_run
//! # use seameet::{Room, RoomConfig, ParticipantId, SeaMeetError};
//! # fn example(room: &Room, id_a: &ParticipantId) -> Result<(), SeaMeetError> {
//! // Check if a participant is present
//! if room.has_participant(id_a) {
//!     let handle = room.get_participant(id_a)?;
//! }
//!
//! // List all participants
//! let handles = room.fetch_participants();
//! println!("{} participant(s) connected", handles.len());
//! # Ok(())
//! # }
//! ```

pub mod room;

// ── Core types ──────────────────────────────────────────────────────────

pub use seameet_core::{
    EncodedAudio, EncodedVideo, ParticipantId, PcmFrame, RoomEvent, SeaMeetError, TrackDirection,
    TrackHandle, TrackId, VideoFrame, VideoTrackKind,
};
pub use uuid::Uuid;

// ── Traits ──────────────────────────────────────────────────────────────

pub use seameet_core::{AudioSource, Decoder, Encoder, Processor, ProcessorHint, VideoSource};

// ── Codec implementations ───────────────────────────────────────────────

pub use seameet_codec::{
    AudioPassthrough, OpusDecoder, OpusEncoder, OpusEncoderConfig, Vp8Decoder, Vp8Encoder,
    Vp8EncoderConfig,
};
pub use seameet_core::Passthrough;

// ── Signaling ───────────────────────────────────────────────────────────

pub use seameet_signaling::engine::{
    dispatch as signaling_dispatch, run_connection, NoopHooks, SignalingHooks, SignalingState,
    Room as SignalingRoom, Member, WsSink,
};
pub use seameet_signaling::transport::{ConnectionReader, IncomingConnection, TransportListener};
pub use seameet_signaling::{SdpMessage, SignalingBackend};

#[cfg(feature = "tungstenite")]
pub use seameet_signaling::{WsSignaling, WsSignalingConfig};
#[cfg(feature = "tungstenite")]
pub use seameet_signaling::ws_listener::WsListener;

// ── Pipeline (low-level escape hatch) ───────────────────────────────────

pub use seameet_pipeline::{
    InboundPipeline, MediaKind, Mid, OutboundPipeline, PeerCmd, PeerConnection, PeerEvent,
    ProcessorChain,
};

// ── SFU ───────────────────────────────────────────────────────────────

#[cfg(feature = "sfu")]
pub use seameet_sfu as sfu;

// ── High-level facade ───────────────────────────────────────────────────

pub use room::{Room, RoomConfig, RoomHandle};
