use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use seameet::{
    run_connection, ParticipantId, SdpMessage, SignalingHooks, SignalingState, TransportListener,
    WsListener,
};
use str0m::change::SdpOffer;
use str0m::net::Protocol;
use str0m::bwe::Bitrate;
use str0m::{Candidate, Output, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, warn};

mod sfu;
use sfu::*;

/// Per-room SFU state.
struct RoomState {
    peers: Peers,
}

impl RoomState {
    fn new() -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

type Rooms = Arc<RwLock<HashMap<String, RoomState>>>;

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
    let all_peers: Peers = Arc::new(RwLock::new(HashMap::new()));

    // Spawn UDP reader that dispatches packets using route table.
    tokio::spawn(udp_reader(
        Arc::clone(&shared_socket),
        Arc::clone(&all_peers),
        Arc::clone(&routes),
    ));

    let state = Arc::new(RwLock::new(SignalingState::new()));
    let hooks = Arc::new(SfuHooks {
        rooms: Arc::clone(&rooms),
        all_peers: Arc::clone(&all_peers),
        socket: Arc::clone(&shared_socket),
        routes: Arc::clone(&routes),
        udp_local_addr,
    });

    let mut listener = WsListener::bind("0.0.0.0:3001").await.expect("bind WS");

    info!("WS    → ws://localhost:3001");
    info!("UDP   → 0.0.0.0:{udp_port}");

    while let Some(conn) = listener.accept().await {
        let state = Arc::clone(&state);
        let hooks = Arc::clone(&hooks);
        tokio::spawn(run_connection(conn, state, hooks));
    }
}

// ── SFU hooks ──────────────────────────────────────────────────────────

struct SfuHooks {
    rooms: Rooms,
    all_peers: Peers,
    socket: Arc<UdpSocket>,
    routes: RouteTable,
    udp_local_addr: SocketAddr,
}

/// Per-connection state keyed by participant ID.
struct PeerSession {
    has_media_task: bool,
    current_room_id: Option<String>,
}

type Sessions = Arc<RwLock<HashMap<ParticipantId, PeerSession>>>;

static SESSIONS: std::sync::OnceLock<Sessions> = std::sync::OnceLock::new();

fn sessions() -> &'static Sessions {
    SESSIONS.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

impl SfuHooks {
    /// Returns the shared Peers map for a participant's current room.
    async fn room_peers_for(&self, pid: &ParticipantId) -> Option<Peers> {
        let sess = sessions().read().await;
        let room_id = sess.get(pid)?.current_room_id.as_ref()?.clone();
        drop(sess);

        let rooms = self.rooms.read().await;
        rooms.get(&room_id).map(|r| Arc::clone(&r.peers))
    }
}

impl SignalingHooks for SfuHooks {
    async fn on_message(
        &self,
        sdp: &SdpMessage,
        _raw: &str,
        pid: ParticipantId,
        self_tx: &mpsc::UnboundedSender<String>,
        _state: &Arc<RwLock<SignalingState>>,
    ) -> bool {
        match sdp {
            SdpMessage::Offer { sdp, .. } => {
                let sess = sessions().read().await;
                let has_media = sess.get(&pid).map(|s| s.has_media_task).unwrap_or(false);
                drop(sess);

                let Some(room_peers) = self.room_peers_for(&pid).await else {
                    return true;
                };

                if has_media {
                    // Renegotiation path.
                    let p = room_peers.read().await;
                    if let Some(peer) = p.get(&pid) {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        let _ = peer.cmd_tx.send(PeerCmd::RenegotiationOffer {
                            sdp: sdp.clone(),
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
                                    let _ = self_tx.send(json);
                                }
                            }
                            Ok(_) => {
                                warn!(participant = %pid, "renegotiation produced empty SDP");
                            }
                            Err(_) => {
                                warn!(participant = %pid, "renegotiation reply channel closed");
                            }
                        }
                    }
                    return true;
                }

                // First offer — build Rtc with shared socket address.
                let local_ip = local_ip().await;
                let local_addr: SocketAddr = (local_ip, self.udp_local_addr.port()).into();

                let mut config = RtcConfig::new()
                    .set_rtp_mode(true)
                    .enable_raw_packets(true)
                    .enable_bwe(Some(Bitrate::kbps(800)));
                {
                    let cc = config.codec_config();
                    cc.enable_h264(false);
                    cc.enable_vp9(false);
                }
                let mut rtc = config.build();

                if let Ok(c) = Candidate::host(local_addr, Protocol::Udp) {
                    rtc.add_local_candidate(c);
                }

                if let Ok(public_ip) = std::env::var("PUBLIC_IP") {
                    if let Ok(ip) = public_ip.parse::<std::net::IpAddr>() {
                        let public_addr: SocketAddr = (ip, self.udp_local_addr.port()).into();
                        if let Ok(c) = Candidate::host(public_addr, Protocol::Udp) {
                            rtc.add_local_candidate(c);
                            info!(%public_addr, "added public IP candidate");
                        }
                    }
                }

                let sdp = patch_sdp_directions(sdp);

                let offer = match SdpOffer::from_sdp_string(&sdp) {
                    Ok(o) => o,
                    Err(e) => {
                        warn!("bad SDP offer: {e}");
                        return true;
                    }
                };

                let answer = match rtc.sdp_api().accept_offer(offer) {
                    Ok(a) => a,
                    Err(e) => {
                        warn!("accept_offer failed: {e}");
                        return true;
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

                let initial_mids = parse_mids_from_sdp(&answer_sdp);
                info!(participant = %pid, count = initial_mids.len(), "parsed mids from SDP answer");
                for (mid, kind) in &initial_mids {
                    info!(participant = %pid, ?mid, ?kind, "SDP mid");
                }

                // Drain pending Transmit events (DTLS handshake packets).
                loop {
                    match rtc.poll_output() {
                        Ok(Output::Transmit(t)) => {
                            let _ = self.socket.try_send_to(&t.contents, t.destination);
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
                    let _ = self_tx.send(json);
                }

                info!(participant = %pid, %local_addr, "SDP negotiated");

                let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
                let sfu_peer = SfuPeer {
                    cmd_tx,
                    ws_tx: self_tx.clone(),
                };
                {
                    let mut p = room_peers.write().await;
                    p.insert(pid, SfuPeer {
                        cmd_tx: sfu_peer.cmd_tx.clone(),
                        ws_tx: sfu_peer.ws_tx.clone(),
                    });
                }
                {
                    let mut ap = self.all_peers.write().await;
                    ap.insert(pid, sfu_peer);
                }

                {
                    let mut sess = sessions().write().await;
                    if let Some(s) = sess.get_mut(&pid) {
                        s.has_media_task = true;
                    }
                }

                let peers_clone = Arc::clone(&room_peers);
                let socket_clone = Arc::clone(&self.socket);
                let routes_clone = Arc::clone(&self.routes);
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

                true
            }

            SdpMessage::IceCandidate { candidate, .. } => {
                let ap = self.all_peers.read().await;
                if let Some(peer) = ap.get(&pid) {
                    let _ = peer.cmd_tx.send(PeerCmd::IceCandidate(candidate.clone()));
                }
                true
            }

            SdpMessage::ScreenShareStarted { from, room_id, track_id } => {
                if let Some(room_peers) = self.room_peers_for(&pid).await {
                    let p = room_peers.read().await;
                    if let Some(peer) = p.get(from) {
                        let _ = peer.cmd_tx.send(PeerCmd::ScreenShareActive(true));
                    }
                    let msg = SdpMessage::ScreenShareStarted { from: *from, room_id: room_id.clone(), track_id: *track_id };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        for (id, peer) in p.iter() {
                            if *id != pid {
                                let _ = peer.ws_tx.send(json.clone());
                            }
                        }
                    }
                }
                true
            }

            SdpMessage::ScreenShareStopped { from, room_id, track_id } => {
                if let Some(room_peers) = self.room_peers_for(&pid).await {
                    let p = room_peers.read().await;
                    if let Some(peer) = p.get(from) {
                        let _ = peer.cmd_tx.send(PeerCmd::ScreenShareActive(false));
                    }
                    let msg = SdpMessage::ScreenShareStopped { from: *from, room_id: room_id.clone(), track_id: *track_id };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        for (id, peer) in p.iter() {
                            if *id != pid {
                                let _ = peer.ws_tx.send(json.clone());
                            }
                        }
                    }
                }
                true
            }

            SdpMessage::Join { room_id, .. } => {
                // Get or create the shared room state for this room_id.
                {
                    let mut r = self.rooms.write().await;
                    r.entry(room_id.clone()).or_insert_with(RoomState::new);
                }

                // Register the participant's session with their room_id.
                {
                    let mut sess = sessions().write().await;
                    sess.insert(pid, PeerSession {
                        has_media_task: false,
                        current_room_id: Some(room_id.clone()),
                    });
                }

                // Let the engine handle the Join (room membership + Ready/PeerJoined).
                false
            }

            // Let the engine handle Leave, PeerLeft, etc.
            _ => false,
        }
    }

    async fn on_disconnect(
        &self,
        pid: ParticipantId,
        affected_rooms: &[(String, bool)],
        _state: &Arc<RwLock<SignalingState>>,
    ) {
        // Clean up route table entries.
        {
            let mut r = self.routes.write().await;
            r.retain(|_, v| *v != pid);
        }
        // Remove from all_peers.
        self.all_peers.write().await.remove(&pid);

        // Clean up per-room SFU peer state.
        for (room_id, room_empty) in affected_rooms {
            let room_peers = {
                let rooms = self.rooms.read().await;
                rooms.get(room_id).map(|r| Arc::clone(&r.peers))
            };
            if let Some(room_peers) = room_peers {
                room_peers.write().await.remove(&pid);
            }
            if *room_empty {
                self.rooms.write().await.remove(room_id);
            }
        }

        // Remove session.
        sessions().write().await.remove(&pid);
    }
}
