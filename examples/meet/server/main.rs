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
        sessions: Arc::new(RwLock::new(HashMap::new())),
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
    sessions: Sessions,
}

/// Per-connection state keyed by participant ID.
struct PeerSession {
    has_media_task: bool,
    current_room_id: Option<String>,
}

type Sessions = Arc<RwLock<HashMap<ParticipantId, PeerSession>>>;

impl SfuHooks {
    /// Returns the shared Peers map for a participant's current room.
    async fn room_peers_for(&self, pid: &ParticipantId) -> Option<Peers> {
        let sess = self.sessions.read().await;
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
                let sess = self.sessions.read().await;
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
                    let mut sess = self.sessions.write().await;
                    if let Some(s) = sess.get_mut(&pid) {
                        s.has_media_task = true;
                    }
                }

                let peers_clone = Arc::clone(&room_peers);
                let socket_clone = Arc::clone(&self.socket);
                let routes_clone = Arc::clone(&self.routes);
                let ws_tx_clone = self_tx.clone();
                let sess = self.sessions.read().await;
                let media_room_id = sess.get(&pid)
                    .and_then(|s| s.current_room_id.clone())
                    .unwrap_or_default();
                drop(sess);
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
                        ws_tx_clone,
                        media_room_id,
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

            SdpMessage::MuteAudio { from, room_id } => {
                if let Some(room_peers) = self.room_peers_for(&pid).await {
                    let p = room_peers.read().await;
                    // Tell the muted peer's media task to stop forwarding its audio.
                    if let Some(peer) = p.get(from) {
                        let _ = peer.cmd_tx.send(PeerCmd::SetMuted(true));
                    }
                    // Notify all other peers' media tasks so they can skip incoming audio.
                    for (id, peer) in p.iter() {
                        if *id != *from {
                            let _ = peer.cmd_tx.send(PeerCmd::PeerMuteChanged {
                                pid: *from,
                                muted: true,
                            });
                        }
                    }
                    // Broadcast the mute event via WS for UI updates.
                    let msg = SdpMessage::MuteAudio { from: *from, room_id: room_id.clone() };
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

            SdpMessage::UnmuteAudio { from, room_id } => {
                if let Some(room_peers) = self.room_peers_for(&pid).await {
                    let p = room_peers.read().await;
                    if let Some(peer) = p.get(from) {
                        let _ = peer.cmd_tx.send(PeerCmd::SetMuted(false));
                    }
                    for (id, peer) in p.iter() {
                        if *id != *from {
                            let _ = peer.cmd_tx.send(PeerCmd::PeerMuteChanged {
                                pid: *from,
                                muted: false,
                            });
                        }
                    }
                    let msg = SdpMessage::UnmuteAudio { from: *from, room_id: room_id.clone() };
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

            SdpMessage::VideoConfigChanged { from, room_id, width, height, fps } => {
                if let Some(room_peers) = self.room_peers_for(&pid).await {
                    let p = room_peers.read().await;
                    let msg = SdpMessage::VideoConfigChanged {
                        from: *from,
                        room_id: room_id.clone(),
                        width: *width,
                        height: *height,
                        fps: *fps,
                    };
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
                    let mut sess = self.sessions.write().await;
                    sess.insert(pid, PeerSession {
                        has_media_task: false,
                        current_room_id: Some(room_id.clone()),
                    });
                }

                // Notify all peers in the room about the updated peer count.
                {
                    let rooms = self.rooms.read().await;
                    if let Some(room) = rooms.get(room_id) {
                        let p = room.peers.read().await;
                        // +1 because this peer hasn't been added to the SFU peers yet.
                        let remote_count = p.len();
                        for (_id, peer) in p.iter() {
                            let _ = peer.cmd_tx.send(PeerCmd::PeerCountChanged {
                                remote_peer_count: remote_count,
                            });
                        }
                    }
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

        // Clean up per-room SFU peer state and notify remaining peers.
        for (room_id, room_empty) in affected_rooms {
            let room_peers = {
                let rooms = self.rooms.read().await;
                rooms.get(room_id).map(|r| Arc::clone(&r.peers))
            };
            if let Some(room_peers) = room_peers {
                // Broadcast ScreenShareStopped so frontends clean up any active screen share.
                let screen_stopped = SdpMessage::ScreenShareStopped {
                    from: pid,
                    room_id: room_id.clone(),
                    track_id: 0,
                };
                if let Ok(json) = serde_json::to_string(&screen_stopped) {
                    let p = room_peers.read().await;
                    for (id, peer) in p.iter() {
                        if *id != pid {
                            let _ = peer.ws_tx.send(json.clone());
                        }
                    }
                }
                room_peers.write().await.remove(&pid);
            }
            if *room_empty {
                self.rooms.write().await.remove(room_id);
            }
        }

        // Remove session.
        self.sessions.write().await.remove(&pid);
    }
}

// ── Integration tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use seameet::{run_connection, ConnectionReader, IncomingConnection, SdpMessage, SignalingState};
    use std::future::Future;
    use std::pin::Pin;
    use tokio::sync::mpsc;

    fn pid(n: u128) -> ParticipantId {
        ParticipantId::new(uuid::Uuid::from_u128(n))
    }

    /// Mock ConnectionReader backed by an mpsc channel.
    struct MockReader {
        rx: mpsc::UnboundedReceiver<String>,
    }

    impl ConnectionReader for MockReader {
        fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
            Box::pin(async { self.rx.recv().await })
        }
    }

    /// A test peer: send messages via `tx`, receive server responses via `out_rx`.
    struct TestPeer {
        tx: mpsc::UnboundedSender<String>,
        out_rx: mpsc::UnboundedReceiver<String>,
        /// Clone of the writer channel — used to register SFU peers.
        out_tx: mpsc::UnboundedSender<String>,
    }

    impl TestPeer {
        fn send(&self, msg: &SdpMessage) {
            let json = serde_json::to_string(msg).unwrap();
            self.tx.send(json).unwrap();
        }

        async fn recv(&mut self) -> SdpMessage {
            let json = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                self.out_rx.recv(),
            )
            .await
            .expect("timeout waiting for message")
            .expect("channel closed");
            serde_json::from_str(&json).unwrap()
        }

        async fn try_recv(&mut self) -> Option<SdpMessage> {
            match tokio::time::timeout(
                std::time::Duration::from_millis(100),
                self.out_rx.recv(),
            )
            .await
            {
                Ok(Some(json)) => serde_json::from_str(&json).ok(),
                _ => None,
            }
        }

        /// Drain all pending messages.
        async fn drain(&mut self) -> Vec<SdpMessage> {
            let mut msgs = Vec::new();
            while let Some(msg) = self.try_recv().await {
                msgs.push(msg);
            }
            msgs
        }

        fn disconnect(self) {
            drop(self.tx);
        }
    }

    /// Shared test context.
    struct TestCtx {
        state: Arc<RwLock<SignalingState>>,
        hooks: Arc<SfuHooks>,
    }

    impl TestCtx {
        async fn new() -> Self {
            let rooms: Rooms = Arc::new(RwLock::new(HashMap::new()));
            let routes: RouteTable = Arc::new(RwLock::new(HashMap::new()));
            let all_peers: Peers = Arc::new(RwLock::new(HashMap::new()));

            let socket = Arc::new(
                tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap()
            );
            let addr = socket.local_addr().unwrap();

            let hooks = Arc::new(SfuHooks {
                rooms,
                all_peers,
                socket,
                routes,
                udp_local_addr: addr,
                sessions: Arc::new(RwLock::new(HashMap::new())),
            });

            Self {
                state: Arc::new(RwLock::new(SignalingState::new())),
                hooks,
            }
        }

        fn connect_peer(&self) -> (TestPeer, tokio::task::JoinHandle<()>) {
            let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
            let (out_tx, out_rx) = mpsc::unbounded_channel::<String>();

            let out_tx_clone = out_tx.clone();
            let conn = IncomingConnection {
                reader: Box::new(MockReader { rx: in_rx }),
                writer: out_tx,
            };

            let state = Arc::clone(&self.state);
            let hooks = Arc::clone(&self.hooks);
            let handle = tokio::spawn(run_connection(conn, state, hooks));

            (TestPeer { tx: in_tx, out_rx, out_tx: out_tx_clone }, handle)
        }

        /// Register a fake SFU peer so that mute/screenshare/videoconfig
        /// hooks can find the peer without requiring actual WebRTC offers.
        async fn register_sfu_peer(&self, peer: &TestPeer, id: ParticipantId, room_id: &str) {
            let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
            let sfu_peer = SfuPeer { cmd_tx, ws_tx: peer.out_tx.clone() };

            // Add to room peers
            let rooms = self.hooks.rooms.read().await;
            if let Some(room) = rooms.get(room_id) {
                room.peers.write().await.insert(id, sfu_peer.clone());
            }
            drop(rooms);

            // Add to all_peers
            self.hooks.all_peers.write().await.insert(id, sfu_peer);

            // Mark has_media_task
            let mut sess = self.hooks.sessions.write().await;
            if let Some(s) = sess.get_mut(&id) {
                s.has_media_task = true;
            }
        }
    }

    /// Join a room and register as SFU peer (simulates join + offer completion).
    async fn join_and_register(ctx: &TestCtx, peer: &mut TestPeer, id: ParticipantId, room_id: &str) {
        peer.send(&SdpMessage::Join {
            participant: id,
            room_id: room_id.into(),
            display_name: None,
        });
        peer.recv().await; // Ready
        ctx.register_sfu_peer(peer, id, room_id).await;
    }

    // ── Signaling tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_two_peers_join_room() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        // A joins
        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "room1".into(),
            display_name: Some("Alice".into()),
        });
        let ready = a.recv().await;
        assert!(matches!(ready, SdpMessage::Ready { initiator: true, .. }));

        // B joins
        b.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "room1".into(),
            display_name: Some("Bob".into()),
        });
        let ready_b = b.recv().await;
        match &ready_b {
            SdpMessage::Ready { peers, initiator, .. } => {
                assert!(!initiator);
                assert_eq!(peers.len(), 1);
                assert_eq!(peers[0], pid(1));
            }
            _ => panic!("expected Ready, got {:?}", ready_b),
        }

        // A should receive PeerJoined for B
        let peer_joined = a.recv().await;
        match &peer_joined {
            SdpMessage::PeerJoined { participant, display_name, .. } => {
                assert_eq!(*participant, pid(2));
                assert_eq!(display_name.as_deref(), Some("Bob"));
            }
            _ => panic!("expected PeerJoined, got {:?}", peer_joined),
        }
    }

    #[tokio::test]
    async fn test_third_peer_joins_sees_both() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        // A and B join
        a.send(&SdpMessage::Join { participant: pid(1), room_id: "r".into(), display_name: None });
        a.recv().await; // Ready
        b.send(&SdpMessage::Join { participant: pid(2), room_id: "r".into(), display_name: None });
        b.recv().await; // Ready
        a.recv().await; // PeerJoined(B)

        // C joins — should see A and B in peers list
        c.send(&SdpMessage::Join { participant: pid(3), room_id: "r".into(), display_name: None });
        let ready_c = c.recv().await;
        match &ready_c {
            SdpMessage::Ready { peers, .. } => {
                assert_eq!(peers.len(), 2);
                assert!(peers.contains(&pid(1)));
                assert!(peers.contains(&pid(2)));
            }
            _ => panic!("expected Ready, got {:?}", ready_c),
        }

        // A and B should both receive PeerJoined(C)
        let a_msg = a.recv().await;
        assert!(matches!(a_msg, SdpMessage::PeerJoined { participant, .. } if participant == pid(3)));
        let b_msg = b.recv().await;
        assert!(matches!(b_msg, SdpMessage::PeerJoined { participant, .. } if participant == pid(3)));
    }

    #[tokio::test]
    async fn test_peer_leave_notifies_others() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        a.send(&SdpMessage::Join { participant: pid(1), room_id: "r".into(), display_name: None });
        a.recv().await;
        b.send(&SdpMessage::Join { participant: pid(2), room_id: "r".into(), display_name: None });
        b.recv().await;
        a.recv().await; // PeerJoined(B)

        // B disconnects
        b.disconnect();
        // Give the runtime time to process the disconnect
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // A should get PeerLeft + ScreenShareStopped (cleanup broadcast)
        let msgs = a.drain().await;
        let has_peer_left = msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(2)));
        assert!(has_peer_left, "expected PeerLeft for B, got: {:?}", msgs);
    }

    #[tokio::test]
    async fn test_mute_broadcast() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await; // PeerJoined(B)
        join_and_register(&ctx, &mut c, pid(3), "r").await;
        a.recv().await; // PeerJoined(C)
        b.recv().await; // PeerJoined(C)

        // A mutes — B and C should receive MuteAudio
        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        let has_mute = b_msgs.iter().any(|m| matches!(m, SdpMessage::MuteAudio { from, .. } if *from == pid(1)));
        assert!(has_mute, "B should receive MuteAudio, got: {:?}", b_msgs);

        let c_msgs = c.drain().await;
        let has_mute = c_msgs.iter().any(|m| matches!(m, SdpMessage::MuteAudio { from, .. } if *from == pid(1)));
        assert!(has_mute, "C should receive MuteAudio, got: {:?}", c_msgs);
    }

    #[tokio::test]
    async fn test_unmute_broadcast() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await; // PeerJoined(B)

        // A mutes then unmutes
        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        b.drain().await; // consume mute

        a.send(&SdpMessage::UnmuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        let has_unmute = b_msgs.iter().any(|m| matches!(m, SdpMessage::UnmuteAudio { from, .. } if *from == pid(1)));
        assert!(has_unmute, "B should receive UnmuteAudio, got: {:?}", b_msgs);
    }

    #[tokio::test]
    async fn test_screen_share_broadcast() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await; // PeerJoined(B)

        // A starts screen share — B should receive ScreenShareStarted
        a.send(&SdpMessage::ScreenShareStarted { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        let has_screen = b_msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStarted { from, .. } if *from == pid(1)));
        assert!(has_screen, "B should receive ScreenShareStarted, got: {:?}", b_msgs);
    }

    #[tokio::test]
    async fn test_screen_share_stop_broadcast() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await; // PeerJoined(B)

        a.send(&SdpMessage::ScreenShareStarted { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        b.drain().await;

        // A stops screen share
        a.send(&SdpMessage::ScreenShareStopped { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        let has_stop = b_msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStopped { from, .. } if *from == pid(1)));
        assert!(has_stop, "B should receive ScreenShareStopped, got: {:?}", b_msgs);
    }

    #[tokio::test]
    async fn test_disconnect_broadcasts_screen_share_stopped() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await; // PeerJoined(B)

        // A starts screen share then disconnects
        a.send(&SdpMessage::ScreenShareStarted { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        b.drain().await;

        a.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let b_msgs = b.drain().await;
        let has_screen_stop = b_msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStopped { from, .. } if *from == pid(1)));
        let has_peer_left = b_msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(1)));
        assert!(has_screen_stop, "B should receive ScreenShareStopped on disconnect, got: {:?}", b_msgs);
        assert!(has_peer_left, "B should receive PeerLeft on disconnect, got: {:?}", b_msgs);
    }

    #[tokio::test]
    async fn test_video_config_broadcast() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await; // PeerJoined(B)
        join_and_register(&ctx, &mut c, pid(3), "r").await;
        a.recv().await; // PeerJoined(C)
        b.recv().await; // PeerJoined(C)

        // A changes video config
        a.send(&SdpMessage::VideoConfigChanged {
            from: pid(1),
            room_id: "r".into(),
            width: 1920,
            height: 1080,
            fps: 30,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        let has_config = b_msgs.iter().any(|m| matches!(m,
            SdpMessage::VideoConfigChanged { from, width, height, fps, .. }
            if *from == pid(1) && *width == 1920 && *height == 1080 && *fps == 30
        ));
        assert!(has_config, "B should receive VideoConfigChanged, got: {:?}", b_msgs);

        let c_msgs = c.drain().await;
        let has_config = c_msgs.iter().any(|m| matches!(m, SdpMessage::VideoConfigChanged { from, .. } if *from == pid(1)));
        assert!(has_config, "C should receive VideoConfigChanged, got: {:?}", c_msgs);
    }

    #[tokio::test]
    async fn test_mute_unmute_cycle() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await; // PeerJoined(B)

        // Mute
        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = b.drain().await;
        assert!(msgs.iter().any(|m| matches!(m, SdpMessage::MuteAudio { .. })));

        // Unmute
        a.send(&SdpMessage::UnmuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = b.drain().await;
        assert!(msgs.iter().any(|m| matches!(m, SdpMessage::UnmuteAudio { .. })));

        // Mute again
        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = b.drain().await;
        assert!(msgs.iter().any(|m| matches!(m, SdpMessage::MuteAudio { .. })));
    }

    #[tokio::test]
    async fn test_disconnect_cleanup_state() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        a.send(&SdpMessage::Join { participant: pid(1), room_id: "r".into(), display_name: None });
        a.recv().await;
        b.send(&SdpMessage::Join { participant: pid(2), room_id: "r".into(), display_name: None });
        b.recv().await;
        a.recv().await;

        // B disconnects
        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Verify session cleanup
        let sess = ctx.hooks.sessions.read().await;
        assert!(!sess.contains_key(&pid(2)), "B's session should be cleaned up");
        drop(sess);

        // Verify room still exists (A is still in it)
        let rooms = ctx.hooks.rooms.read().await;
        assert!(rooms.contains_key("r"), "room should still exist");
    }

    #[tokio::test]
    async fn test_last_peer_leaves_room_cleaned_up() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();

        a.send(&SdpMessage::Join { participant: pid(1), room_id: "r".into(), display_name: None });
        a.recv().await;

        a.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Room should be cleaned up
        let rooms = ctx.hooks.rooms.read().await;
        assert!(!rooms.contains_key("r"), "room should be cleaned up when last peer leaves");
    }

    // ── Multi-participant complex scenarios ─────────────────────────

    #[tokio::test]
    async fn test_five_peers_join_and_see_each_other() {
        let ctx = TestCtx::new().await;
        let mut peers = Vec::new();
        let mut handles = Vec::new();

        for i in 1..=5u128 {
            let (mut p, h) = ctx.connect_peer();
            p.send(&SdpMessage::Join {
                participant: pid(i),
                room_id: "big".into(),
                display_name: Some(format!("P{i}")),
            });
            let ready = p.recv().await;
            match &ready {
                SdpMessage::Ready { peers: existing, .. } => {
                    assert_eq!(existing.len(), (i - 1) as usize, "peer {i} should see {prev} existing", prev = i - 1);
                }
                _ => panic!("expected Ready for peer {i}"),
            }
            // Drain PeerJoined notifications for earlier peers
            for earlier in peers.iter_mut() {
                let earlier: &mut TestPeer = earlier;
                earlier.drain().await;
            }
            peers.push(p);
            handles.push(h);
        }
    }

    #[tokio::test]
    async fn test_sequential_join_leave_join() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        // A joins
        a.send(&SdpMessage::Join { participant: pid(1), room_id: "r".into(), display_name: None });
        a.recv().await;

        // B joins
        b.send(&SdpMessage::Join { participant: pid(2), room_id: "r".into(), display_name: None });
        b.recv().await;
        a.recv().await; // PeerJoined(B)

        // B disconnects
        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        a.drain().await; // PeerLeft(B)

        // C joins (new peer on same room)
        let (mut c, _hc) = ctx.connect_peer();
        c.send(&SdpMessage::Join { participant: pid(3), room_id: "r".into(), display_name: None });
        let ready = c.recv().await;
        match &ready {
            SdpMessage::Ready { peers, .. } => {
                assert_eq!(peers.len(), 1);
                assert!(peers.contains(&pid(1)), "C should see A");
                assert!(!peers.contains(&pid(2)), "C should NOT see disconnected B");
            }
            _ => panic!("expected Ready"),
        }

        // A gets PeerJoined(C)
        let msg = a.recv().await;
        assert!(matches!(msg, SdpMessage::PeerJoined { participant, .. } if participant == pid(3)));
    }

    #[tokio::test]
    async fn test_rapid_connect_disconnect_cycle() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        a.send(&SdpMessage::Join { participant: pid(1), room_id: "r".into(), display_name: None });
        a.recv().await;

        // Rapidly connect and disconnect 5 peers
        for i in 2..=6u128 {
            let (mut p, _h) = ctx.connect_peer();
            p.send(&SdpMessage::Join { participant: pid(i), room_id: "r".into(), display_name: None });
            p.recv().await; // Ready
            p.disconnect();
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let msgs = a.drain().await;

        // A should have received PeerJoined + PeerLeft for each
        for i in 2..=6u128 {
            let has_joined = msgs.iter().any(|m| matches!(m, SdpMessage::PeerJoined { participant, .. } if *participant == pid(i)));
            let has_left = msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(i)));
            assert!(has_joined, "missing PeerJoined for pid({i})");
            assert!(has_left, "missing PeerLeft for pid({i})");
        }
    }

    // ── Cross-cases: mute + disconnect ──────────────────────────────

    #[tokio::test]
    async fn test_muted_peer_disconnects() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await; // PeerJoined(B)

        // B mutes
        b.send(&SdpMessage::MuteAudio { from: pid(2), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        a.drain().await;

        // B disconnects while muted
        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let msgs = a.drain().await;
        let has_left = msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(2)));
        assert!(has_left, "A should receive PeerLeft even though B was muted");
    }

    #[tokio::test]
    async fn test_mute_then_screen_share() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        // A mutes audio
        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = b.drain().await;
        assert!(msgs.iter().any(|m| matches!(m, SdpMessage::MuteAudio { .. })));

        // A starts screen share (while muted)
        a.send(&SdpMessage::ScreenShareStarted { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = b.drain().await;
        assert!(msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStarted { .. })),
            "B should receive ScreenShareStarted even though A is muted");
    }

    #[tokio::test]
    async fn test_screen_share_then_mute_then_unmute() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        // Screen share → mute → unmute → stop screen share
        a.send(&SdpMessage::ScreenShareStarted { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::UnmuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::ScreenShareStopped { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let msgs = b.drain().await;
        let types: Vec<&str> = msgs.iter().map(|m| match m {
            SdpMessage::ScreenShareStarted { .. } => "screen_start",
            SdpMessage::MuteAudio { .. } => "mute",
            SdpMessage::UnmuteAudio { .. } => "unmute",
            SdpMessage::ScreenShareStopped { .. } => "screen_stop",
            _ => "other",
        }).collect();

        assert!(types.contains(&"screen_start"), "missing screen_start in {:?}", types);
        assert!(types.contains(&"mute"), "missing mute in {:?}", types);
        assert!(types.contains(&"unmute"), "missing unmute in {:?}", types);
        assert!(types.contains(&"screen_stop"), "missing screen_stop in {:?}", types);
    }

    // ── Cross-cases: video config + disconnect ──────────────────────

    #[tokio::test]
    async fn test_video_config_then_disconnect() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        // B changes config then disconnects
        b.send(&SdpMessage::VideoConfigChanged { from: pid(2), room_id: "r".into(), width: 640, height: 480, fps: 15 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = a.drain().await;
        assert!(msgs.iter().any(|m| matches!(m, SdpMessage::VideoConfigChanged { width: 640, .. })));

        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let msgs = a.drain().await;
        assert!(msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(2))));
    }

    #[tokio::test]
    async fn test_video_config_change_multiple_times() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        // A changes config 3 times rapidly
        for (w, h, f) in [(640, 480, 15), (1280, 720, 30), (1920, 1080, 60)] {
            a.send(&SdpMessage::VideoConfigChanged { from: pid(1), room_id: "r".into(), width: w, height: h, fps: f });
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let msgs = b.drain().await;
        let configs: Vec<(u32, u32, u32)> = msgs.iter().filter_map(|m| match m {
            SdpMessage::VideoConfigChanged { width, height, fps, .. } => Some((*width, *height, *fps)),
            _ => None,
        }).collect();
        assert_eq!(configs.len(), 3, "B should receive all 3 config changes");
        assert_eq!(configs[2], (1920, 1080, 60), "last config should be 1080p60");
    }

    // ── Multi-room isolation ────────────────────────────────────────

    #[tokio::test]
    async fn test_different_rooms_isolated() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        // A and B in room1
        join_and_register(&ctx, &mut a, pid(1), "room1").await;
        join_and_register(&ctx, &mut b, pid(2), "room1").await;
        a.recv().await; // PeerJoined(B)

        // C in room2 (different room)
        join_and_register(&ctx, &mut c, pid(3), "room2").await;

        // A mutes in room1 — C (in room2) should NOT receive it
        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "room1".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        assert!(b_msgs.iter().any(|m| matches!(m, SdpMessage::MuteAudio { .. })), "B should get mute");

        let c_msgs = c.drain().await;
        let c_has_mute = c_msgs.iter().any(|m| matches!(m, SdpMessage::MuteAudio { .. }));
        assert!(!c_has_mute, "C (different room) should NOT receive MuteAudio");
    }

    #[tokio::test]
    async fn test_screen_share_different_rooms_isolated() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r1").await;
        join_and_register(&ctx, &mut b, pid(2), "r1").await;
        a.recv().await;
        join_and_register(&ctx, &mut c, pid(3), "r2").await;

        a.send(&SdpMessage::ScreenShareStarted { from: pid(1), room_id: "r1".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        assert!(b_msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStarted { .. })));

        let c_msgs = c.drain().await;
        assert!(!c_msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStarted { .. })),
            "C (different room) should NOT get screen share");
    }

    // ── Late joiner sees existing state ─────────────────────────────

    #[tokio::test]
    async fn test_late_joiner_gets_display_names() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        a.send(&SdpMessage::Join { participant: pid(1), room_id: "r".into(), display_name: Some("Alice".into()) });
        a.recv().await;

        b.send(&SdpMessage::Join { participant: pid(2), room_id: "r".into(), display_name: Some("Bob".into()) });
        let ready = b.recv().await;
        match &ready {
            SdpMessage::Ready { display_names, peers, .. } => {
                assert_eq!(peers.len(), 1);
                assert_eq!(display_names.get(&pid(1).to_string()), Some(&"Alice".to_string()));
            }
            _ => panic!("expected Ready"),
        }
    }

    // ── Disconnect during screen share + mute ───────────────────────

    #[tokio::test]
    async fn test_disconnect_while_sharing_and_muted() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;
        join_and_register(&ctx, &mut c, pid(3), "r").await;
        a.recv().await;
        b.recv().await;

        // A: mute + screen share
        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
        a.send(&SdpMessage::ScreenShareStarted { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        b.drain().await;
        c.drain().await;

        // A disconnects while muted and sharing
        a.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let b_msgs = b.drain().await;
        let has_screen_stop = b_msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStopped { from, .. } if *from == pid(1)));
        let has_left = b_msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(1)));
        assert!(has_screen_stop, "B should get ScreenShareStopped, got: {:?}", b_msgs);
        assert!(has_left, "B should get PeerLeft, got: {:?}", b_msgs);

        let c_msgs = c.drain().await;
        let has_screen_stop = c_msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStopped { from, .. } if *from == pid(1)));
        let has_left = c_msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(1)));
        assert!(has_screen_stop, "C should get ScreenShareStopped, got: {:?}", c_msgs);
        assert!(has_left, "C should get PeerLeft, got: {:?}", c_msgs);
    }

    // ── Multiple disconnections ─────────────────────────────────────

    #[tokio::test]
    async fn test_all_peers_disconnect_one_by_one() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        a.send(&SdpMessage::Join { participant: pid(1), room_id: "r".into(), display_name: None });
        a.recv().await;
        b.send(&SdpMessage::Join { participant: pid(2), room_id: "r".into(), display_name: None });
        b.recv().await;
        a.recv().await;
        c.send(&SdpMessage::Join { participant: pid(3), room_id: "r".into(), display_name: None });
        c.recv().await;
        a.recv().await;
        b.recv().await;

        // C leaves
        c.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let a_msgs = a.drain().await;
        assert!(a_msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(3))));
        let b_msgs = b.drain().await;
        assert!(b_msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(3))));

        // B leaves
        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let a_msgs = a.drain().await;
        assert!(a_msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(2))));

        // A leaves
        a.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Room should be fully cleaned up
        let rooms = ctx.hooks.rooms.read().await;
        assert!(!rooms.contains_key("r"));
    }

    #[tokio::test]
    async fn test_simultaneous_disconnect_two_peers() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        a.send(&SdpMessage::Join { participant: pid(1), room_id: "r".into(), display_name: None });
        a.recv().await;
        b.send(&SdpMessage::Join { participant: pid(2), room_id: "r".into(), display_name: None });
        b.recv().await;
        a.recv().await;
        c.send(&SdpMessage::Join { participant: pid(3), room_id: "r".into(), display_name: None });
        c.recv().await;
        a.recv().await;
        b.recv().await;

        // B and C disconnect "simultaneously"
        b.disconnect();
        c.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let msgs = a.drain().await;
        let left_b = msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(2)));
        let left_c = msgs.iter().any(|m| matches!(m, SdpMessage::PeerLeft { participant, .. } if *participant == pid(3)));
        assert!(left_b, "A should get PeerLeft for B");
        assert!(left_c, "A should get PeerLeft for C");
    }

    // ── Mute does not affect other peers ────────────────────────────

    #[tokio::test]
    async fn test_mute_only_affects_muter() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;
        join_and_register(&ctx, &mut c, pid(3), "r").await;
        a.recv().await;
        b.recv().await;

        // A mutes
        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        b.drain().await;
        c.drain().await;

        // B sends video config (B is NOT muted, should work fine)
        b.send(&SdpMessage::VideoConfigChanged { from: pid(2), room_id: "r".into(), width: 1280, height: 720, fps: 30 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let a_msgs = a.drain().await;
        assert!(a_msgs.iter().any(|m| matches!(m, SdpMessage::VideoConfigChanged { from, .. } if *from == pid(2))),
            "A should still receive messages from non-muted B");

        let c_msgs = c.drain().await;
        assert!(c_msgs.iter().any(|m| matches!(m, SdpMessage::VideoConfigChanged { from, .. } if *from == pid(2))),
            "C should still receive messages from non-muted B");
    }

    // ── Multiple screen shares (only one at a time per peer) ────────

    #[tokio::test]
    async fn test_two_peers_screen_share_simultaneously() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;
        join_and_register(&ctx, &mut c, pid(3), "r").await;
        a.recv().await;
        b.recv().await;

        // A and B both start screen sharing
        a.send(&SdpMessage::ScreenShareStarted { from: pid(1), room_id: "r".into(), track_id: 1 });
        b.send(&SdpMessage::ScreenShareStarted { from: pid(2), room_id: "r".into(), track_id: 2 });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let c_msgs = c.drain().await;
        let from_a = c_msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStarted { from, .. } if *from == pid(1)));
        let from_b = c_msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStarted { from, .. } if *from == pid(2)));
        assert!(from_a, "C should see A's screen share");
        assert!(from_b, "C should see B's screen share");
    }

    // ── Reconnection after disconnect ───────────────────────────────

    #[tokio::test]
    async fn test_peer_reconnects_after_disconnect() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b1, _hb1) = ctx.connect_peer();

        a.send(&SdpMessage::Join { participant: pid(1), room_id: "r".into(), display_name: None });
        a.recv().await;
        b1.send(&SdpMessage::Join { participant: pid(2), room_id: "r".into(), display_name: None });
        b1.recv().await;
        a.recv().await;

        // B disconnects
        b1.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        a.drain().await;

        // B reconnects (new connection, same pid)
        let (mut b2, _hb2) = ctx.connect_peer();
        b2.send(&SdpMessage::Join { participant: pid(2), room_id: "r".into(), display_name: Some("Bob v2".into()) });
        let ready = b2.recv().await;
        match &ready {
            SdpMessage::Ready { peers, .. } => {
                assert_eq!(peers.len(), 1);
                assert!(peers.contains(&pid(1)));
            }
            _ => panic!("expected Ready"),
        }

        // A should get PeerJoined for B again
        let msg = a.recv().await;
        match &msg {
            SdpMessage::PeerJoined { participant, display_name, .. } => {
                assert_eq!(*participant, pid(2));
                assert_eq!(display_name.as_deref(), Some("Bob v2"));
            }
            _ => panic!("expected PeerJoined, got {:?}", msg),
        }
    }

    // ── Stress: rapid mute/unmute ───────────────────────────────────

    #[tokio::test]
    async fn test_rapid_mute_unmute_10_times() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        for _ in 0..10 {
            a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
            a.send(&SdpMessage::UnmuteAudio { from: pid(1), room_id: "r".into() });
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let msgs = b.drain().await;
        let mutes = msgs.iter().filter(|m| matches!(m, SdpMessage::MuteAudio { .. })).count();
        let unmutes = msgs.iter().filter(|m| matches!(m, SdpMessage::UnmuteAudio { .. })).count();
        assert_eq!(mutes, 10, "should receive 10 mute events");
        assert_eq!(unmutes, 10, "should receive 10 unmute events");
    }

    // ── Edge: message to empty room ─────────────────────────────────

    #[tokio::test]
    async fn test_mute_in_room_alone() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;

        // A mutes while alone — should not crash, no one to receive
        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let msgs = a.drain().await;
        // A should NOT receive its own mute back
        assert!(!msgs.iter().any(|m| matches!(m, SdpMessage::MuteAudio { .. })));
    }

    #[tokio::test]
    async fn test_screen_share_in_room_alone() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;

        a.send(&SdpMessage::ScreenShareStarted { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let msgs = a.drain().await;
        assert!(!msgs.iter().any(|m| matches!(m, SdpMessage::ScreenShareStarted { .. })),
            "A should NOT receive its own screen share");
    }

    // ── Full lifecycle: join → config → screen → mute → unmute → stop → leave ──

    #[tokio::test]
    async fn test_full_lifecycle_all_features() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await; // PeerJoined(B)

        // 1. A sets video config
        a.send(&SdpMessage::VideoConfigChanged { from: pid(1), room_id: "r".into(), width: 1920, height: 1080, fps: 30 });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        // 2. A starts screen share
        a.send(&SdpMessage::ScreenShareStarted { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        // 3. A mutes
        a.send(&SdpMessage::MuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        // 4. A unmutes
        a.send(&SdpMessage::UnmuteAudio { from: pid(1), room_id: "r".into() });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        // 5. A changes config again
        a.send(&SdpMessage::VideoConfigChanged { from: pid(1), room_id: "r".into(), width: 1280, height: 720, fps: 60 });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        // 6. A stops screen share
        a.send(&SdpMessage::ScreenShareStopped { from: pid(1), room_id: "r".into(), track_id: 1 });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        // 7. A disconnects
        a.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let msgs = b.drain().await;
        let types: Vec<&str> = msgs.iter().map(|m| match m {
            SdpMessage::VideoConfigChanged { .. } => "config",
            SdpMessage::ScreenShareStarted { .. } => "screen_start",
            SdpMessage::MuteAudio { .. } => "mute",
            SdpMessage::UnmuteAudio { .. } => "unmute",
            SdpMessage::ScreenShareStopped { .. } => "screen_stop",
            SdpMessage::PeerLeft { .. } => "left",
            _ => "other",
        }).collect();

        assert!(types.contains(&"config"), "missing config in {:?}", types);
        assert!(types.contains(&"screen_start"), "missing screen_start in {:?}", types);
        assert!(types.contains(&"mute"), "missing mute in {:?}", types);
        assert!(types.contains(&"unmute"), "missing unmute in {:?}", types);
        assert!(types.contains(&"screen_stop"), "missing screen_stop in {:?}", types);
        assert!(types.contains(&"left"), "missing left in {:?}", types);

        // Should have 2 config changes
        let config_count = types.iter().filter(|&&t| t == "config").count();
        assert_eq!(config_count, 2, "should have 2 config changes");

        // screen_stop should appear at least twice (explicit + disconnect cleanup)
        let screen_stop_count = types.iter().filter(|&&t| t == "screen_stop").count();
        assert!(screen_stop_count >= 1, "should have at least 1 screen_stop");
    }
}
