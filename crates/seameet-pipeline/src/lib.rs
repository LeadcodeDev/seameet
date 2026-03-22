//! Media pipeline layer for the SeaMeet real-time communication framework.
//!
//! Provides inbound (receive → jitter → decode) and outbound (process → encode
//! → RTP send) pipelines, a processor chain, and a `Peer` that aggregates both
//! directions for a single participant.

pub mod inbound;
pub mod outbound;
pub mod peer;
pub mod peer_connection;
pub mod processor;

pub use inbound::{InboundEvent, InboundPipeline};
pub use outbound::{rtp_channel, OutboundPipeline};
pub use peer::Peer;
pub use peer_connection::{PeerCmd, PeerConnection, PeerEvent};
pub use str0m::media::{MediaKind, Mid};
pub use processor::ProcessorChain;
