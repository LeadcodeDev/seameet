//! Convenience wrapper that combines [`WsListener`](crate::ws_listener::WsListener)
//! with the signaling [`engine`](crate::engine) for backwards compatibility.

use std::sync::Arc;

use seameet_core::SeaMeetError;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::info;

use crate::engine::{NoopHooks, SignalingState};
use crate::transport::TransportListener;
use crate::ws_listener::WsListener;

// Re-export engine types that were previously defined here.
pub use crate::engine::{dispatch, Member, Room, WsSink};

/// In-memory WebSocket signaling server.
///
/// Routes [`SdpMessage`](crate::SdpMessage)s between participants across named rooms.
/// A single WebSocket connection can join multiple rooms.
pub struct RoomServer {
    listener: WsListener,
    state: Arc<RwLock<SignalingState>>,
    require_e2ee: bool,
    e2ee_join_timeout: std::time::Duration,
}

impl RoomServer {
    /// Binds the server to the given address (e.g. `"127.0.0.1:0"`).
    pub async fn bind(addr: &str) -> Result<Self, SeaMeetError> {
        let listener = WsListener::bind(addr).await?;
        Ok(Self {
            listener,
            state: Arc::new(RwLock::new(SignalingState::new())),
            require_e2ee: false,
            e2ee_join_timeout: std::time::Duration::from_secs(5),
        })
    }

    /// Require participants to publish an `e2ee_public_key` within
    /// [`Self::with_e2ee_join_timeout`] of `Join` (default 5s).
    pub fn require_e2ee(mut self, required: bool) -> Self {
        self.require_e2ee = required;
        if required {
            // Pre-populate state so RoomStatus advertises the requirement.
            self.state.try_write().expect("freshly built").set_e2ee_required(true);
        }
        self
    }

    /// Override the E2EE handshake deadline (default 5s).
    pub fn with_e2ee_join_timeout(mut self, dur: std::time::Duration) -> Self {
        self.e2ee_join_timeout = dur;
        self
    }

    /// Returns the local address the server is listening on.
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, SeaMeetError> {
        self.listener.local_addr()
    }

    /// Starts accepting connections. Returns a [`JoinHandle`] for the server task.
    pub fn run(self) -> JoinHandle<()> {
        let state = self.state;
        let mut listener = self.listener;
        if self.require_e2ee {
            let hooks = Arc::new(crate::engine::E2eeNoopHooks {
                timeout: self.e2ee_join_timeout,
            });
            tokio::spawn(async move {
                while let Some(conn) = listener.accept().await {
                    let state = Arc::clone(&state);
                    let hooks = Arc::clone(&hooks);
                    tokio::spawn(crate::engine::run_connection(conn, state, hooks));
                }
                info!("RoomServer listener closed");
            })
        } else {
            let hooks = Arc::new(NoopHooks);
            tokio::spawn(async move {
                while let Some(conn) = listener.accept().await {
                    let state = Arc::clone(&state);
                    let hooks = Arc::clone(&hooks);
                    tokio::spawn(crate::engine::run_connection(conn, state, hooks));
                }
                info!("RoomServer listener closed");
            })
        }
    }
}
