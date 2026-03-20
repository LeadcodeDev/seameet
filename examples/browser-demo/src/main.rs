use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::response::Html;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use seameet::SdpMessage;
use seameet::ParticipantId;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

/// Per-peer outbound channel.
type PeerTx = mpsc::UnboundedSender<String>;

/// A peer registered in a room.
#[derive(Debug)]
struct Peer {
    id: ParticipantId,
    tx: PeerTx,
}

/// Rooms mapped by room_id → list of peers.
type Rooms = Arc<Mutex<HashMap<String, Vec<Peer>>>>;

// Embed index.html at compile time.
const INDEX_HTML: &str = include_str!("../static/index.html");

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let rooms: Rooms = Arc::new(Mutex::new(HashMap::new()));

    let http_handle = tokio::spawn(serve_http());
    let ws_handle = tokio::spawn(serve_ws(rooms));

    info!("HTTP  → http://localhost:3000");
    info!("WS    → ws://localhost:3001");

    let _ = tokio::join!(http_handle, ws_handle);
}

/// Serves the static HTML page on port 3000.
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

/// Accepts WebSocket connections on port 3001 and relays signaling messages.
async fn serve_ws(rooms: Rooms) {
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

        let rooms = Arc::clone(&rooms);
        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    warn!(%peer_addr, "WS handshake failed: {e}");
                    return;
                }
            };
            info!(%peer_addr, "WS connected");
            handle_ws(ws, rooms, peer_addr).await;
        });
    }
}

/// Handles a single WebSocket connection.
async fn handle_ws(
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    rooms: Rooms,
    peer_addr: SocketAddr,
) {
    let (mut ws_tx, mut ws_rx) = ws.split();

    // Channel for outbound messages.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Writer task: forwards channel messages to the WebSocket.
    let writer = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if ws_tx.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });

    let mut participant_id: Option<ParticipantId> = None;
    let mut room_id: Option<String> = None;

    // Read the first message — must be a Join.
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

        match &sdp {
            SdpMessage::Join {
                participant,
                room_id: rid,
            } => {
                participant_id = Some(*participant);
                room_id = Some(rid.clone());

                let mut rooms_lock = rooms.lock().await;
                let room = rooms_lock.entry(rid.clone()).or_default();

                // Tell the new peer it's ready (not initiator — it waits for offers).
                let ready_msg = serde_json::json!({
                    "type": "ready",
                    "initiator": false,
                    "participant_count": room.len(),
                });
                let _ = tx.send(ready_msg.to_string());

                // Notify all existing peers that a new peer joined,
                // so they can create offers to it.
                let peer_joined_msg = serde_json::json!({
                    "type": "peer_joined",
                    "participant": participant.to_string(),
                });
                let peer_joined_str = peer_joined_msg.to_string();
                for existing in room.iter() {
                    let _ = existing.tx.send(peer_joined_str.clone());
                }

                room.push(Peer {
                    id: *participant,
                    tx: tx.clone(),
                });

                info!(
                    participant = %participant,
                    room = rid,
                    count = room.len(),
                    "participant joined room"
                );
                continue;
            }
            _ => {
                // Route non-Join messages.
                if let Some(ref rid) = room_id {
                    info!(
                        participant = ?participant_id,
                        room = rid,
                        msg_type = msg_type_label(&sdp),
                        "relaying message"
                    );
                    route_message(&rooms, rid, &sdp, participant_id.as_ref(), &msg).await;
                }
            }
        }
    }

    // Connection closed — clean up.
    if let (Some(pid), Some(rid)) = (participant_id, room_id.as_ref()) {
        info!(participant = %pid, room = rid, "participant disconnected");

        // Broadcast Leave to others.
        let leave = SdpMessage::Leave { participant: pid };
        if let Ok(leave_json) = serde_json::to_string(&leave) {
            route_message(&rooms, rid, &leave, Some(&pid), &leave_json).await;
        }

        // Remove from room.
        let mut rooms_lock = rooms.lock().await;
        if let Some(room) = rooms_lock.get_mut(rid) {
            room.retain(|p| p.id != pid);
            if room.is_empty() {
                rooms_lock.remove(rid);
                info!(room = rid, "room is now empty, removed");
            }
        }
    }

    // Drop the sender so the writer task terminates.
    drop(tx);
    let _ = writer.await;
}

/// Routes a message to the target participant or broadcasts to all others.
async fn route_message(
    rooms: &Rooms,
    room_id: &str,
    sdp: &SdpMessage,
    sender: Option<&ParticipantId>,
    raw_json: &str,
) {
    let target = extract_target(sdp);
    let rooms_lock = rooms.lock().await;
    let Some(room) = rooms_lock.get(room_id) else {
        return;
    };

    match target {
        Some(target_id) => {
            for peer in room {
                if peer.id == target_id {
                    let _ = peer.tx.send(raw_json.to_owned());
                }
            }
        }
        None => {
            for peer in room {
                if sender.map_or(true, |s| peer.id != *s) {
                    let _ = peer.tx.send(raw_json.to_owned());
                }
            }
        }
    }
}

/// Extracts the `to` field to determine routing.
fn extract_target(sdp: &SdpMessage) -> Option<ParticipantId> {
    match sdp {
        SdpMessage::Offer { to, .. } => *to,
        SdpMessage::Answer { to, .. } => Some(*to),
        SdpMessage::IceCandidate { to, .. } => Some(*to),
        SdpMessage::Join { .. } | SdpMessage::Leave { .. } | SdpMessage::Error { .. } => None,
    }
}

/// Returns a short label for logging.
fn msg_type_label(sdp: &SdpMessage) -> &'static str {
    match sdp {
        SdpMessage::Offer { .. } => "offer",
        SdpMessage::Answer { .. } => "answer",
        SdpMessage::IceCandidate { .. } => "ice_candidate",
        SdpMessage::Join { .. } => "join",
        SdpMessage::Leave { .. } => "leave",
        SdpMessage::Error { .. } => "error",
    }
}
