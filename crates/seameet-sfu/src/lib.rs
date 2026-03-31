pub mod media;

pub use media::{ForwardedMedia, PeerCmd, Peers, RouteTable, SfuPeer};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use media::*;
use seameet_core::ParticipantId;
use seameet_signaling::engine::{broadcast_room_status, SignalingHooks, SignalingState};
use seameet_signaling::SdpMessage;
use str0m::bwe::Bitrate;
use str0m::change::SdpOffer;
use str0m::net::Protocol;
use str0m::{Candidate, Output, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{info, warn};

// ── Configuration ─────────────────────────────────────────────────────

pub struct SfuConfig {
    pub udp_port: u16,
    pub public_ip: Option<std::net::IpAddr>,
    pub bwe_kbps: Option<u32>,
}

impl Default for SfuConfig {
    fn default() -> Self {
        Self {
            udp_port: 10000,
            public_ip: None,
            bwe_kbps: None,
        }
    }
}

// ── Internal types ────────────────────────────────────────────────────

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

/// Per-connection state keyed by participant ID.
struct PeerSession {
    has_media_task: bool,
    current_room_id: Option<String>,
}

type Sessions = Arc<RwLock<HashMap<ParticipantId, PeerSession>>>;

// ── SfuServer ─────────────────────────────────────────────────────────

pub struct SfuServer {
    rooms: Rooms,
    all_peers: Peers,
    socket: Arc<UdpSocket>,
    routes: RouteTable,
    udp_local_addr: SocketAddr,
    sessions: Sessions,
    /// Monotonic counter for SfuPeer generations (reconnection detection).
    next_peer_gen: std::sync::atomic::AtomicU64,
    /// Optional BWE override from SfuConfig (kbps).
    bwe_kbps: Option<u32>,
}

impl SfuServer {
    pub async fn new(config: SfuConfig) -> Result<Arc<Self>, std::io::Error> {
        let shared_socket =
            Arc::new(UdpSocket::bind(format!("0.0.0.0:{}", config.udp_port)).await?);
        let udp_local_addr = shared_socket.local_addr()?;

        let all_peers: Peers = Arc::new(RwLock::new(HashMap::new()));
        let routes: RouteTable = Arc::new(RwLock::new(HashMap::new()));

        // Spawn UDP reader.
        tokio::spawn(udp_reader(
            Arc::clone(&shared_socket),
            Arc::clone(&all_peers),
            Arc::clone(&routes),
        ));

        Ok(Arc::new(Self {
            rooms: Arc::new(RwLock::new(HashMap::new())),
            all_peers,
            socket: shared_socket,
            routes,
            udp_local_addr,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            next_peer_gen: std::sync::atomic::AtomicU64::new(1),
            bwe_kbps: config.bwe_kbps,
        }))
    }

    pub fn signaling_state(&self) -> Arc<RwLock<SignalingState>> {
        Arc::new(RwLock::new(SignalingState::new()))
    }

    pub fn udp_local_addr(&self) -> SocketAddr {
        self.udp_local_addr
    }

    /// Returns the shared Peers map for a participant's current room.
    async fn room_peers_for(&self, pid: &ParticipantId) -> Option<Peers> {
        let sess = self.sessions.read().await;
        let room_id = sess.get(pid)?.current_room_id.as_ref()?.clone();
        drop(sess);

        let rooms = self.rooms.read().await;
        rooms.get(&room_id).map(|r| Arc::clone(&r.peers))
    }
}

impl SignalingHooks for SfuServer {
    async fn on_message(
        &self,
        sdp: &SdpMessage,
        _raw: &str,
        pid: ParticipantId,
        self_tx: &mpsc::UnboundedSender<String>,
        state: &Arc<RwLock<SignalingState>>,
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

                let bwe = self
                    .config_bwe_kbps()
                    .map(Bitrate::kbps)
                    .unwrap_or(Bitrate::kbps(8_000));

                let mut config = RtcConfig::new()
                    .set_rtp_mode(true)
                    .enable_raw_packets(true)
                    .enable_bwe(Some(bwe));
                {
                    let cc = config.codec_config();
                    cc.enable_h264(false);
                    cc.enable_vp9(true);
                }
                let mut rtc = config.build();

                if let Ok(c) = Candidate::host(local_addr, Protocol::Udp) {
                    rtc.add_local_candidate(c);
                }

                if let Some(public_ip) = self.public_ip() {
                    let public_addr: SocketAddr = (public_ip, self.udp_local_addr.port()).into();
                    if let Ok(c) = Candidate::host(public_addr, Protocol::Udp) {
                        rtc.add_local_candidate(c);
                        info!(%public_addr, "added public IP candidate");
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
                        || line.starts_with("a=inactive")
                    {
                        info!(participant = %pid, "SDP answer direction: {line}");
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

                let peer_gen = self
                    .next_peer_gen
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
                let sfu_peer = SfuPeer {
                    cmd_tx,
                    ws_tx: self_tx.clone(),
                    gen: peer_gen,
                };
                {
                    let mut p = room_peers.write().await;
                    p.insert(
                        pid,
                        SfuPeer {
                            cmd_tx: sfu_peer.cmd_tx.clone(),
                            ws_tx: sfu_peer.ws_tx.clone(),
                            gen: peer_gen,
                        },
                    );
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
                let media_room_id = sess
                    .get(&pid)
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
                        peer_gen,
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

            SdpMessage::ScreenShareStarted {
                from,
                room_id,
                track_id,
            } => {
                if let Some(room_peers) = self.room_peers_for(&pid).await {
                    let p = room_peers.read().await;
                    if let Some(peer) = p.get(from) {
                        let _ = peer.cmd_tx.send(PeerCmd::ScreenShareActive(true));
                    }
                    // WS broadcast screen_share_started for transceiver routing
                    let msg = SdpMessage::ScreenShareStarted {
                        from: *from,
                        room_id: room_id.clone(),
                        track_id: *track_id,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        for (id, peer) in p.iter() {
                            if *id != pid {
                                let _ = peer.ws_tx.send(json.clone());
                            }
                        }
                    }
                }
                // Update signaling media state + broadcast room_status
                {
                    let mut st = state.write().await;
                    if let Some(room) = st.room_mut(room_id) {
                        room.set_screen_sharing(&pid, true);
                    }
                    broadcast_room_status(&st, room_id);
                }
                true
            }

            SdpMessage::ScreenShareStopped {
                from,
                room_id,
                track_id,
            } => {
                if let Some(room_peers) = self.room_peers_for(&pid).await {
                    let p = room_peers.read().await;
                    if let Some(peer) = p.get(from) {
                        let _ = peer.cmd_tx.send(PeerCmd::ScreenShareActive(false));
                    }
                    // WS broadcast screen_share_stopped for transceiver routing
                    let msg = SdpMessage::ScreenShareStopped {
                        from: *from,
                        room_id: room_id.clone(),
                        track_id: *track_id,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        for (id, peer) in p.iter() {
                            if *id != pid {
                                let _ = peer.ws_tx.send(json.clone());
                            }
                        }
                    }
                }
                // Update signaling media state + broadcast room_status
                {
                    let mut st = state.write().await;
                    if let Some(room) = st.room_mut(room_id) {
                        room.set_screen_sharing(&pid, false);
                    }
                    broadcast_room_status(&st, room_id);
                }
                true
            }

            SdpMessage::MuteAudio { from, room_id } => {
                if let Some(room_peers) = self.room_peers_for(&pid).await {
                    let p = room_peers.read().await;
                    if let Some(peer) = p.get(from) {
                        let _ = peer.cmd_tx.send(PeerCmd::SetMuted(true));
                    }
                    for (id, peer) in p.iter() {
                        if *id != *from {
                            let _ = peer.cmd_tx.send(PeerCmd::PeerMuteChanged {
                                pid: *from,
                                muted: true,
                            });
                        }
                    }
                }
                // Update signaling media state + broadcast room_status
                {
                    let mut st = state.write().await;
                    if let Some(room) = st.room_mut(room_id) {
                        room.set_audio_muted(&pid, true);
                    }
                    broadcast_room_status(&st, room_id);
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
                }
                // Update signaling media state + broadcast room_status
                {
                    let mut st = state.write().await;
                    if let Some(room) = st.room_mut(room_id) {
                        room.set_audio_muted(&pid, false);
                    }
                    broadcast_room_status(&st, room_id);
                }
                true
            }

            SdpMessage::MuteVideo { room_id, .. } => {
                // Update signaling media state + broadcast room_status
                let mut st = state.write().await;
                if let Some(room) = st.room_mut(room_id) {
                    room.set_video_muted(&pid, true);
                }
                broadcast_room_status(&st, room_id);
                true
            }

            SdpMessage::UnmuteVideo { room_id, .. } => {
                // Update signaling media state + broadcast room_status
                let mut st = state.write().await;
                if let Some(room) = st.room_mut(room_id) {
                    room.set_video_muted(&pid, false);
                }
                broadcast_room_status(&st, room_id);
                true
            }

            SdpMessage::VideoConfigChanged {
                from,
                room_id,
                width,
                height,
                fps,
            } => {
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
                // Prune stale members from signaling state and notify
                // media tasks (PeerCmd::PeerLeft) BEFORE the default dispatch
                // creates the new peer. WS notification is handled by
                // room_status broadcast in dispatch.
                {
                    let mut st = state.write().await;
                    let pruned = st
                        .room_mut(room_id)
                        .map(|r| r.prune_stale())
                        .unwrap_or_default();
                    drop(st);

                    // Notify media tasks so stale source_slots are cleaned up
                    // and MIDs are correctly reassigned on reconnect.
                    if !pruned.is_empty() {
                        let rooms = self.rooms.read().await;
                        if let Some(room) = rooms.get(room_id) {
                            let p = room.peers.read().await;
                            for stale_pid in &pruned {
                                info!(participant = %stale_pid, room = room_id, "SFU: pruned stale member on join, sending PeerLeft to media tasks");
                                for (id, peer) in p.iter() {
                                    if id != stale_pid {
                                        let _ =
                                            peer.cmd_tx.send(PeerCmd::PeerLeft { pid: *stale_pid });
                                    }
                                }
                            }
                        }
                    }

                    // Rapid reconnect: if the joining participant is already in
                    // room_peers but was NOT caught by prune_stale, notify other
                    // media tasks so they clear the stale source_slot.  Without
                    // this, the old slot is silently reused and no keyframe is
                    // requested for the new media stream → frozen video.
                    if !pruned.contains(&pid) {
                        let rooms = self.rooms.read().await;
                        if let Some(room) = rooms.get(room_id) {
                            let p = room.peers.read().await;
                            if p.contains_key(&pid) {
                                info!(participant = %pid, room = room_id, "SFU: re-join detected (not pruned), sending PeerLeft to media tasks");
                                for (id, peer) in p.iter() {
                                    if *id != pid {
                                        let _ = peer.cmd_tx.send(PeerCmd::PeerLeft { pid });
                                    }
                                }
                            }
                        }
                    }
                }

                {
                    let mut r = self.rooms.write().await;
                    r.entry(room_id.clone()).or_insert_with(RoomState::new);
                }

                {
                    let mut sess = self.sessions.write().await;
                    sess.insert(
                        pid,
                        PeerSession {
                            has_media_task: false,
                            current_room_id: Some(room_id.clone()),
                        },
                    );
                }

                {
                    let rooms = self.rooms.read().await;
                    if let Some(room) = rooms.get(room_id) {
                        let p = room.peers.read().await;
                        let remote_count = p.len();
                        for (_id, peer) in p.iter() {
                            let _ = peer.cmd_tx.send(PeerCmd::PeerCountChanged {
                                remote_peer_count: remote_count,
                            });
                        }
                    }
                }

                false
            }

            _ => false,
        }
    }

    async fn on_disconnect(
        &self,
        pid: ParticipantId,
        affected_rooms: &[(String, bool)],
        _state: &Arc<RwLock<SignalingState>>,
    ) {
        {
            let mut r = self.routes.write().await;
            r.retain(|_, v| *v != pid);
        }
        self.all_peers.write().await.remove(&pid);

        for (room_id, room_empty) in affected_rooms {
            let room_peers = {
                let rooms = self.rooms.read().await;
                rooms.get(room_id).map(|r| Arc::clone(&r.peers))
            };
            if let Some(room_peers) = room_peers {
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
                            // Clear stale source slot so a reconnect gets fresh MIDs.
                            let _ = peer.cmd_tx.send(PeerCmd::PeerLeft { pid });
                        }
                    }
                }
                room_peers.write().await.remove(&pid);
            }
            if *room_empty {
                self.rooms.write().await.remove(room_id);
            }
        }

        self.sessions.write().await.remove(&pid);
    }
}

// Private helpers for SfuServer config access.
impl SfuServer {
    fn config_bwe_kbps(&self) -> Option<u64> {
        self.bwe_kbps.map(|v| v as u64)
    }

    fn public_ip(&self) -> Option<std::net::IpAddr> {
        // Check PUBLIC_IP env var at runtime.
        std::env::var("PUBLIC_IP").ok().and_then(|s| s.parse().ok())
    }
}

// ── Integration tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use seameet_signaling::engine::{run_connection, SignalingState};
    use seameet_signaling::transport::{ConnectionReader, IncomingConnection};
    use seameet_signaling::SdpMessage;
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
        out_tx: mpsc::UnboundedSender<String>,
    }

    impl TestPeer {
        fn send(&self, msg: &SdpMessage) {
            let json = serde_json::to_string(msg).unwrap();
            self.tx.send(json).unwrap();
        }

        async fn recv(&mut self) -> SdpMessage {
            let json = tokio::time::timeout(std::time::Duration::from_secs(2), self.out_rx.recv())
                .await
                .expect("timeout waiting for message")
                .expect("channel closed");
            serde_json::from_str(&json).unwrap()
        }

        async fn try_recv(&mut self) -> Option<SdpMessage> {
            match tokio::time::timeout(std::time::Duration::from_millis(100), self.out_rx.recv())
                .await
            {
                Ok(Some(json)) => serde_json::from_str(&json).ok(),
                _ => None,
            }
        }

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

    struct TestCtx {
        state: Arc<RwLock<SignalingState>>,
        hooks: Arc<SfuServer>,
    }

    impl TestCtx {
        async fn new() -> Self {
            let socket = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
            let addr = socket.local_addr().unwrap();

            let hooks = Arc::new(SfuServer {
                rooms: Arc::new(RwLock::new(HashMap::new())),
                all_peers: Arc::new(RwLock::new(HashMap::new())),
                socket,
                routes: Arc::new(RwLock::new(HashMap::new())),
                udp_local_addr: addr,
                sessions: Arc::new(RwLock::new(HashMap::new())),
                next_peer_gen: std::sync::atomic::AtomicU64::new(1),
                bwe_kbps: None,
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
                writer_handle: None,
            };

            let state = Arc::clone(&self.state);
            let hooks = Arc::clone(&self.hooks);
            let handle = tokio::spawn(run_connection(conn, state, hooks));

            (
                TestPeer {
                    tx: in_tx,
                    out_rx,
                    out_tx: out_tx_clone,
                },
                handle,
            )
        }

        async fn register_sfu_peer(&self, peer: &TestPeer, id: ParticipantId, room_id: &str) {
            let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
            let sfu_peer = SfuPeer {
                cmd_tx,
                ws_tx: peer.out_tx.clone(),
                gen: 0,
            };

            let rooms = self.hooks.rooms.read().await;
            if let Some(room) = rooms.get(room_id) {
                room.peers.write().await.insert(id, sfu_peer.clone());
            }
            drop(rooms);

            self.hooks.all_peers.write().await.insert(id, sfu_peer);

            let mut sess = self.hooks.sessions.write().await;
            if let Some(s) = sess.get_mut(&id) {
                s.has_media_task = true;
            }
        }
    }

    async fn join_and_register(
        ctx: &TestCtx,
        peer: &mut TestPeer,
        id: ParticipantId,
        room_id: &str,
    ) {
        peer.send(&SdpMessage::Join {
            participant: id,
            room_id: room_id.into(),
            display_name: None,
        });
        peer.recv().await; // Ready
        ctx.register_sfu_peer(peer, id, room_id).await;
    }

    impl TestPeer {
        /// Receives the next message, skipping any `RoomStatus` messages.
        async fn recv_skip_status(&mut self) -> SdpMessage {
            loop {
                let msg = self.recv().await;
                if matches!(msg, SdpMessage::RoomStatus { .. }) {
                    continue;
                }
                return msg;
            }
        }
    }

    // ── Signaling tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_two_peers_join_room() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "room1".into(),
            display_name: Some("Alice".into()),
        });
        let ready = a.recv().await;
        assert!(matches!(
            ready,
            SdpMessage::Ready {
                initiator: true,
                ..
            }
        ));

        b.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "room1".into(),
            display_name: Some("Bob".into()),
        });
        let ready_b = b.recv().await;
        match &ready_b {
            SdpMessage::Ready {
                peers, initiator, ..
            } => {
                assert!(!initiator);
                assert_eq!(peers.len(), 1);
                assert_eq!(peers[0], pid(1));
            }
            _ => panic!("expected Ready, got {:?}", ready_b),
        }

        // A receives room_status with B (pid(2)) present
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = a.drain().await;
        let has_b = msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants
                    .iter()
                    .any(|p| p.id == pid(2) && p.display_name.as_deref() == Some("Bob"))
            } else {
                false
            }
        });
        assert!(
            has_b,
            "A should receive RoomStatus with Bob, got: {:?}",
            msgs
        );
    }

    #[tokio::test]
    async fn test_third_peer_joins_sees_both() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "r".into(),
            display_name: None,
        });
        a.recv().await; // Ready
        b.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "r".into(),
            display_name: None,
        });
        b.recv().await; // Ready
                        // Drain A's RoomStatus from both joins
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        a.drain().await;

        c.send(&SdpMessage::Join {
            participant: pid(3),
            room_id: "r".into(),
            display_name: None,
        });
        let ready_c = c.recv().await;
        match &ready_c {
            SdpMessage::Ready { peers, .. } => {
                assert_eq!(peers.len(), 2);
                assert!(peers.contains(&pid(1)));
                assert!(peers.contains(&pid(2)));
            }
            _ => panic!("expected Ready, got {:?}", ready_c),
        }

        // A and B should receive room_status with C (pid(3)) present
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let a_msgs = a.drain().await;
        assert!(a_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(3))
            } else {
                false
            }
        }));
        let b_msgs = b.drain().await;
        assert!(b_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(3))
            } else {
                false
            }
        }));
    }

    #[tokio::test]
    async fn test_peer_leave_notifies_others() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "r".into(),
            display_name: None,
        });
        a.recv().await;
        b.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "r".into(),
            display_name: None,
        });
        b.recv().await;
        a.recv().await;

        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let msgs = a.drain().await;
        // B's departure is notified via room_status (no longer PeerLeft)
        let has_b_gone = msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(2))
            } else {
                false
            }
        });
        assert!(has_b_gone, "expected RoomStatus without B, got: {:?}", msgs);
    }

    #[tokio::test]
    async fn test_mute_broadcast() {
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

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        let has_mute = b_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(1) && p.audio_muted)
            } else {
                false
            }
        });
        assert!(
            has_mute,
            "B should receive RoomStatus with audio_muted for pid(1), got: {:?}",
            b_msgs
        );

        let c_msgs = c.drain().await;
        let has_mute = c_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(1) && p.audio_muted)
            } else {
                false
            }
        });
        assert!(
            has_mute,
            "C should receive RoomStatus with audio_muted for pid(1), got: {:?}",
            c_msgs
        );
    }

    #[tokio::test]
    async fn test_unmute_broadcast() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        b.drain().await;

        a.send(&SdpMessage::UnmuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        let has_unmute = b_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants
                    .iter()
                    .any(|p| p.id == pid(1) && !p.audio_muted)
            } else {
                false
            }
        });
        assert!(
            has_unmute,
            "B should receive RoomStatus with audio unmuted for pid(1), got: {:?}",
            b_msgs
        );
    }

    #[tokio::test]
    async fn test_screen_share_broadcast() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        a.send(&SdpMessage::ScreenShareStarted {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        let has_screen = b_msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::ScreenShareStarted { from, .. } if *from == pid(1)));
        assert!(
            has_screen,
            "B should receive ScreenShareStarted, got: {:?}",
            b_msgs
        );
    }

    #[tokio::test]
    async fn test_screen_share_stop_broadcast() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        a.send(&SdpMessage::ScreenShareStarted {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        b.drain().await;

        a.send(&SdpMessage::ScreenShareStopped {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        let has_stop = b_msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::ScreenShareStopped { from, .. } if *from == pid(1)));
        assert!(
            has_stop,
            "B should receive ScreenShareStopped, got: {:?}",
            b_msgs
        );
    }

    #[tokio::test]
    async fn test_disconnect_broadcasts_screen_share_stopped() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        a.send(&SdpMessage::ScreenShareStarted {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        b.drain().await;

        a.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let b_msgs = b.drain().await;
        let has_screen_stop = b_msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::ScreenShareStopped { from, .. } if *from == pid(1)));
        let has_a_gone = b_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(1))
            } else {
                false
            }
        });
        assert!(
            has_screen_stop,
            "B should receive ScreenShareStopped on disconnect, got: {:?}",
            b_msgs
        );
        assert!(
            has_a_gone,
            "B should receive RoomStatus without A on disconnect, got: {:?}",
            b_msgs
        );
    }

    #[tokio::test]
    async fn test_video_config_broadcast() {
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

        a.send(&SdpMessage::VideoConfigChanged {
            from: pid(1),
            room_id: "r".into(),
            width: 1920,
            height: 1080,
            fps: 30,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        let has_config = b_msgs.iter().any(|m| {
            matches!(m,
            SdpMessage::VideoConfigChanged { from, width, height, fps, .. }
            if *from == pid(1) && *width == 1920 && *height == 1080 && *fps == 30)
        });
        assert!(
            has_config,
            "B should receive VideoConfigChanged, got: {:?}",
            b_msgs
        );

        let c_msgs = c.drain().await;
        let has_config = c_msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::VideoConfigChanged { from, .. } if *from == pid(1)));
        assert!(
            has_config,
            "C should receive VideoConfigChanged, got: {:?}",
            c_msgs
        );
    }

    #[tokio::test]
    async fn test_mute_unmute_cycle() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = b.drain().await;
        assert!(msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(1) && p.audio_muted)
            } else {
                false
            }
        }));

        a.send(&SdpMessage::UnmuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = b.drain().await;
        assert!(msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants
                    .iter()
                    .any(|p| p.id == pid(1) && !p.audio_muted)
            } else {
                false
            }
        }));

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = b.drain().await;
        assert!(msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(1) && p.audio_muted)
            } else {
                false
            }
        }));
    }

    #[tokio::test]
    async fn test_disconnect_cleanup_state() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "r".into(),
            display_name: None,
        });
        a.recv().await;
        b.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "r".into(),
            display_name: None,
        });
        b.recv().await;
        a.recv().await;

        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let sess = ctx.hooks.sessions.read().await;
        assert!(
            !sess.contains_key(&pid(2)),
            "B's session should be cleaned up"
        );
        drop(sess);

        let rooms = ctx.hooks.rooms.read().await;
        assert!(rooms.contains_key("r"), "room should still exist");
    }

    #[tokio::test]
    async fn test_last_peer_leaves_room_cleaned_up() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();

        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "r".into(),
            display_name: None,
        });
        a.recv().await;

        a.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let rooms = ctx.hooks.rooms.read().await;
        assert!(
            !rooms.contains_key("r"),
            "room should be cleaned up when last peer leaves"
        );
    }

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
                SdpMessage::Ready {
                    peers: existing, ..
                } => {
                    assert_eq!(
                        existing.len(),
                        (i - 1) as usize,
                        "peer {i} should see {prev} existing",
                        prev = i - 1
                    );
                }
                _ => panic!("expected Ready for peer {i}"),
            }
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

        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "r".into(),
            display_name: None,
        });
        a.recv().await;

        b.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "r".into(),
            display_name: None,
        });
        b.recv().await;
        a.recv().await;

        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        a.drain().await;

        let (mut c, _hc) = ctx.connect_peer();
        c.send(&SdpMessage::Join {
            participant: pid(3),
            room_id: "r".into(),
            display_name: None,
        });
        let ready = c.recv().await;
        match &ready {
            SdpMessage::Ready { peers, .. } => {
                assert_eq!(peers.len(), 1);
                assert!(peers.contains(&pid(1)), "C should see A");
                assert!(!peers.contains(&pid(2)), "C should NOT see disconnected B");
            }
            _ => panic!("expected Ready"),
        }

        // A receives room_status with C (pid(3)) present
        let msg = a.recv().await;
        match &msg {
            SdpMessage::RoomStatus { participants, .. } => {
                assert!(
                    participants.iter().any(|p| p.id == pid(3)),
                    "C should be in room_status"
                );
            }
            _ => panic!("expected RoomStatus, got {:?}", msg),
        }
    }

    #[tokio::test]
    async fn test_rapid_connect_disconnect_cycle() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "r".into(),
            display_name: None,
        });
        a.recv().await;

        for i in 2..=6u128 {
            let (mut p, _h) = ctx.connect_peer();
            p.send(&SdpMessage::Join {
                participant: pid(i),
                room_id: "r".into(),
                display_name: None,
            });
            p.recv().await;
            p.disconnect();
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let msgs = a.drain().await;

        // Each peer's join and leave is reflected via room_status snapshots.
        // Verify that we received room_status messages showing each peer present then absent.
        for i in 2..=6u128 {
            let has_joined = msgs.iter().any(|m| {
                if let SdpMessage::RoomStatus { participants, .. } = m {
                    participants.iter().any(|p| p.id == pid(i))
                } else {
                    false
                }
            });
            let has_left = msgs.iter().any(|m| {
                if let SdpMessage::RoomStatus { participants, .. } = m {
                    !participants.iter().any(|p| p.id == pid(i))
                } else {
                    false
                }
            });
            assert!(has_joined, "missing room_status with pid({i}) present");
            assert!(has_left, "missing room_status without pid({i})");
        }
    }

    #[tokio::test]
    async fn test_muted_peer_disconnects() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        b.send(&SdpMessage::MuteAudio {
            from: pid(2),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        a.drain().await;

        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let msgs = a.drain().await;
        let has_left = msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(2))
            } else {
                false
            }
        });
        assert!(
            has_left,
            "A should receive RoomStatus without B after disconnect"
        );
    }

    #[tokio::test]
    async fn test_mute_then_screen_share() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = b.drain().await;
        assert!(msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(1) && p.audio_muted)
            } else {
                false
            }
        }));

        a.send(&SdpMessage::ScreenShareStarted {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = b.drain().await;
        assert!(
            msgs.iter()
                .any(|m| matches!(m, SdpMessage::ScreenShareStarted { .. })),
            "B should receive ScreenShareStarted even though A is muted"
        );
    }

    #[tokio::test]
    async fn test_screen_share_then_mute_then_unmute() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        a.send(&SdpMessage::ScreenShareStarted {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::UnmuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::ScreenShareStopped {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let msgs = b.drain().await;
        let types: Vec<&str> = msgs
            .iter()
            .map(|m| match m {
                SdpMessage::ScreenShareStarted { .. } => "screen_start",
                SdpMessage::ScreenShareStopped { .. } => "screen_stop",
                SdpMessage::RoomStatus { .. } => "room_status",
                _ => "other",
            })
            .collect();

        assert!(
            types.contains(&"screen_start"),
            "missing screen_start in {:?}",
            types
        );
        assert!(
            types.contains(&"room_status"),
            "missing room_status in {:?}",
            types
        );
        // Verify mute/unmute state in room_status snapshots
        let has_muted = msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(1) && p.audio_muted)
            } else {
                false
            }
        });
        let has_unmuted = msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants
                    .iter()
                    .any(|p| p.id == pid(1) && !p.audio_muted)
            } else {
                false
            }
        });
        assert!(has_muted, "missing room_status with audio_muted=true");
        assert!(has_unmuted, "missing room_status with audio_muted=false");
        assert!(
            types.contains(&"screen_stop"),
            "missing screen_stop in {:?}",
            types
        );
    }

    #[tokio::test]
    async fn test_video_config_then_disconnect() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        b.send(&SdpMessage::VideoConfigChanged {
            from: pid(2),
            room_id: "r".into(),
            width: 640,
            height: 480,
            fps: 15,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msgs = a.drain().await;
        assert!(msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::VideoConfigChanged { width: 640, .. })));

        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let msgs = a.drain().await;
        assert!(msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(2))
            } else {
                false
            }
        }));
    }

    #[tokio::test]
    async fn test_video_config_change_multiple_times() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        for (w, h, f) in [(640, 480, 15), (1280, 720, 30), (1920, 1080, 60)] {
            a.send(&SdpMessage::VideoConfigChanged {
                from: pid(1),
                room_id: "r".into(),
                width: w,
                height: h,
                fps: f,
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let msgs = b.drain().await;
        let configs: Vec<(u32, u32, u32)> = msgs
            .iter()
            .filter_map(|m| match m {
                SdpMessage::VideoConfigChanged {
                    width, height, fps, ..
                } => Some((*width, *height, *fps)),
                _ => None,
            })
            .collect();
        assert_eq!(configs.len(), 3, "B should receive all 3 config changes");
        assert_eq!(
            configs[2],
            (1920, 1080, 60),
            "last config should be 1080p60"
        );
    }

    #[tokio::test]
    async fn test_different_rooms_isolated() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "room1").await;
        join_and_register(&ctx, &mut b, pid(2), "room1").await;
        a.recv().await;

        join_and_register(&ctx, &mut c, pid(3), "room2").await;

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "room1".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        assert!(
            b_msgs.iter().any(|m| {
                if let SdpMessage::RoomStatus { participants, .. } = m {
                    participants.iter().any(|p| p.id == pid(1) && p.audio_muted)
                } else {
                    false
                }
            }),
            "B should get RoomStatus with audio_muted for pid(1)"
        );

        let c_msgs = c.drain().await;
        let c_has_mute = c_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(1) && p.audio_muted)
            } else {
                false
            }
        });
        assert!(
            !c_has_mute,
            "C (different room) should NOT receive RoomStatus about room1"
        );
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

        a.send(&SdpMessage::ScreenShareStarted {
            from: pid(1),
            room_id: "r1".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let b_msgs = b.drain().await;
        assert!(b_msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::ScreenShareStarted { .. })));

        let c_msgs = c.drain().await;
        assert!(
            !c_msgs
                .iter()
                .any(|m| matches!(m, SdpMessage::ScreenShareStarted { .. })),
            "C (different room) should NOT get screen share"
        );
    }

    #[tokio::test]
    async fn test_late_joiner_gets_display_names() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "r".into(),
            display_name: Some("Alice".into()),
        });
        a.recv().await;

        b.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "r".into(),
            display_name: Some("Bob".into()),
        });
        let ready = b.recv().await;
        match &ready {
            SdpMessage::Ready {
                display_names,
                peers,
                ..
            } => {
                assert_eq!(peers.len(), 1);
                assert_eq!(
                    display_names.get(&pid(1).to_string()),
                    Some(&"Alice".to_string())
                );
            }
            _ => panic!("expected Ready"),
        }
    }

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

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        a.send(&SdpMessage::ScreenShareStarted {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        b.drain().await;
        c.drain().await;

        a.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let b_msgs = b.drain().await;
        let has_screen_stop = b_msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::ScreenShareStopped { from, .. } if *from == pid(1)));
        let has_left = b_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(1))
            } else {
                false
            }
        });
        assert!(
            has_screen_stop,
            "B should get ScreenShareStopped, got: {:?}",
            b_msgs
        );
        assert!(
            has_left,
            "B should get RoomStatus without A, got: {:?}",
            b_msgs
        );

        let c_msgs = c.drain().await;
        let has_screen_stop = c_msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::ScreenShareStopped { from, .. } if *from == pid(1)));
        let has_left = c_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(1))
            } else {
                false
            }
        });
        assert!(
            has_screen_stop,
            "C should get ScreenShareStopped, got: {:?}",
            c_msgs
        );
        assert!(
            has_left,
            "C should get RoomStatus without A, got: {:?}",
            c_msgs
        );
    }

    #[tokio::test]
    async fn test_all_peers_disconnect_one_by_one() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "r".into(),
            display_name: None,
        });
        a.recv().await;
        b.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "r".into(),
            display_name: None,
        });
        b.recv().await;
        a.recv().await;
        c.send(&SdpMessage::Join {
            participant: pid(3),
            room_id: "r".into(),
            display_name: None,
        });
        c.recv().await;
        a.recv().await;
        b.recv().await;

        c.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let a_msgs = a.drain().await;
        assert!(a_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(3))
            } else {
                false
            }
        }));
        let b_msgs = b.drain().await;
        assert!(b_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(3))
            } else {
                false
            }
        }));

        b.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let a_msgs = a.drain().await;
        assert!(a_msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(2))
            } else {
                false
            }
        }));

        a.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let rooms = ctx.hooks.rooms.read().await;
        assert!(!rooms.contains_key("r"));
    }

    #[tokio::test]
    async fn test_simultaneous_disconnect_two_peers() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();
        let (mut c, _hc) = ctx.connect_peer();

        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "r".into(),
            display_name: None,
        });
        a.recv().await;
        b.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "r".into(),
            display_name: None,
        });
        b.recv().await;
        a.recv().await;
        c.send(&SdpMessage::Join {
            participant: pid(3),
            room_id: "r".into(),
            display_name: None,
        });
        c.recv().await;
        a.recv().await;
        b.recv().await;

        b.disconnect();
        c.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let msgs = a.drain().await;
        let left_b = msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(2))
            } else {
                false
            }
        });
        let left_c = msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(3))
            } else {
                false
            }
        });
        assert!(left_b, "A should get RoomStatus without B");
        assert!(left_c, "A should get RoomStatus without C");
    }

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

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        b.drain().await;
        c.drain().await;

        b.send(&SdpMessage::VideoConfigChanged {
            from: pid(2),
            room_id: "r".into(),
            width: 1280,
            height: 720,
            fps: 30,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let a_msgs = a.drain().await;
        assert!(
            a_msgs.iter().any(
                |m| matches!(m, SdpMessage::VideoConfigChanged { from, .. } if *from == pid(2))
            ),
            "A should still receive messages from non-muted B"
        );

        let c_msgs = c.drain().await;
        assert!(
            c_msgs.iter().any(
                |m| matches!(m, SdpMessage::VideoConfigChanged { from, .. } if *from == pid(2))
            ),
            "C should still receive messages from non-muted B"
        );
    }

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

        a.send(&SdpMessage::ScreenShareStarted {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        b.send(&SdpMessage::ScreenShareStarted {
            from: pid(2),
            room_id: "r".into(),
            track_id: 2,
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let c_msgs = c.drain().await;
        let from_a = c_msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::ScreenShareStarted { from, .. } if *from == pid(1)));
        let from_b = c_msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::ScreenShareStarted { from, .. } if *from == pid(2)));
        assert!(from_a, "C should see A's screen share");
        assert!(from_b, "C should see B's screen share");
    }

    #[tokio::test]
    async fn test_peer_reconnects_after_disconnect() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b1, _hb1) = ctx.connect_peer();

        a.send(&SdpMessage::Join {
            participant: pid(1),
            room_id: "r".into(),
            display_name: None,
        });
        a.recv().await;
        b1.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "r".into(),
            display_name: None,
        });
        b1.recv().await;
        a.recv().await;

        b1.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        a.drain().await;

        let (mut b2, _hb2) = ctx.connect_peer();
        b2.send(&SdpMessage::Join {
            participant: pid(2),
            room_id: "r".into(),
            display_name: Some("Bob v2".into()),
        });
        let ready = b2.recv().await;
        match &ready {
            SdpMessage::Ready { peers, .. } => {
                assert_eq!(peers.len(), 1);
                assert!(peers.contains(&pid(1)));
            }
            _ => panic!("expected Ready"),
        }

        // A receives room_status with B (pid(2)) present and display_name "Bob v2"
        let msg = a.recv().await;
        match &msg {
            SdpMessage::RoomStatus { participants, .. } => {
                let b_status = participants.iter().find(|p| p.id == pid(2));
                assert!(b_status.is_some(), "B should be in room_status");
                assert_eq!(b_status.unwrap().display_name.as_deref(), Some("Bob v2"));
            }
            _ => panic!("expected RoomStatus, got {:?}", msg),
        }
    }

    #[tokio::test]
    async fn test_rapid_mute_unmute_10_times() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        // Drain join-related messages (PeerJoined, RoomStatus from join)
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        a.drain().await;
        b.drain().await;

        for _ in 0..10 {
            a.send(&SdpMessage::MuteAudio {
                from: pid(1),
                room_id: "r".into(),
            });
            a.send(&SdpMessage::UnmuteAudio {
                from: pid(1),
                room_id: "r".into(),
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let msgs = b.drain().await;
        // Each mute/unmute now produces a RoomStatus. Count RoomStatus messages
        // where pid(1) is audio_muted=true and audio_muted=false respectively.
        let mutes = msgs
            .iter()
            .filter(|m| {
                if let SdpMessage::RoomStatus { participants, .. } = m {
                    participants.iter().any(|p| p.id == pid(1) && p.audio_muted)
                } else {
                    false
                }
            })
            .count();
        let unmutes = msgs
            .iter()
            .filter(|m| {
                if let SdpMessage::RoomStatus { participants, .. } = m {
                    participants
                        .iter()
                        .any(|p| p.id == pid(1) && !p.audio_muted)
                } else {
                    false
                }
            })
            .count();
        assert_eq!(
            mutes, 10,
            "should receive 10 room_status with audio_muted=true"
        );
        assert_eq!(
            unmutes, 10,
            "should receive 10 room_status with audio_muted=false"
        );
    }

    #[tokio::test]
    async fn test_mute_in_room_alone() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let msgs = a.drain().await;
        assert!(!msgs
            .iter()
            .any(|m| matches!(m, SdpMessage::MuteAudio { .. })));
    }

    #[tokio::test]
    async fn test_screen_share_in_room_alone() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;

        a.send(&SdpMessage::ScreenShareStarted {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let msgs = a.drain().await;
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, SdpMessage::ScreenShareStarted { .. })),
            "A should NOT receive its own screen share"
        );
    }

    #[tokio::test]
    async fn test_full_lifecycle_all_features() {
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "r").await;
        join_and_register(&ctx, &mut b, pid(2), "r").await;
        a.recv().await;

        a.send(&SdpMessage::VideoConfigChanged {
            from: pid(1),
            room_id: "r".into(),
            width: 1920,
            height: 1080,
            fps: 30,
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::ScreenShareStarted {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::MuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::UnmuteAudio {
            from: pid(1),
            room_id: "r".into(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::VideoConfigChanged {
            from: pid(1),
            room_id: "r".into(),
            width: 1280,
            height: 720,
            fps: 60,
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.send(&SdpMessage::ScreenShareStopped {
            from: pid(1),
            room_id: "r".into(),
            track_id: 1,
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        a.disconnect();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let msgs = b.drain().await;
        let types: Vec<&str> = msgs
            .iter()
            .map(|m| match m {
                SdpMessage::VideoConfigChanged { .. } => "config",
                SdpMessage::ScreenShareStarted { .. } => "screen_start",
                SdpMessage::ScreenShareStopped { .. } => "screen_stop",
                SdpMessage::PeerLeft { .. } => "left",
                SdpMessage::RoomStatus { .. } => "room_status",
                _ => "other",
            })
            .collect();

        assert!(types.contains(&"config"), "missing config in {:?}", types);
        assert!(
            types.contains(&"screen_start"),
            "missing screen_start in {:?}",
            types
        );
        // Mute/unmute state is now delivered via room_status snapshots
        assert!(
            types.contains(&"room_status"),
            "missing room_status in {:?}",
            types
        );
        // Verify the room_status messages contain the expected audio states
        let has_muted = msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(1) && p.audio_muted)
            } else {
                false
            }
        });
        let has_unmuted = msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants
                    .iter()
                    .any(|p| p.id == pid(1) && !p.audio_muted)
            } else {
                false
            }
        });
        assert!(has_muted, "missing room_status with audio_muted=true");
        assert!(has_unmuted, "missing room_status with audio_muted=false");
        assert!(
            types.contains(&"screen_stop"),
            "missing screen_stop in {:?}",
            types
        );
        // A's departure is notified via room_status (no longer PeerLeft)
        let has_a_gone = msgs.iter().any(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(1))
            } else {
                false
            }
        });
        assert!(has_a_gone, "missing room_status without A after disconnect");

        let config_count = types.iter().filter(|&&t| t == "config").count();
        assert_eq!(config_count, 2, "should have 2 config changes");

        let screen_stop_count = types.iter().filter(|&&t| t == "screen_stop").count();
        assert!(screen_stop_count >= 1, "should have at least 1 screen_stop");
    }

    // ── Reconnect ghost tile tests ────────────────────────────────

    #[tokio::test]
    async fn test_reconnect_new_uuid_does_not_see_ghost_tile() {
        // Simulate: user "bb" (pid=2) is in a room with "aaa" (pid=1).
        // "bb" refreshes → old WS drops, new WS connects with pid=3 (new UUID).
        // Before old disconnect handler runs, new pid=3 joins.
        // The Ready message for pid=3 must NOT include pid=2 (stale).
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b_old, _hb) = ctx.connect_peer();

        // Both join room.
        join_and_register(&ctx, &mut a, pid(1), "room1").await;
        join_and_register(&ctx, &mut b_old, pid(2), "room1").await;
        a.drain().await; // clear peer_joined etc.

        // "bb" drops old WS (simulates tab close/refresh).
        b_old.disconnect();
        // Small yield so the channel close propagates.
        tokio::task::yield_now().await;

        // New "bb" connects with a new UUID before old disconnect handler runs.
        let (mut b_new, _hb2) = ctx.connect_peer();
        b_new.send(&SdpMessage::Join {
            participant: pid(3),
            room_id: "room1".into(),
            display_name: Some("bb".into()),
        });
        let ready = b_new.recv().await;
        match ready {
            SdpMessage::Ready { peers, .. } => {
                // pid=2 (old "bb") must NOT be in the peers list.
                assert!(
                    !peers.contains(&pid(2)),
                    "stale peer pid=2 should have been pruned, but peers = {:?}",
                    peers
                );
                // pid=1 ("aaa") should be present.
                assert!(
                    peers.contains(&pid(1)),
                    "active peer pid=1 should be in peers list"
                );
            }
            other => panic!("expected Ready, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_reconnect_room_status_ordering() {
        // When "bb" refreshes (UUID-2 → UUID-3), "aaa" (pid=1) must receive
        // a room_status WITHOUT the old bb before a room_status WITH the new bb,
        // so the frontend frees the transceiver slot before allocating a new one.
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b_old, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "room1").await;
        join_and_register(&ctx, &mut b_old, pid(2), "room1").await;
        a.drain().await;

        // "bb" drops old WS.
        b_old.disconnect();
        tokio::task::yield_now().await;

        // New "bb" joins with a new UUID.
        let (mut b_new, _hb2) = ctx.connect_peer();
        b_new.send(&SdpMessage::Join {
            participant: pid(3),
            room_id: "room1".into(),
            display_name: Some("bb".into()),
        });
        b_new.recv_skip_status().await;

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Collect all messages "aaa" received.
        let msgs = a.drain().await;

        // Find first room_status without old bb (pid=2), then first with new bb (pid=3).
        let without_old = msgs.iter().position(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                !participants.iter().any(|p| p.id == pid(2))
            } else {
                false
            }
        });
        let with_new = msgs.iter().position(|m| {
            if let SdpMessage::RoomStatus { participants, .. } = m {
                participants.iter().any(|p| p.id == pid(3))
            } else {
                false
            }
        });
        assert!(
            without_old.is_some(),
            "aaa should receive room_status without old bb, got: {:?}",
            msgs
        );
        assert!(
            with_new.is_some(),
            "aaa should receive room_status with new bb, got: {:?}",
            msgs
        );
        assert!(
            without_old.unwrap() <= with_new.unwrap(),
            "room_status without old bb must arrive BEFORE/WITH room_status with new bb"
        );
    }

    #[tokio::test]
    async fn test_prune_stale_does_not_remove_active_peers() {
        // Verify that prune_stale only removes closed connections,
        // not active ones.
        let ctx = TestCtx::new().await;
        let (mut a, _ha) = ctx.connect_peer();
        let (mut b, _hb) = ctx.connect_peer();

        join_and_register(&ctx, &mut a, pid(1), "room1").await;
        join_and_register(&ctx, &mut b, pid(2), "room1").await;
        a.drain().await;
        b.drain().await;

        // Third peer joins — both pid=1 and pid=2 should appear (neither is stale).
        let (mut c, _hc) = ctx.connect_peer();
        c.send(&SdpMessage::Join {
            participant: pid(3),
            room_id: "room1".into(),
            display_name: Some("cc".into()),
        });
        let ready = c.recv().await;
        match ready {
            SdpMessage::Ready { peers, .. } => {
                assert!(peers.contains(&pid(1)), "pid=1 should be present");
                assert!(peers.contains(&pid(2)), "pid=2 should be present");
                assert_eq!(peers.len(), 2);
            }
            other => panic!("expected Ready, got {:?}", other),
        }
    }
}
