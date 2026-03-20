use std::future::Future;

use seameet_core::SeaMeetError;

use crate::message::SdpMessage;

/// Abstraction over a signaling transport (WebSocket, HTTP, etc.).
pub trait SignalingBackend: Send {
    /// Sends an [`SdpMessage`] to the remote peer or server.
    fn send(&self, msg: SdpMessage) -> impl Future<Output = Result<(), SeaMeetError>> + Send;

    /// Receives the next [`SdpMessage`] from the remote peer or server.
    fn recv(&mut self) -> impl Future<Output = Result<SdpMessage, SeaMeetError>> + Send;

    /// Gracefully closes the signaling connection.
    fn close(&self) -> impl Future<Output = Result<(), SeaMeetError>> + Send;
}
