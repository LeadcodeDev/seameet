use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use seameet_core::SeaMeetError;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

use crate::transport::{ConnectionReader, IncomingConnection, TransportListener};

/// How long to wait for any frame before treating the connection as dead.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to send a WS Ping to keep the read timer from expiring on
/// live-but-quiet connections.  Browsers auto-respond with Pong frames.
const PING_INTERVAL: Duration = Duration::from_secs(10);

/// WebSocket reader wrapping a `tokio_tungstenite` stream split.
///
/// Generic over the underlying I/O type so it can be unit-tested with
/// an in-memory duplex stream instead of a real TCP socket.
struct WsReader<S> {
    stream: futures_util::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    idle_timeout: Duration,
}

impl<S> ConnectionReader for WsReader<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    fn recv(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send + '_>> {
        Box::pin(async move {
            loop {
                match tokio::time::timeout(self.idle_timeout, self.stream.next()).await {
                    // Idle timeout expired — treat as dead connection.
                    Err(_elapsed) => return None,
                    // Valid text frame.
                    Ok(Some(Ok(Message::Text(t)))) => return Some(t.to_string()),
                    // Close frame or stream exhausted.
                    Ok(Some(Ok(Message::Close(_)))) | Ok(None) => return None,
                    // Ping / Pong / Binary — reset the idle timer and loop.
                    Ok(Some(Ok(_))) => continue,
                    // Transport error.
                    Ok(Some(Err(_))) => return None,
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

            // Spawn writer task: pumps from mpsc channel into the WS sink, and
            // periodically sends Ping frames so the read-side idle timer does
            // not expire on live-but-quiet connections.
            let mut sink = ws_sink;
            let writer_task = tokio::spawn(async move {
                let mut ping = tokio::time::interval(PING_INTERVAL);
                ping.tick().await; // discard the immediate first tick
                loop {
                    tokio::select! {
                        maybe = rx.recv() => match maybe {
                            Some(text) => {
                                if sink.send(Message::Text(text)).await.is_err() {
                                    break;
                                }
                            }
                            None => break,
                        },
                        _ = ping.tick() => {
                            if sink.send(Message::Ping(Vec::new())).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });

            return Some(IncomingConnection {
                reader: Box::new(WsReader {
                    stream: ws_stream,
                    idle_timeout: IDLE_TIMEOUT,
                }),
                writer: tx,
                writer_handle: Some(writer_task.abort_handle()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::protocol::Role;

    #[tokio::test]
    async fn reader_returns_text_then_none_after_idle_timeout() {
        let (server_io, client_io) = tokio::io::duplex(4096);
        let server_ws =
            tokio_tungstenite::WebSocketStream::from_raw_socket(server_io, Role::Server, None)
                .await;
        let mut client_ws =
            tokio_tungstenite::WebSocketStream::from_raw_socket(client_io, Role::Client, None)
                .await;
        let (_server_sink, server_stream) = server_ws.split();
        let mut reader = WsReader {
            stream: server_stream,
            idle_timeout: Duration::from_millis(150),
        };

        // A text frame is delivered.
        client_ws
            .send(Message::Text("hello".to_string()))
            .await
            .unwrap();
        assert_eq!(reader.recv().await, Some("hello".to_string()));

        // Then the client goes silent → idle timeout → recv returns None.
        assert_eq!(reader.recv().await, None);

        // Keep client end alive so the socket isn't closed prematurely.
        let _ = client_ws;
    }
}
