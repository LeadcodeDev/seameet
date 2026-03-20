use std::net::SocketAddr;
use std::sync::Arc;

use axum::response::Html;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use seameet::{signaling_dispatch, ParticipantId, SdpMessage, SignalingState};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

type State = Arc<Mutex<SignalingState>>;

const INDEX_HTML: &str = include_str!("../static/index.html");

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let state: State = Arc::new(Mutex::new(SignalingState::new()));

    let http_handle = tokio::spawn(serve_http());
    let ws_handle = tokio::spawn(serve_ws(state));

    info!("HTTP  → http://localhost:3000");
    info!("WS    → ws://localhost:3001");

    let _ = tokio::join!(http_handle, ws_handle);
}

async fn serve_http() {
    let app = Router::new().route("/", get(|| async { Html(INDEX_HTML) }));
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("failed to bind HTTP on {addr}: {e}");
            return;
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        error!("HTTP server error: {e}");
    }
}

async fn serve_ws(state: State) {
    let addr = SocketAddr::from(([0, 0, 0, 0], 3001));
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("failed to bind WS on {addr}: {e}");
            return;
        }
    };

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                warn!("accept error: {e}");
                continue;
            }
        };
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    warn!(%peer_addr, "WS handshake failed: {e}");
                    return;
                }
            };
            info!(%peer_addr, "WS connected");
            handle_ws(ws, state, peer_addr).await;
        });
    }
}

async fn handle_ws(
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    state: State,
    peer_addr: SocketAddr,
) {
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let writer = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if ws_tx.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });

    let mut participant_id: Option<ParticipantId> = None;

    while let Some(result) = ws_rx.next().await {
        let msg = match result {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(e) => {
                warn!(%peer_addr, "WS read error: {e}");
                break;
            }
        };

        let sdp: SdpMessage = match serde_json::from_str(&msg) {
            Ok(m) => m,
            Err(e) => {
                warn!(%peer_addr, "invalid message: {e}");
                continue;
            }
        };

        if participant_id.is_none() {
            if let SdpMessage::Join { participant, .. } = &sdp {
                participant_id = Some(*participant);
            } else {
                continue;
            }
        }

        let pid = match participant_id {
            Some(id) => id,
            None => continue,
        };

        signaling_dispatch(&sdp, &msg, pid, &tx, &state).await;
    }

    // Connection closed.
    if let Some(pid) = participant_id {
        info!(participant = %pid, %peer_addr, "disconnected");
        let affected = state.lock().await.disconnect(&pid);
        for (room_id, _) in affected {
            let leave = SdpMessage::Leave {
                participant: pid,
                room_id: room_id.clone(),
            };
            if let Ok(json) = serde_json::to_string(&leave) {
                let st = state.lock().await;
                let peers = st.peers(&room_id, &pid);
                drop(st);
                for peer_tx in peers {
                    let _ = peer_tx.send(json.clone());
                }
            }
        }
    }

    drop(tx);
    let _ = writer.await;
}
