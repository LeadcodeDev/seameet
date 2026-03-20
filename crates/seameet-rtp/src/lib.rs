//! RTP/RTCP transport layer for the SeaMeet real-time communication framework.
//!
//! Provides RTP packet parsing/serialization, RTCP feedback messages, a
//! jitter buffer for reordering, and an RTP sender with video fragmentation.

pub mod jitter;
pub mod packet;
pub mod rtcp;
pub mod sender;

pub use jitter::{JitterBuffer, JitterBufferEvent, JitterStats};
pub use packet::{RtpPacket, PT_AUDIO, PT_VIDEO};
pub use rtcp::RtcpPacket;
pub use sender::{RtpSender, SenderStats};
