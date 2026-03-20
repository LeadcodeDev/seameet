use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use seameet_core::{ParticipantId, SeaMeetError};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, warn};

use crate::message::SdpMessage;

/// Entry for a connected peer: its identifier and a channel to send it messages.
#[derive(Debug)]
struct PeerHandle {
    id: ParticipantId,
    tx: mpsc::UnboundedSender<String>,
}

type Rooms = Arc<Mutex<HashMap<String, Vec<PeerHandle>>>>;

/// In-memory WebSocket signaling server used for testing and local development.
///
/// Routes [`SdpMessage`]s between participants within named rooms.
pub struct RoomServer {
    listener: TcpListener,
    rooms: Rooms,
}

impl RoomServer {
    /// Binds the server to the given address (e.g. `"127.0.0.1:0"`).
    pub async fn bind(addr: &str) -> Result<Self, SeaMeetError> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            listener,
            rooms: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Returns the local address the server is listening on.
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, SeaMeetError> {
        self.listener.local_addr().map_err(SeaMeetError::Io)
    }

    /// Starts accepting connections. Returns a [`JoinHandle`] for the server task.
    pub fn run(self) -> JoinHandle<()> {
        let rooms = self.rooms;
        let listener = self.listener;
        tokio::spawn(async move {
            loop {
                let (stream, addr) = match listener.accept().await {
                    Ok(v) => v,
                    Err(e) => {
                        error!("accept error: {e}");
                        continue;
                    }
                };
                debug!(%addr, "new TCP connection");
                let rooms = Arc::clone(&rooms);
                tokio::spawn(async move {
                    let ws = match tokio_tungstenite::accept_async(stream).await {
                        Ok(ws) => ws,
                        Err(e) => {
                            warn!(%addr, "WebSocket handshake failed: {e}");
                            return;
                        }
                    };
                    Self::handle_connection(ws, rooms).await;
                });
            }
        })
    }

    /// Handles a single WebSocket connection lifecycle.
    async fn handle_connection(
        ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        rooms: Rooms,
    ) {
        let (mut sink, mut stream) = ws.split();

        // Channel for outbound messages to this peer.
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        // Spawn a writer task that forwards channel messages to the WebSocket sink.
        let writer = tokio::spawn(async move {
            while let Some(text) = rx.recv().await {
                if sink.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
        });

        let mut participant_id: Option<ParticipantId> = None;
        let mut room_id: Option<String> = None;

        while let Some(result) = stream.next().await {
            let msg = match result {
                Ok(m) => m,
                Err(_) => break,
            };

            match msg {
                Message::Text(text) => {
                    let sdp: SdpMessage = match serde_json::from_str(&text) {
                        Ok(m) => m,
                        Err(e) => {
                            warn!("invalid message: {e}");
                            continue;
                        }
                    };

                    // Handle Join: register peer in the room.
                    if let SdpMessage::Join {
                        participant,
                        room_id: rid,
                    } = &sdp
                    {
                        participant_id = Some(*participant);
                        room_id = Some(rid.clone());
                        let mut rooms_lock = rooms.lock().await;
                        rooms_lock.entry(rid.clone()).or_default().push(PeerHandle {
                            id: *participant,
                            tx: tx.clone(),
                        });
                        debug!(participant = %participant, room = rid, "participant joined");
                        continue;
                    }

                    // Route the message to peers in the same room.
                    if let Some(ref rid) = room_id {
                        Self::route_message(&rooms, rid, &sdp, participant_id).await;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }

        // Connection closed — emit Leave and clean up.
        if let (Some(pid), Some(ref rid)) = (participant_id, &room_id) {
            debug!(participant = %pid, room = rid, "participant disconnected");
            let leave = SdpMessage::Leave { participant: pid };
            Self::route_message(&rooms, rid, &leave, Some(pid)).await;

            let mut rooms_lock = rooms.lock().await;
            if let Some(room) = rooms_lock.get_mut(rid) {
                room.retain(|p| p.id != pid);
                if room.is_empty() {
                    rooms_lock.remove(rid);
                }
            }
        }

        // Drop the sender so the writer task terminates.
        drop(tx);
        let _ = writer.await;
    }

    /// Routes a message to the target participant (`to` field) or broadcasts
    /// to all others in the room.
    async fn route_message(
        rooms: &Rooms,
        room_id: &str,
        sdp: &SdpMessage,
        sender: Option<ParticipantId>,
    ) {
        let target = Self::extract_target(sdp);
        let json = match serde_json::to_string(sdp) {
            Ok(j) => j,
            Err(e) => {
                warn!("failed to serialize message for routing: {e}");
                return;
            }
        };

        let rooms_lock = rooms.lock().await;
        let Some(room) = rooms_lock.get(room_id) else {
            return;
        };

        match target {
            Some(target_id) => {
                // Unicast to a specific participant.
                for peer in room {
                    if peer.id == target_id {
                        let _ = peer.tx.send(json.clone());
                    }
                }
            }
            None => {
                // Broadcast to everyone except the sender.
                for peer in room {
                    if Some(peer.id) != sender {
                        let _ = peer.tx.send(json.clone());
                    }
                }
            }
        }
    }

    /// Extracts the `to` field from a message to determine routing.
    fn extract_target(sdp: &SdpMessage) -> Option<ParticipantId> {
        match sdp {
            SdpMessage::Offer { to, .. } => *to,
            SdpMessage::Answer { to, .. } => Some(*to),
            SdpMessage::IceCandidate { to, .. } => Some(*to),
            SdpMessage::Join { .. } | SdpMessage::Leave { .. } | SdpMessage::Error { .. } => None,
        }
    }
}
