use async_trait::async_trait;
use seameet_core::SeaMeetError;

use crate::message::SdpMessage;

/// Abstraction over a signaling transport (WebSocket, HTTP, etc.).
#[async_trait]
pub trait SignalingBackend: Send {
    /// Sends an [`SdpMessage`] to the remote peer or server.
    async fn send(&self, msg: SdpMessage) -> Result<(), SeaMeetError>;

    /// Receives the next [`SdpMessage`] from the remote peer or server.
    async fn recv(&mut self) -> Result<SdpMessage, SeaMeetError>;

    /// Gracefully closes the signaling connection.
    async fn close(&self) -> Result<(), SeaMeetError>;
}
