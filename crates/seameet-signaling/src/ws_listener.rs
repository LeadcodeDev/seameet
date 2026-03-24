use futures_util::{SinkExt, StreamExt};
use seameet_core::SeaMeetError;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

use crate::transport::{ConnectionReader, IncomingConnection, TransportListener};

/// WebSocket reader wrapping a `tokio_tungstenite` stream split.
struct WsReader {
    stream: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    >,
}

impl ConnectionReader for WsReader {
    fn recv(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send + '_>> {
        Box::pin(async move {
            loop {
                match self.stream.next().await {
                    Some(Ok(Message::Text(t))) => return Some(t),
                    Some(Ok(Message::Close(_))) | None => return None,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => return None,
                }
            }
        })
    }
}

/// WebSocket transport listener backed by `tokio_tungstenite`.
///
/// Accepts TCP connections, performs the WebSocket handshake, and returns
/// [`IncomingConnection`]s ready for the signaling engine.
pub struct WsListener {
    listener: TcpListener,
}

impl WsListener {
    /// Binds a TCP listener on the given address and returns a new `WsListener`.
    pub async fn bind(addr: &str) -> Result<Self, SeaMeetError> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self { listener })
    }

    /// Returns the local address the listener is bound to.
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, SeaMeetError> {
        self.listener.local_addr().map_err(SeaMeetError::Io)
    }
}

impl TransportListener for WsListener {
    async fn accept(&mut self) -> Option<IncomingConnection> {
        loop {
            let (stream, addr) = match self.listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    warn!("TCP accept error: {e}");
                    continue;
                }
            };
            debug!(%addr, "new TCP connection");

            let ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    warn!(%addr, "WebSocket handshake failed: {e}");
                    continue;
                }
            };

            let (ws_sink, ws_stream) = ws.split();
            let (tx, mut rx) = mpsc::unbounded_channel::<String>();

            // Spawn writer task: pumps from mpsc channel into the WS sink.
            let mut sink = ws_sink;
            let writer_task = tokio::spawn(async move {
                while let Some(text) = rx.recv().await {
                    if sink.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
            });

            return Some(IncomingConnection {
                reader: Box::new(WsReader { stream: ws_stream }),
                writer: tx,
                writer_handle: Some(writer_task.abort_handle()),
            });
        }
    }
}
