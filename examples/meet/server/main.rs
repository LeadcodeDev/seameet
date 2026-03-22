use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use seameet::{ParticipantId, SdpMessage};
use str0m::change::SdpOffer;
use str0m::net::Protocol;
use str0m::{Candidate, Output, RtcConfig};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

mod sfu;
use sfu::*;

// Per-room state: peers and display names.
type DisplayNames = Arc<RwLock<HashMap<ParticipantId, String>>>;

struct RoomState {
    peers: Peers,
    display_names: DisplayNames,
}

type Rooms = Arc<RwLock<HashMap<String, RoomState>>>;

impl RoomState {
    fn new() -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
            display_names: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "meet=debug,str0m=warn,str0m::rtp_=error".parse().expect("filter")),
        )
        .init();

    // Single shared UDP socket for all peers.
    let udp_port: u16 = std::env::var("UDP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10000);
    let shared_socket = Arc::new(
        UdpSocket::bind(format!("0.0.0.0:{udp_port}"))
            .await
            .expect("bind shared UDP socket"),
    );
    let udp_local_addr = shared_socket.local_addr().expect("UDP local addr");

    let rooms: Rooms = Arc::new(RwLock::new(HashMap::new()));
    let routes: RouteTable = Arc::new(RwLock::new(HashMap::new()));
    // Global peers map for UDP routing (all rooms share the UDP socket).
    let all_peers: Peers = Arc::new(RwLock::new(HashMap::new()));

    // Spawn UDP reader that dispatches packets using route table.
    tokio::spawn(udp_reader(
        Arc::clone(&shared_socket),
        Arc::clone(&all_peers),
        Arc::clone(&routes),
    ));

    let ws = tokio::spawn(serve_ws(
        rooms,
        all_peers,
        Arc::clone(&shared_socket),
        udp_local_addr,
        Arc::clone(&routes),
    ));

    info!("WS    → ws://localhost:3001");
    info!("UDP   → 0.0.0.0:{udp_port}");

    let _ = ws.await;
}

// ── WebSocket signaling ─────────────────────────────────────────────────

async fn serve_ws(
    rooms: Rooms,
    all_peers: Peers,
    socket: Arc<UdpSocket>,
    udp_local_addr: SocketAddr,
    routes: RouteTable,
) {
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
        let all_peers = Arc::clone(&all_peers);
        let socket = Arc::clone(&socket);
        let routes = Arc::clone(&routes);
        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    warn!(%peer_addr, "WS handshake failed: {e}");
                    return;
                }
            };
            info!(%peer_addr, "WS connected");
            handle_ws(ws, rooms, all_peers, socket, udp_local_addr, routes).await;
        });
    }
}

async fn handle_ws(
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    rooms: Rooms,
    all_peers: Peers,
    socket: Arc<UdpSocket>,
    udp_local_addr: SocketAddr,
    routes: RouteTable,
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
    let mut current_room_id: Option<String> = None;
    let mut has_media_task = false;

    while let Some(result) = ws_rx.next().await {
        let msg = match result {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(e) => {
                warn!("WS read error: {e}");
                break;
            }
        };

        // Parse as raw JSON Value first to extract display_name.
        let raw: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(e) => {
                warn!("invalid JSON: {e}");
                continue;
            }
        };

        let sdp: SdpMessage = match serde_json::from_str(&msg) {
            Ok(m) => m,
            Err(e) => {
                warn!("invalid message: {e}");
                continue;
            }
        };

        match sdp {
            SdpMessage::Join { participant, room_id, .. } => {
                participant_id = Some(participant);
                current_room_id = Some(room_id.clone());

                // Extract and store display_name if present.
                let display_name = raw
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Get or create room state.
                let room = {
                    let mut r = rooms.write().await;
                    let room = r.entry(room_id.clone()).or_insert_with(RoomState::new);
                    (Arc::clone(&room.peers), Arc::clone(&room.display_names))
                };
                let (room_peers, room_names) = room;

                if !display_name.is_empty() {
                    room_names.write().await.insert(participant, display_name.clone());
                }

                let existing_peers: Vec<ParticipantId>;
                {
                    let p = room_peers.read().await;
                    existing_peers = p.keys().copied().collect();

                    // Build PeerJoined with display_name.
                    let joined_msg = SdpMessage::PeerJoined {
                        participant,
                        room_id: room_id.clone(),
                    };
                    if let Ok(json) = serde_json::to_string(&joined_msg) {
                        let mut val: serde_json::Value = serde_json::from_str(&json).unwrap();
                        if !display_name.is_empty() {
                            val["display_name"] = serde_json::Value::String(display_name.clone());
                        }
                        let json = serde_json::to_string(&val).unwrap();
                        for peer in p.values() {
                            let _ = peer.ws_tx.send(json.clone());
                        }
                    }
                }

                // Build Ready with peer display names.
                let ready = SdpMessage::Ready {
                    room_id: room_id.clone(),
                    initiator: true,
                    peers: existing_peers.clone(),
                };
                if let Ok(json) = serde_json::to_string(&ready) {
                    let mut val: serde_json::Value = serde_json::from_str(&json).unwrap();
                    let names = room_names.read().await;
                    let mut peer_names = serde_json::Map::new();
                    for pid in &existing_peers {
                        if let Some(name) = names.get(pid) {
                            peer_names.insert(pid.to_string(), serde_json::Value::String(name.clone()));
                        }
                    }
                    if !peer_names.is_empty() {
                        val["display_names"] = serde_json::Value::Object(peer_names);
                    }
                    let json = serde_json::to_string(&val).unwrap();
                    let _ = tx.send(json);
                }
                info!(participant = %participant, existing = existing_peers.len(), "joined room {room_id}");
            }

            SdpMessage::Offer { sdp, .. } => {
                let Some(pid) = participant_id else { continue };
                let Some(ref rid) = current_room_id else { continue };
                info!(participant = %pid, "processing offer");

                // Get room peers for this participant.
                let room_peers = {
                    let r = rooms.read().await;
                    r.get(rid).map(|room| Arc::clone(&room.peers))
                };
                let Some(room_peers) = room_peers else { continue };

                if has_media_task {
                    let p = room_peers.read().await;
                    if let Some(peer) = p.get(&pid) {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        let _ = peer.cmd_tx.send(PeerCmd::RenegotiationOffer {
                            sdp,
                            reply_tx,
                        });
                        drop(p);

                        match reply_rx.await {
                            Ok(answer_sdp) if !answer_sdp.is_empty() => {
                                let answer_msg = SdpMessage::Answer {
                                    from: pid,
                                    to: pid,
                                    room_id: "sfu".into(),
                                    sdp: answer_sdp,
                                };
                                if let Ok(json) = serde_json::to_string(&answer_msg) {
                                    let _ = tx.send(json);
                                }
                            }
                            Ok(_) => {
                                warn!(participant = %pid, "renegotiation produced empty SDP — not sending answer");
                            }
                            Err(_) => {
                                warn!(participant = %pid, "renegotiation reply channel closed");
                            }
                        }
                    }
                    continue;
                }

                // First offer — build Rtc with shared socket address.
                let local_ip = local_ip().await;
                let local_addr: SocketAddr = (local_ip, udp_local_addr.port()).into();

                let mut config = RtcConfig::new()
                    .set_rtp_mode(true)
                    .enable_raw_packets(true);
                {
                    let cc = config.codec_config();
                    cc.enable_h264(false);
                    cc.enable_vp9(false);
                }
                let mut rtc = config.build();

                if let Ok(c) = Candidate::host(local_addr, Protocol::Udp) {
                    rtc.add_local_candidate(c);
                }

                // Add public IP candidate for remote clients.
                if let Ok(public_ip) = std::env::var("PUBLIC_IP") {
                    if let Ok(ip) = public_ip.parse::<std::net::IpAddr>() {
                        let public_addr: SocketAddr = (ip, udp_local_addr.port()).into();
                        if let Ok(c) = Candidate::host(public_addr, Protocol::Udp) {
                            rtc.add_local_candidate(c);
                            info!(%public_addr, "added public IP candidate");
                        }
                    }
                }

                let sdp = patch_sdp_directions(&sdp);

                let offer = match SdpOffer::from_sdp_string(&sdp) {
                    Ok(o) => o,
                    Err(e) => {
                        warn!("bad SDP offer: {e}");
                        continue;
                    }
                };

                let answer = match rtc.sdp_api().accept_offer(offer) {
                    Ok(a) => a,
                    Err(e) => {
                        warn!("accept_offer failed: {e}");
                        continue;
                    }
                };

                let answer_sdp = answer.to_sdp_string();
                for line in answer_sdp.lines() {
                    if line.starts_with("m=")
                        || line.starts_with("a=sendrecv")
                        || line.starts_with("a=recvonly")
                        || line.starts_with("a=sendonly")
                        || line.starts_with("a=rtpmap")
                    {
                        debug!("SDP answer: {line}");
                    }
                }

                // Parse mids directly from the SDP answer.  str0m's MediaAdded
                // events only fire after DTLS completes (Event::Connected), which
                // is too late — ForwardedMedia from other peers floods the channel
                // before that.  Parsing the SDP gives us mids immediately.
                let initial_mids = parse_mids_from_sdp(&answer_sdp);
                info!(participant = %pid, count = initial_mids.len(), "parsed mids from SDP answer");
                for (mid, kind) in &initial_mids {
                    info!(participant = %pid, ?mid, ?kind, "SDP mid");
                }

                // Still drain any pending Transmit events (DTLS handshake packets).
                loop {
                    match rtc.poll_output() {
                        Ok(Output::Transmit(t)) => {
                            let _ = socket.try_send_to(&t.contents, t.destination);
                        }
                        Ok(Output::Event(_)) => {}
                        Ok(Output::Timeout(_)) => break,
                        Err(_) => break,
                    }
                }

                let (own_audio_pt, own_video_pt) = parse_pts_from_sdp(&answer_sdp);
                info!(participant = %pid, ?own_audio_pt, ?own_video_pt, "negotiated PTs");

                let answer_msg = SdpMessage::Answer {
                    from: pid,
                    to: pid,
                    room_id: "sfu".into(),
                    sdp: answer_sdp,
                };
                if let Ok(json) = serde_json::to_string(&answer_msg) {
                    let _ = tx.send(json);
                }

                info!(participant = %pid, %local_addr, "SDP negotiated");

                let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
                let sfu_peer = SfuPeer {
                    cmd_tx,
                    ws_tx: tx.clone(),
                };
                {
                    let mut p = room_peers.write().await;
                    p.insert(pid, SfuPeer {
                        cmd_tx: sfu_peer.cmd_tx.clone(),
                        ws_tx: sfu_peer.ws_tx.clone(),
                    });
                }
                // Also register in all_peers for UDP routing.
                {
                    let mut ap = all_peers.write().await;
                    ap.insert(pid, sfu_peer);
                }
                has_media_task = true;

                let peers_clone = Arc::clone(&room_peers);
                let socket_clone = Arc::clone(&socket);
                let routes_clone = Arc::clone(&routes);
                tokio::spawn(async move {
                    run_media(
                        rtc,
                        socket_clone,
                        local_addr,
                        pid,
                        cmd_rx,
                        peers_clone,
                        routes_clone,
                        own_audio_pt.unwrap_or(111),
                        own_video_pt.unwrap_or(96),
                        initial_mids,
                    )
                    .await;
                });
            }

            SdpMessage::IceCandidate { candidate, .. } => {
                let Some(pid) = participant_id else { continue };
                let ap = all_peers.read().await;
                if let Some(peer) = ap.get(&pid) {
                    let _ = peer.cmd_tx.send(PeerCmd::IceCandidate(candidate));
                }
            }

            SdpMessage::ScreenShareStarted { from, room_id, track_id } => {
                let room_peers = {
                    let r = rooms.read().await;
                    r.get(&room_id).map(|room| Arc::clone(&room.peers))
                };
                if let Some(room_peers) = room_peers {
                    let p = room_peers.read().await;
                    if let Some(peer) = p.get(&from) {
                        let _ = peer.cmd_tx.send(PeerCmd::ScreenShareActive(true));
                    }
                    let msg = SdpMessage::ScreenShareStarted { from, room_id, track_id };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        for (id, peer) in p.iter() {
                            if Some(*id) != participant_id {
                                let _ = peer.ws_tx.send(json.clone());
                            }
                        }
                    }
                }
            }

            SdpMessage::ScreenShareStopped { from, room_id, track_id } => {
                let room_peers = {
                    let r = rooms.read().await;
                    r.get(&room_id).map(|room| Arc::clone(&room.peers))
                };
                if let Some(room_peers) = room_peers {
                    let p = room_peers.read().await;
                    if let Some(peer) = p.get(&from) {
                        let _ = peer.cmd_tx.send(PeerCmd::ScreenShareActive(false));
                    }
                    let msg = SdpMessage::ScreenShareStopped { from, room_id, track_id };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        for (id, peer) in p.iter() {
                            if Some(*id) != participant_id {
                                let _ = peer.ws_tx.send(json.clone());
                            }
                        }
                    }
                }
            }

            _ => {}
        }
    }

    if let Some(pid) = participant_id {
        info!(participant = %pid, "disconnected");
        // Clean up route table entries for this peer.
        {
            let mut r = routes.write().await;
            r.retain(|_, v| *v != pid);
        }
        // Remove from all_peers (UDP routing).
        all_peers.write().await.remove(&pid);

        // Clean up room state.
        if let Some(ref rid) = current_room_id {
            let room = {
                let r = rooms.read().await;
                r.get(rid).map(|room| (Arc::clone(&room.peers), Arc::clone(&room.display_names)))
            };
            if let Some((room_peers, room_names)) = room {
                room_names.write().await.remove(&pid);

                let mut p = room_peers.write().await;
                p.remove(&pid);

                let left_msg = SdpMessage::PeerLeft {
                    participant: pid,
                    room_id: rid.clone(),
                };
                if let Ok(json) = serde_json::to_string(&left_msg) {
                    for peer in p.values() {
                        let _ = peer.ws_tx.send(json.clone());
                    }
                }

                // Clean up empty rooms.
                if p.is_empty() {
                    drop(p);
                    rooms.write().await.remove(rid);
                }
            }
        }
    }

    drop(tx);
    let _ = writer.await;
}
