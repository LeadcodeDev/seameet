use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use axum::response::Html;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use seameet::{ParticipantId, SdpMessage};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

/// Per-participant outbound channel.
type WsSink = mpsc::UnboundedSender<String>;

/// Shared signaling state with double index (rooms ↔ participants).
struct SignalingState {
    rooms: HashMap<String, Vec<ParticipantId>>,
    participants: HashMap<ParticipantId, (WsSink, HashSet<String>)>,
}

impl SignalingState {
    fn new() -> Self {
        Self {
            rooms: HashMap::new(),
            participants: HashMap::new(),
        }
    }

    fn join(&mut self, participant: ParticipantId, room_id: &str, sink: WsSink) -> bool {
        let room = self.rooms.entry(room_id.to_owned()).or_default();
        let is_first = room.is_empty();
        if !room.contains(&participant) {
            room.push(participant);
        }
        let entry = self
            .participants
            .entry(participant)
            .or_insert_with(|| (sink, HashSet::new()));
        entry.1.insert(room_id.to_owned());
        is_first
    }

    fn leave(&mut self, participant: &ParticipantId, room_id: &str) -> bool {
        let room_empty = if let Some(room) = self.rooms.get_mut(room_id) {
            room.retain(|id| id != participant);
            if room.is_empty() {
                self.rooms.remove(room_id);
                true
            } else {
                false
            }
        } else {
            true
        };
        if let Some((_, rooms)) = self.participants.get_mut(participant) {
            rooms.remove(room_id);
            if rooms.is_empty() {
                self.participants.remove(participant);
            }
        }
        room_empty
    }

    fn disconnect(&mut self, participant: &ParticipantId) -> Vec<(String, bool)> {
        let room_ids: Vec<String> = self
            .participants
            .get(participant)
            .map(|(_, rooms)| rooms.iter().cloned().collect())
            .unwrap_or_default();
        let mut affected = Vec::with_capacity(room_ids.len());
        for rid in room_ids {
            let empty = self.leave(participant, &rid);
            affected.push((rid, empty));
        }
        self.participants.remove(participant);
        affected
    }

    fn peers(&self, room_id: &str, exclude: &ParticipantId) -> Vec<WsSink> {
        let Some(room) = self.rooms.get(room_id) else {
            return Vec::new();
        };
        room.iter()
            .filter(|id| *id != exclude)
            .filter_map(|id| self.participants.get(id).map(|(tx, _)| tx.clone()))
            .collect()
    }

    fn sink(&self, participant: &ParticipantId) -> Option<WsSink> {
        self.participants.get(participant).map(|(tx, _)| tx.clone())
    }
}

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

        // First message must be Join to identify the participant.
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

        match &sdp {
            SdpMessage::Join {
                participant,
                room_id,
            } => {
                let mut st = state.lock().await;
                let initiator = st.join(*participant, room_id, tx.clone());

                let ready = SdpMessage::Ready {
                    room_id: room_id.clone(),
                    initiator,
                };
                if let Ok(json) = serde_json::to_string(&ready) {
                    let _ = tx.send(json);
                }

                if !initiator {
                    let peers = st.peers(room_id, participant);
                    drop(st);
                    if let Ok(json) = serde_json::to_string(&sdp) {
                        for peer_tx in peers {
                            let _ = peer_tx.send(json.clone());
                        }
                    }
                }

                info!(participant = %participant, room = room_id, "joined");
            }
            SdpMessage::Leave {
                participant,
                room_id,
            } => {
                let mut st = state.lock().await;
                let peers = st.peers(room_id, participant);
                st.leave(participant, room_id);
                drop(st);

                if let Ok(json) = serde_json::to_string(&sdp) {
                    for peer_tx in peers {
                        let _ = peer_tx.send(json.clone());
                    }
                }
                info!(participant = %participant, room = room_id, "left");
            }
            SdpMessage::Offer { room_id, to, .. } => {
                let st = state.lock().await;
                if let Some(target) = to {
                    if let Some(peer_tx) = st.sink(target) {
                        let _ = peer_tx.send(msg.clone());
                    }
                } else {
                    let peers = st.peers(room_id, &pid);
                    drop(st);
                    for peer_tx in peers {
                        let _ = peer_tx.send(msg.clone());
                    }
                }
            }
            SdpMessage::Answer { to, .. } => {
                let st = state.lock().await;
                if let Some(peer_tx) = st.sink(to) {
                    let _ = peer_tx.send(msg.clone());
                }
            }
            SdpMessage::IceCandidate { to, .. } => {
                let st = state.lock().await;
                if let Some(peer_tx) = st.sink(to) {
                    let _ = peer_tx.send(msg.clone());
                }
            }
            _ => {}
        }
    }

    // Connection closed — clean up all rooms.
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
