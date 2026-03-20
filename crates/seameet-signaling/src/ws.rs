use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use seameet_core::SeaMeetError;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, warn};

use crate::message::SdpMessage;
use crate::traits::SignalingBackend;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type SplitSink = futures_util::stream::SplitSink<WsStream, Message>;
type SplitStream = futures_util::stream::SplitStream<WsStream>;

/// Configuration for [`WsSignaling`].
#[derive(Debug, Clone)]
pub struct WsSignalingConfig {
    /// Timeout for receiving a message. Defaults to 30 seconds.
    pub recv_timeout: Duration,
    /// Maximum number of reconnection attempts. Defaults to 3.
    pub max_reconnect_attempts: u32,
    /// Base delay for exponential backoff. Defaults to 1 second.
    pub base_backoff: Duration,
}

impl Default for WsSignalingConfig {
    fn default() -> Self {
        Self {
            recv_timeout: Duration::from_secs(30),
            max_reconnect_attempts: 3,
            base_backoff: Duration::from_secs(1),
        }
    }
}

/// WebSocket-based signaling backend.
///
/// Connects to a signaling server over WebSocket and exchanges [`SdpMessage`]s
/// serialized as JSON text frames.
pub struct WsSignaling {
    /// URL used for reconnection.
    url: String,
    /// Configuration.
    config: WsSignalingConfig,
    /// Write half of the WebSocket, shared for concurrent sends.
    sink: Arc<Mutex<SplitSink>>,
    /// Read half of the WebSocket.
    stream: SplitStream,
}

impl std::fmt::Debug for WsSignaling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsSignaling")
            .field("url", &self.url)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl WsSignaling {
    /// Connects to the signaling server at `url` with default configuration.
    pub async fn connect(url: &str) -> Result<Self, SeaMeetError> {
        Self::connect_with_config(url, WsSignalingConfig::default()).await
    }

    /// Connects to the signaling server at `url` with the given configuration.
    pub async fn connect_with_config(
        url: &str,
        config: WsSignalingConfig,
    ) -> Result<Self, SeaMeetError> {
        let ws = Self::connect_ws(url, &config).await?;
        let (sink, stream) = ws.split();
        Ok(Self {
            url: url.to_owned(),
            config,
            sink: Arc::new(Mutex::new(sink)),
            stream,
        })
    }

    /// Low-level WebSocket connection with exponential backoff reconnection.
    async fn connect_ws(url: &str, config: &WsSignalingConfig) -> Result<WsStream, SeaMeetError> {
        let mut attempt = 0u32;
        loop {
            match tokio_tungstenite::connect_async(url).await {
                Ok((ws, _)) => {
                    debug!(url, "WebSocket connected");
                    return Ok(ws);
                }
                Err(e) => {
                    attempt += 1;
                    if attempt > config.max_reconnect_attempts {
                        return Err(SeaMeetError::Signaling(format!(
                            "failed to connect after {} attempts: {e}",
                            config.max_reconnect_attempts
                        )));
                    }
                    let delay = config.base_backoff * 2u32.pow(attempt - 1);
                    warn!(
                        attempt,
                        max = config.max_reconnect_attempts,
                        ?delay,
                        "connection failed, retrying: {e}"
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

#[async_trait]
impl SignalingBackend for WsSignaling {
    async fn send(&self, msg: SdpMessage) -> Result<(), SeaMeetError> {
        let json = serde_json::to_string(&msg)
            .map_err(|e| SeaMeetError::Signaling(format!("serialize error: {e}")))?;
        let mut sink = self.sink.lock().await;
        sink.send(Message::Text(json))
            .await
            .map_err(|e| SeaMeetError::Signaling(format!("send error: {e}")))?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<SdpMessage, SeaMeetError> {
        let timeout = self.config.recv_timeout;
        loop {
            let maybe = tokio::time::timeout(timeout, self.stream.next()).await;
            match maybe {
                Err(_) => {
                    return Err(SeaMeetError::Signaling("recv timed out".into()));
                }
                Ok(None) => {
                    return Err(SeaMeetError::Signaling("connection closed".into()));
                }
                Ok(Some(Err(e))) => {
                    return Err(SeaMeetError::Signaling(format!("recv error: {e}")));
                }
                Ok(Some(Ok(msg))) => match msg {
                    Message::Text(text) => {
                        let sdp: SdpMessage = serde_json::from_str(&text).map_err(|e| {
                            SeaMeetError::Signaling(format!("deserialize error: {e}"))
                        })?;
                        return Ok(sdp);
                    }
                    Message::Ping(data) => {
                        let mut sink = self.sink.lock().await;
                        let _ = sink.send(Message::Pong(data)).await;
                        continue;
                    }
                    Message::Pong(_) => continue,
                    Message::Close(_) => {
                        return Err(SeaMeetError::Signaling("connection closed".into()));
                    }
                    _ => continue,
                },
            }
        }
    }

    async fn close(&self) -> Result<(), SeaMeetError> {
        let mut sink = self.sink.lock().await;
        sink.send(Message::Close(None))
            .await
            .map_err(|e| SeaMeetError::Signaling(format!("close error: {e}")))?;
        Ok(())
    }
}
