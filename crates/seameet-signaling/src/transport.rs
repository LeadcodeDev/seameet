use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;

/// Reads incoming text messages from a connection.
///
/// Implementations wrap transport-specific read halves (WebSocket stream,
/// HTTP long-poll, etc.) and present a uniform `recv()` interface to the
/// signaling engine.
pub trait ConnectionReader: Send + 'static {
    /// Returns the next text message, or `None` when the connection closes.
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>>;
}

/// A newly accepted connection, ready for the signaling engine.
///
/// The `writer` channel is transport-agnostic — the adapter spawns its own
/// task to pump messages from this channel into the real transport sink.
pub struct IncomingConnection {
    /// Reader half of the connection.
    pub reader: Box<dyn ConnectionReader>,
    /// Sender for outbound text messages.
    pub writer: mpsc::UnboundedSender<String>,
}

/// Accepts incoming connections from a specific transport.
///
/// Each call to [`accept`](TransportListener::accept) blocks until a new
/// connection is ready, returning `None` when the listener shuts down.
pub trait TransportListener: Send + 'static {
    /// Waits for the next connection.
    fn accept(&mut self) -> impl Future<Output = Option<IncomingConnection>> + Send;
}
