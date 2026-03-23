use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use seameet::{ParticipantId, SdpMessage};
use str0m::change::SdpOffer;
use str0m::media::{MediaKind, Mid};
use str0m::net::Protocol;
use str0m::rtp::ExtensionValues;
use str0m::{Candidate, Event, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, warn};

pub enum PeerCmd {
    UdpPacket { data: Vec<u8>, source: SocketAddr },
    IceCandidate(String),
    Media(ForwardedMedia),
    RequestKeyframe,
    RenegotiationOffer {
        sdp: String,
        reply_tx: oneshot::Sender<String>,
    },
    ScreenShareActive(bool),
    /// Notify this peer's media task that it has been muted/unmuted.
    SetMuted(bool),
    /// Notify this peer that a remote peer changed mute state.
    PeerMuteChanged {
        pid: ParticipantId,
        muted: bool,
    },
    /// Notify this peer's media task about the current remote peer count.
    PeerCountChanged {
        remote_peer_count: usize,
    },
}

pub struct ForwardedMedia {
    pub pt: u8,
    pub seq_no: u64,
    pub time: u32,
    pub marker: bool,
    pub payload: Vec<u8>,
    pub is_audio: bool,
    pub is_screen: bool,
    pub source_pid: ParticipantId,
    pub wallclock: Instant,
}

pub struct SourceSlot {
    audio_mid: Mid,
    video_mid: Mid,
    screen_mid: Option<Mid>,
    audio_tx_seq: u64,
    video_tx_seq: u64,
    screen_tx_seq: u64,
    audio_pt: u8,
    video_pt: u8,
}

#[derive(Clone)]
pub struct SfuPeer {
    pub cmd_tx: mpsc::UnboundedSender<PeerCmd>,
    pub ws_tx: mpsc::UnboundedSender<String>,
}

pub type Peers = Arc<RwLock<HashMap<ParticipantId, SfuPeer>>>;
/// Maps remote UDP address → ParticipantId for efficient packet routing.
pub type RouteTable = Arc<RwLock<HashMap<SocketAddr, ParticipantId>>>;

/// Maximum number of media packets buffered per source while awaiting renegotiation.
const MEDIA_BUFFER_CAP: usize = 500;

// ── UDP reader (single socket, broadcast to all peers) ─────────────────

pub async fn udp_reader(socket: Arc<UdpSocket>, peers: Peers, routes: RouteTable) {
    let mut buf = vec![0u8; 65535];
    loop {
        let (n, source) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                warn!("UDP recv error: {e}");
                continue;
            }
        };
        let data = buf[..n].to_vec();

        // Try route table first (fast path for known peers).
        // Use try_read to never block the UDP reader.
        let target_pid = routes
            .try_read()
            .ok()
            .and_then(|r| r.get(&source).copied());

        let p = peers.read().await;
        if let Some(pid) = target_pid {
            // Known source → send to specific peer only.
            if let Some(peer) = p.get(&pid) {
                let _ = peer.cmd_tx.send(PeerCmd::UdpPacket { data, source });
            }
        } else {
            // Unknown source (initial STUN) → broadcast to all peers.
            for peer in p.values() {
                let _ = peer.cmd_tx.send(PeerCmd::UdpPacket {
                    data: data.clone(),
                    source,
                });
            }
        }
    }
}

// ── Media task (RTP mode) ───────────────────────────────────────────────

pub async fn run_media(
    mut rtc: Rtc,
    socket: Arc<UdpSocket>,
    local_addr: SocketAddr,
    pid: ParticipantId,
    mut cmd_rx: mpsc::UnboundedReceiver<PeerCmd>,
    peers: Peers,
    routes: RouteTable,
    own_audio_pt: u8,
    own_video_pt: u8,
    initial_mids: Vec<(Mid, MediaKind)>,
    ws_tx: mpsc::UnboundedSender<String>,
    room_id: String,
) {
    let mut own_audio_mid: Option<Mid> = None;
    let mut own_video_mid: Option<Mid> = None;
    let mut own_screen_mid: Option<Mid> = None;
    let mut screen_share_active = false;
    // When true, the next new video mid from renegotiation is assigned as screen mid.
    let mut screen_share_pending = false;
    let mut muted = false;
    let mut muted_peers: std::collections::HashSet<ParticipantId> = std::collections::HashSet::new();
    let mut media_started = false;
    let mut ice_connected = false;
    let mut keyframes_requested_on_connect = false;
    let mut route_registered = false;

    let mut source_slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
    let mut last_pli = Instant::now();
    let mut all_mids: Vec<(Mid, MediaKind)> = Vec::new();
    let mut rtp_rx_count: u64 = 0;
    let mut rtp_tx_count: u64 = 0;
    let mut pending_media: HashMap<ParticipantId, Vec<ForwardedMedia>> = HashMap::new();

    // Process mids collected right after accept_offer (before ICE/DTLS).
    for (mid, kind) in &initial_mids {
        all_mids.push((*mid, *kind));
        match kind {
            MediaKind::Audio if own_audio_mid.is_none() => {
                own_audio_mid = Some(*mid);
            }
            MediaKind::Video if own_video_mid.is_none() => {
                own_video_mid = Some(*mid);
            }
            _ => {}
        }
    }
    info!(
        participant = %pid,
        ?own_audio_mid, ?own_video_mid, ?own_screen_mid,
        total_mids = all_mids.len(),
        "initial mids processed"
    );

    loop {
        let _ = rtc.handle_input(Input::Timeout(Instant::now()));

        let mut new_media = Vec::new();
        let timeout = drain_outputs(
            &mut rtc,
            &socket,
            &pid,
            &peers,
            own_audio_pt,
            own_audio_mid,
            own_video_mid,
            own_screen_mid,
            muted,
            &muted_peers,
            &mut rtp_rx_count,
            &mut new_media,
            &mut ice_connected,
        )
        .await;

        // Process newly discovered media lines.
        for (mid, kind) in &new_media {
            if all_mids.iter().any(|(m, _)| m == mid) {
                continue;
            }
            all_mids.push((*mid, *kind));
            match kind {
                MediaKind::Audio if own_audio_mid.is_none() => {
                    own_audio_mid = Some(*mid);
                }
                MediaKind::Video if own_video_mid.is_none() => {
                    own_video_mid = Some(*mid);
                }
                _ => {}
            }
        }

        // Start media forwarding once we have own mids.
        if !media_started && own_audio_mid.is_some() && own_video_mid.is_some() {
            media_started = true;
            if let Some(mid) = own_video_mid {
                let mut api = rtc.direct_api();
                if let Some(rx) = api.stream_rx_by_mid(mid, None) {
                    rx.request_keyframe(str0m::media::KeyframeRequestKind::Pli);
                    info!(participant = %pid, "requested initial keyframe (PLI)");
                }
            }
            {
                let p = peers.read().await;
                for (id, peer) in p.iter() {
                    if *id != pid {
                        let _ = peer.cmd_tx.send(PeerCmd::RequestKeyframe);
                    }
                }
            }
        }

        // Request keyframes once ICE connects — media written before DTLS
        // was silently discarded, so we need fresh keyframes now.
        if ice_connected && media_started && !keyframes_requested_on_connect {
            keyframes_requested_on_connect = true;
            info!(participant = %pid, "ICE connected — requesting keyframes from all peers");
            if let Some(mid) = own_video_mid {
                let mut api = rtc.direct_api();
                if let Some(rx) = api.stream_rx_by_mid(mid, None) {
                    rx.request_keyframe(str0m::media::KeyframeRequestKind::Pli);
                }
            }
            let p = peers.read().await;
            for (id, peer) in p.iter() {
                if *id != pid {
                    let _ = peer.cmd_tx.send(PeerCmd::RequestKeyframe);
                }
            }
        }

        if !rtc.is_alive() {
            debug!(participant = %pid, "rtc no longer alive");
            break;
        }

        let wait = timeout
            .map(|t| t.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(5))
            .min(Duration::from_millis(5));

        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(PeerCmd::UdpPacket { data, source }) => {
                        if let Ok(receive) = str0m::net::Receive::new(
                            Protocol::Udp, source, local_addr, &data,
                        ) {
                            let _ = rtc.handle_input(Input::Receive(Instant::now(), receive));
                            if ice_connected && !route_registered {
                                routes.write().await.insert(source, pid);
                                route_registered = true;
                                info!(participant = %pid, %source, "registered UDP route");
                            }
                        }
                    }
                    Some(PeerCmd::IceCandidate(c)) => {
                        if let Ok(cand) = Candidate::from_sdp_string(&c) {
                            rtc.add_remote_candidate(cand);
                        }
                    }
                    Some(PeerCmd::Media(m)) => {
                        // Skip audio from muted peers.
                        if m.is_audio && muted_peers.contains(&m.source_pid) {
                            continue;
                        }
                        // Check if slot exists or can be created.
                        let has_slot = source_slots.contains_key(&m.source_pid)
                            || get_or_create_slot(
                                m.source_pid, &mut source_slots,
                                &all_mids, own_audio_mid, own_video_mid, own_screen_mid, own_audio_pt, own_video_pt,
                            ).is_some();

                        if !has_slot {
                            // Buffer media while waiting for renegotiation.
                            let buf = pending_media.entry(m.source_pid).or_default();
                            if buf.len() < MEDIA_BUFFER_CAP {
                                buf.push(m);
                            }
                            continue;
                        }

                        let created_new = !source_slots.contains_key(&m.source_pid);
                        let had_screen_mid = source_slots.get(&m.source_pid)
                            .and_then(|s| s.screen_mid).is_some();
                        write_forwarded_rtp(
                            &mut rtc, &m, &mut source_slots, &mut rtp_tx_count,
                            &all_mids, own_audio_mid, own_video_mid, own_screen_mid, own_audio_pt, own_video_pt,
                        );
                        // Request keyframe when we first create a slot OR
                        // when screen_mid was just set (initial keyframe was likely missed).
                        let needs_keyframe = (created_new && source_slots.contains_key(&m.source_pid))
                            || (!had_screen_mid && source_slots.get(&m.source_pid)
                                .and_then(|s| s.screen_mid).is_some());
                        if needs_keyframe {
                            let p = peers.read().await;
                            if let Some(peer) = p.get(&m.source_pid) {
                                let _ = peer.cmd_tx.send(PeerCmd::RequestKeyframe);
                            }
                        }
                    }
                    Some(PeerCmd::RequestKeyframe) => {
                        if let Some(mid) = own_video_mid {
                            let mut api = rtc.direct_api();
                            if let Some(rx) = api.stream_rx_by_mid(mid, None) {
                                rx.request_keyframe(str0m::media::KeyframeRequestKind::Pli);
                                info!(participant = %pid, "PLI requested by remote peer");
                            }
                        }
                        if let Some(mid) = own_screen_mid {
                            let mut api = rtc.direct_api();
                            if let Some(rx) = api.stream_rx_by_mid(mid, None) {
                                rx.request_keyframe(str0m::media::KeyframeRequestKind::Pli);
                            }
                        }
                    }
                    Some(PeerCmd::RenegotiationOffer { sdp, reply_tx }) => {
                        let new_mids = handle_renegotiation(
                            &mut rtc, &socket, &pid, sdp,
                            reply_tx, &mut all_mids,
                        );
                        // Update own mids if needed.
                        for (mid, kind) in &new_mids {
                            match kind {
                                MediaKind::Audio if own_audio_mid.is_none() => {
                                    own_audio_mid = Some(*mid);
                                }
                                MediaKind::Video if own_video_mid.is_none() => {
                                    own_video_mid = Some(*mid);
                                }
                                // Screen share: if pending or active, assign new video mid as screen.
                                MediaKind::Video if (screen_share_pending || screen_share_active) && own_screen_mid.is_none() => {
                                    own_screen_mid = Some(*mid);
                                    screen_share_pending = false;
                                    info!(participant = %pid, ?mid, "screen share mid set from renegotiation");
                                }
                                _ => {}
                            }
                        }

                        info!(
                            participant = %pid,
                            existing_slots = source_slots.len(),
                            new_mids = new_mids.len(),
                            total_mids = all_mids.len(),
                            "renegotiation complete — source slots preserved"
                        );

                        // Flush buffered media now that new mids are available.
                        if !pending_media.is_empty() {
                            let sources: Vec<ParticipantId> = pending_media.keys().copied().collect();
                            let mut flushed_count = 0usize;
                            for source_pid in sources {
                                if let Some(buffered) = pending_media.remove(&source_pid) {
                                    for m in &buffered {
                                        write_forwarded_rtp(
                                            &mut rtc, m, &mut source_slots, &mut rtp_tx_count,
                                            &all_mids, own_audio_mid, own_video_mid, own_screen_mid, own_audio_pt, own_video_pt,
                                        );
                                        flushed_count += 1;
                                    }
                                    // Request keyframe for flushed source.
                                    if source_slots.contains_key(&source_pid) {
                                        let p = peers.read().await;
                                        if let Some(peer) = p.get(&source_pid) {
                                            let _ = peer.cmd_tx.send(PeerCmd::RequestKeyframe);
                                        }
                                    }
                                }
                            }
                            if flushed_count > 0 {
                                info!(participant = %pid, flushed_count, "flushed buffered media after renegotiation");
                            }
                        }

                        // Request keyframes after renegotiation to avoid freeze.
                        if let Some(mid) = own_video_mid {
                            let mut api = rtc.direct_api();
                            if let Some(rx) = api.stream_rx_by_mid(mid, None) {
                                rx.request_keyframe(str0m::media::KeyframeRequestKind::Pli);
                            }
                        }
                        let p = peers.read().await;
                        for (id, peer) in p.iter() {
                            if *id != pid {
                                let _ = peer.cmd_tx.send(PeerCmd::RequestKeyframe);
                            }
                        }
                    }
                    Some(PeerCmd::ScreenShareActive(active)) => {
                        screen_share_active = active;
                        if active {
                            // Mark pending so that the next renegotiation's new video mid
                            // gets assigned as own_screen_mid.
                            if own_screen_mid.is_none() {
                                screen_share_pending = true;
                                // Also check retroactively: renegotiation may have already completed.
                                let used_mids: std::collections::HashSet<Mid> = source_slots
                                    .values()
                                    .flat_map(|s| {
                                        let mut v = vec![s.audio_mid, s.video_mid];
                                        if let Some(sm) = s.screen_mid { v.push(sm); }
                                        v
                                    })
                                    .collect();
                                own_screen_mid = all_mids.iter().rev()
                                    .find(|(mid, kind)| {
                                        matches!(kind, MediaKind::Video)
                                            && Some(*mid) != own_video_mid
                                            && !used_mids.contains(mid)
                                    })
                                    .map(|(mid, _)| *mid);
                                if own_screen_mid.is_some() {
                                    screen_share_pending = false;
                                }
                                info!(participant = %pid, ?own_screen_mid, "screen share mid lookup");
                            }
                        } else {
                            own_screen_mid = None;
                            screen_share_pending = false;
                        }
                        info!(participant = %pid, active, ?own_screen_mid, "screen share state changed");
                    }
                    Some(PeerCmd::SetMuted(is_muted)) => {
                        muted = is_muted;
                        info!(participant = %pid, muted, "mute state changed");
                    }
                    Some(PeerCmd::PeerMuteChanged { pid: peer_pid, muted: is_muted }) => {
                        if is_muted {
                            muted_peers.insert(peer_pid);
                        } else {
                            muted_peers.remove(&peer_pid);
                        }
                        info!(participant = %pid, peer = %peer_pid, muted = is_muted, "peer mute state updated");
                    }
                    Some(PeerCmd::PeerCountChanged { remote_peer_count }) => {
                        let own_count = 2 + if own_screen_mid.is_some() { 1 } else { 0 };
                        if let Some(deficit) = needs_renegotiation(
                            &all_mids, own_count, &source_slots, remote_peer_count,
                        ) {
                            let msg = SdpMessage::RequestRenegotiation {
                                room_id: room_id.clone(),
                                needed_slots: deficit,
                            };
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = ws_tx.send(json);
                            }
                            info!(
                                participant = %pid,
                                deficit,
                                remote_peer_count,
                                "requested renegotiation for more slots"
                            );
                        }
                    }
                    None => break,
                }
            }

            _ = tokio::time::sleep(wait) => {
                let _ = rtc.handle_input(Input::Timeout(Instant::now()));

                if media_started && last_pli.elapsed() >= Duration::from_secs(5) {
                    last_pli = Instant::now();
                    if let Some(mid) = own_video_mid {
                        let mut api = rtc.direct_api();
                        if let Some(rx) = api.stream_rx_by_mid(mid, None) {
                            rx.request_keyframe(str0m::media::KeyframeRequestKind::Pli);
                        }
                    }
                    if let Some(mid) = own_screen_mid {
                        let mut api = rtc.direct_api();
                        if let Some(rx) = api.stream_rx_by_mid(mid, None) {
                            rx.request_keyframe(str0m::media::KeyframeRequestKind::Pli);
                        }
                    }
                    let p = peers.read().await;
                    for (id, peer) in p.iter() {
                        if *id != pid {
                            let _ = peer.cmd_tx.send(PeerCmd::RequestKeyframe);
                        }
                    }
                }
            }
        }
    }

    peers.write().await.remove(&pid);
    info!(participant = %pid, "media task ended");
}

/// Returns the list of newly discovered (mid, kind) pairs from renegotiation.
pub fn handle_renegotiation(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    pid: &ParticipantId,
    sdp: String,
    reply_tx: oneshot::Sender<String>,
    all_mids: &mut Vec<(Mid, MediaKind)>,
) -> Vec<(Mid, MediaKind)> {
    let sdp = patch_sdp_directions(&sdp);

    let offer = match SdpOffer::from_sdp_string(&sdp) {
        Ok(o) => o,
        Err(e) => {
            warn!(participant = %pid, "renegotiation bad SDP: {e}");
            let _ = reply_tx.send(String::new());
            return vec![];
        }
    };

    let answer = match rtc.sdp_api().accept_offer(offer) {
        Ok(a) => a,
        Err(e) => {
            warn!(participant = %pid, "renegotiation accept_offer failed: {e}");
            let _ = reply_tx.send(String::new());
            return vec![];
        }
    };

    let answer_sdp = answer.to_sdp_string();

    // Log answer directions for debugging.
    for line in answer_sdp.lines() {
        if line.starts_with("m=")
            || line.starts_with("a=sendrecv")
            || line.starts_with("a=recvonly")
            || line.starts_with("a=sendonly")
            || line.starts_with("a=inactive")
        {
            debug!(participant = %pid, "renegotiation answer: {line}");
        }
    }

    // Parse mids from the SDP answer (don't wait for MediaAdded events).
    let sdp_mids = parse_mids_from_sdp(&answer_sdp);
    let mut new_mids: Vec<(Mid, MediaKind)> = Vec::new();
    for (mid, kind) in &sdp_mids {
        if !all_mids.iter().any(|(m, _)| m == mid) {
            all_mids.push((*mid, *kind));
            new_mids.push((*mid, *kind));
            info!(participant = %pid, ?mid, ?kind, "renegotiation: new mid from SDP");
        }
    }

    // Drain any pending Transmit events.
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

    let _ = reply_tx.send(answer_sdp);
    new_mids
}

async fn drain_outputs(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    pid: &ParticipantId,
    peers: &Peers,
    own_audio_pt: u8,
    _own_audio_mid: Option<Mid>,
    _own_video_mid: Option<Mid>,
    own_screen_mid: Option<Mid>,
    muted: bool,
    _muted_peers: &std::collections::HashSet<ParticipantId>,
    rtp_rx_count: &mut u64,
    new_media: &mut Vec<(Mid, MediaKind)>,
    ice_connected: &mut bool,
) -> Option<Instant> {
    // Resolve screen share SSRC from own_screen_mid via str0m's stream mapping.
    // This is the authoritative source — no heuristic needed.
    let screen_ssrc: Option<u32> = own_screen_mid.and_then(|mid| {
        let mut api = rtc.direct_api();
        api.stream_rx_by_mid(mid, None).map(|rx| (*rx.ssrc()).into())
    });

    loop {
        match rtc.poll_output() {
            Ok(Output::Transmit(t)) => {
                let _ = socket.try_send_to(&t.contents, t.destination);
            }
            Ok(Output::Event(event)) => match event {
                Event::Connected => {
                    *ice_connected = true;
                    info!(participant = %pid, "WebRTC CONNECTED");
                }
                Event::MediaAdded(m) => {
                    info!(participant = %pid, mid = ?m.mid, kind = ?m.kind, "media added (drain)");
                    new_media.push((m.mid, m.kind));
                }
                Event::RtpPacket(pkt) => {
                    *rtp_rx_count += 1;
                    let is_audio = *pkt.header.payload_type == own_audio_pt;

                    // If this peer is muted, skip forwarding audio entirely.
                    if is_audio && muted {
                        continue;
                    }

                    // Determine screen share by comparing SSRC against the
                    // screen mid's known SSRC (resolved from str0m).
                    let ssrc_raw: u32 = (*pkt.header.ssrc).into();
                    let is_screen = !is_audio
                        && screen_ssrc.map_or(false, |ss| ss == ssrc_raw);

                    if *rtp_rx_count <= 5 || *rtp_rx_count % 500 == 0 {
                        info!(
                            participant = %pid,
                            pt = *pkt.header.payload_type,
                            is_audio,
                            is_screen,
                            marker = pkt.header.marker,
                            len = pkt.payload.len(),
                            total = *rtp_rx_count,
                            "RTP packet received from browser"
                        );
                    }

                    let now = Instant::now();
                    let p = peers.read().await;
                    for (id, peer) in p.iter() {
                        if id != pid {
                            let _ = peer.cmd_tx.send(PeerCmd::Media(ForwardedMedia {
                                pt: *pkt.header.payload_type,
                                seq_no: (*pkt.seq_no).into(),
                                time: pkt.header.timestamp,
                                marker: pkt.header.marker,
                                payload: pkt.payload.clone(),
                                is_audio,
                                is_screen,
                                source_pid: *pid,
                                wallclock: now,
                            }));
                        }
                    }
                }
                Event::EgressBitrateEstimate(bwe) => {
                    info!(participant = %pid, bitrate = ?bwe, "BWE estimate");
                }
                Event::KeyframeRequest(_) => {
                    let p = peers.read().await;
                    for (id, peer) in p.iter() {
                        if id != pid {
                            let _ = peer.cmd_tx.send(PeerCmd::RequestKeyframe);
                        }
                    }
                }
                Event::IceConnectionStateChange(state) => {
                    debug!(participant = %pid, ?state, "ICE state");
                    use str0m::IceConnectionState;
                    if matches!(state, IceConnectionState::Checking | IceConnectionState::New) {
                        *ice_connected = false;
                    }
                }
                Event::RawPacket(_) => {
                    *rtp_rx_count += 1;
                    if *rtp_rx_count <= 20 || *rtp_rx_count % 1000 == 0 {
                        info!(participant = %pid, total = *rtp_rx_count, "RawPacket (unknown SSRC?)");
                    }
                }
                _ => {}
            },
            Ok(Output::Timeout(t)) => return Some(t),
            Err(e) => {
                debug!("poll_output error: {e}");
                return None;
            }
        }
    }
}

/// Determines whether a renegotiation is needed to accommodate the given remote peer count.
///
/// Returns `Some(deficit)` if more slots are needed, `None` otherwise.
pub fn needs_renegotiation(
    all_mids: &[(Mid, MediaKind)],
    own_count: usize,
    slots: &HashMap<ParticipantId, SourceSlot>,
    remote_peer_count: usize,
) -> Option<u32> {
    if remote_peer_count == 0 {
        return None;
    }

    // Count mids already used by existing slots.
    let used_mids: std::collections::HashSet<Mid> = slots
        .values()
        .flat_map(|s| {
            let mut v = vec![s.audio_mid, s.video_mid];
            if let Some(sm) = s.screen_mid {
                v.push(sm);
            }
            v
        })
        .collect();

    // Count free audio and video mids (excluding own mids).
    let free_audio = all_mids
        .iter()
        .filter(|(mid, kind)| {
            matches!(kind, MediaKind::Audio) && !used_mids.contains(mid)
        })
        .count()
        .saturating_sub(own_count.min(1)); // subtract 1 for own audio

    let free_video = all_mids
        .iter()
        .filter(|(mid, kind)| {
            matches!(kind, MediaKind::Video) && !used_mids.contains(mid)
        })
        .count()
        .saturating_sub(own_count.saturating_sub(1)); // subtract own video (+ screen if present)

    // Available slots = min of free audio and free video pairs.
    let slots_available = free_audio.min(free_video);
    // Peers that still need a slot.
    let peers_without_slot = remote_peer_count.saturating_sub(slots.len());

    if peers_without_slot > slots_available {
        Some((peers_without_slot - slots_available) as u32)
    } else {
        None
    }
}

fn get_or_create_slot<'a>(
    source_pid: ParticipantId,
    source_slots: &'a mut HashMap<ParticipantId, SourceSlot>,
    all_mids: &[(Mid, MediaKind)],
    own_audio_mid: Option<Mid>,
    own_video_mid: Option<Mid>,
    own_screen_mid: Option<Mid>,
    own_audio_pt: u8,
    own_video_pt: u8,
) -> Option<&'a mut SourceSlot> {
    if source_slots.contains_key(&source_pid) {
        // Update screen_mid if it was previously None and a free video mid is now available.
        let needs_screen = source_slots.get(&source_pid).unwrap().screen_mid.is_none();
        if needs_screen {
            let used_mids: std::collections::HashSet<Mid> = source_slots
                .values()
                .flat_map(|s| {
                    let mut v = vec![s.audio_mid, s.video_mid];
                    if let Some(sm) = s.screen_mid { v.push(sm); }
                    v
                })
                .collect();
            let free_screen = all_mids.iter().find(|(mid, kind)| {
                matches!(kind, MediaKind::Video)
                    && Some(*mid) != own_video_mid
                    && Some(*mid) != own_screen_mid
                    && !used_mids.contains(mid)
            });
            if let Some((s_mid, _)) = free_screen {
                let s_mid = *s_mid;
                let slot = source_slots.get_mut(&source_pid).unwrap();
                slot.screen_mid = Some(s_mid);
                info!(source = %source_pid, screen_mid = ?s_mid, "updated slot with screen mid");
            }
        }
        return source_slots.get_mut(&source_pid);
    }

    // Collect mids already used by existing slots.
    let used_mids: std::collections::HashSet<Mid> = source_slots
        .values()
        .flat_map(|s| {
            let mut v = vec![s.audio_mid, s.video_mid];
            if let Some(sm) = s.screen_mid { v.push(sm); }
            v
        })
        .collect();

    // Find a free audio mid and a free video mid.
    let free_audio = all_mids.iter().find(|(mid, kind)| {
        matches!(kind, MediaKind::Audio)
            && Some(*mid) != own_audio_mid
            && !used_mids.contains(mid)
    });
    let free_video = all_mids.iter().find(|(mid, kind)| {
        matches!(kind, MediaKind::Video)
            && Some(*mid) != own_video_mid
            && Some(*mid) != own_screen_mid
            && !used_mids.contains(mid)
    });

    let (Some((a_mid, _)), Some((v_mid, _))) = (free_audio, free_video) else {
        warn!(source = %source_pid, "no free mids for lazy slot creation");
        return None;
    };
    let a_mid = *a_mid;
    let v_mid = *v_mid;

    // Also try to find a free video mid for screen share.
    let free_screen = all_mids.iter().find(|(mid, kind)| {
        matches!(kind, MediaKind::Video)
            && Some(*mid) != own_video_mid
            && Some(*mid) != own_screen_mid
            && *mid != v_mid
            && !used_mids.contains(mid)
    });
    let screen_mid = free_screen.map(|(mid, _)| *mid);

    info!(
        source = %source_pid,
        ?a_mid, ?v_mid, ?screen_mid,
        "lazy-created source slot"
    );

    source_slots.insert(
        source_pid,
        SourceSlot {
            audio_mid: a_mid,
            video_mid: v_mid,
            screen_mid,
            audio_tx_seq: 0,
            video_tx_seq: 0,
            screen_tx_seq: 0,
            audio_pt: own_audio_pt,
            video_pt: own_video_pt,
        },
    );
    source_slots.get_mut(&source_pid)
}

fn write_forwarded_rtp(
    rtc: &mut Rtc,
    media: &ForwardedMedia,
    source_slots: &mut HashMap<ParticipantId, SourceSlot>,
    rtp_tx_count: &mut u64,
    all_mids: &[(Mid, MediaKind)],
    own_audio_mid: Option<Mid>,
    own_video_mid: Option<Mid>,
    own_screen_mid: Option<Mid>,
    own_audio_pt: u8,
    own_video_pt: u8,
) {
    let Some(slot) = get_or_create_slot(
        media.source_pid, source_slots,
        all_mids, own_audio_mid, own_video_mid, own_screen_mid, own_audio_pt, own_video_pt,
    ) else {
        return;
    };

    let (mid, seq, pt) = if media.is_audio {
        (slot.audio_mid, &mut slot.audio_tx_seq, slot.audio_pt)
    } else if media.is_screen {
        let Some(screen_mid) = slot.screen_mid else { return };
        (screen_mid, &mut slot.screen_tx_seq, slot.video_pt)
    } else {
        (slot.video_mid, &mut slot.video_tx_seq, slot.video_pt)
    };

    let current_seq = *seq;
    *seq += 1;

    let mut api = rtc.direct_api();
    let Some(stream_tx) = api.stream_tx_by_mid(mid, None) else {
        warn!(?mid, "TX stream not found for mid");
        return;
    };

    *rtp_tx_count += 1;
    if *rtp_tx_count <= 5 || *rtp_tx_count % 500 == 0 {
        info!(
            source = %media.source_pid,
            ?mid, pt, seq = current_seq,
            total = *rtp_tx_count,
            "forwarding RTP"
        );
    }

    if let Err(e) = stream_tx.write_rtp(
        pt.into(),
        current_seq.into(),
        media.time,
        media.wallclock,
        media.marker,
        ExtensionValues::default(),
        false,
        media.payload.clone(),
    ) {
        warn!("write_rtp error: {e}");
    }
}

/// Parse (mid, MediaKind) pairs from the SDP answer.
/// This is more reliable than waiting for str0m's MediaAdded events,
/// which only fire after DTLS completes (Event::Connected).
pub fn parse_mids_from_sdp(sdp: &str) -> Vec<(Mid, MediaKind)> {
    let mut mids = Vec::new();
    let mut current_kind: Option<MediaKind> = None;
    for line in sdp.lines() {
        let line = line.trim();
        if line.starts_with("m=audio") {
            current_kind = Some(MediaKind::Audio);
        } else if line.starts_with("m=video") {
            current_kind = Some(MediaKind::Video);
        } else if let Some(kind) = current_kind {
            if let Some(mid_str) = line.strip_prefix("a=mid:") {
                let mid = Mid::from(mid_str.trim());
                mids.push((mid, kind));
                current_kind = None; // consumed
            }
        }
    }
    mids
}

pub fn parse_pts_from_sdp(sdp: &str) -> (Option<u8>, Option<u8>) {
    let mut audio_pt: Option<u8> = None;
    let mut video_pt: Option<u8> = None;
    for line in sdp.lines() {
        if line.starts_with("m=audio") && audio_pt.is_none() {
            audio_pt = line.split_whitespace().nth(3).and_then(|s| s.parse().ok());
        } else if line.starts_with("m=video") && video_pt.is_none() {
            video_pt = line.split_whitespace().nth(3).and_then(|s| s.parse().ok());
        }
    }
    (audio_pt, video_pt)
}

/// Replace `a=recvonly` with `a=sendrecv` only in active m-sections (port != 0).
/// Disabled m-sections (port=0, from stopped transceivers) are left untouched.
pub fn patch_sdp_directions(sdp: &str) -> String {
    let mut result = String::with_capacity(sdp.len());
    let mut m_section_active = true;
    for line in sdp.lines() {
        if line.starts_with("m=") {
            // m=audio 0 ... → disabled; m=audio 9 ... → active
            m_section_active = line
                .split_whitespace()
                .nth(1)
                .map(|port| port != "0")
                .unwrap_or(true);
        }
        if m_section_active && line == "a=recvonly" {
            result.push_str("a=sendrecv");
        } else {
            result.push_str(line);
        }
        result.push_str("\r\n");
    }
    result
}

pub async fn local_ip() -> std::net::IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("bind temp socket");
    let _ = socket.connect("8.8.8.8:80").await;
    socket
        .local_addr()
        .map(|a| a.ip())
        .unwrap_or_else(|_| std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
}

#[cfg(test)]
mod tests {
    use super::*;
    use seameet::ParticipantId;
    use str0m::media::{MediaKind, Mid};

    fn pid(n: u128) -> ParticipantId {
        ParticipantId::new(uuid::Uuid::from_u128(n))
    }

    fn mid(s: &str) -> Mid {
        Mid::from(s)
    }

    // ── parse_mids_from_sdp ─────────────────────────────────────────

    #[test]
    fn test_parse_mids_from_sdp_basic() {
        let sdp = "\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=mid:1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=mid:2\r\n";
        let mids = parse_mids_from_sdp(sdp);
        assert_eq!(mids.len(), 3);
        assert_eq!(mids[0], (mid("0"), MediaKind::Audio));
        assert_eq!(mids[1], (mid("1"), MediaKind::Video));
        assert_eq!(mids[2], (mid("2"), MediaKind::Video));
    }

    #[test]
    fn test_parse_mids_from_sdp_empty() {
        assert!(parse_mids_from_sdp("").is_empty());
    }

    #[test]
    fn test_parse_mids_from_sdp_no_mid_line() {
        let sdp = "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n";
        assert!(parse_mids_from_sdp(sdp).is_empty());
    }

    // ── parse_pts_from_sdp ──────────────────────────────────────────

    #[test]
    fn test_parse_pts_from_sdp() {
        let sdp = "\
m=audio 9 UDP/TLS/RTP/SAVPF 111 112\r\n\
a=mid:0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96 97\r\n\
a=mid:1\r\n";
        let (audio, video) = parse_pts_from_sdp(sdp);
        assert_eq!(audio, Some(111));
        assert_eq!(video, Some(96));
    }

    #[test]
    fn test_parse_pts_from_sdp_empty() {
        let (audio, video) = parse_pts_from_sdp("");
        assert_eq!(audio, None);
        assert_eq!(video, None);
    }

    // ── patch_sdp_directions ────────────────────────────────────────

    #[test]
    fn test_patch_sdp_directions_active() {
        let sdp = "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=recvonly\r\n";
        let patched = patch_sdp_directions(sdp);
        assert!(patched.contains("a=sendrecv"));
        assert!(!patched.contains("a=recvonly"));
    }

    #[test]
    fn test_patch_sdp_directions_disabled_section() {
        let sdp = "m=audio 0 UDP/TLS/RTP/SAVPF 111\r\na=recvonly\r\n";
        let patched = patch_sdp_directions(sdp);
        assert!(patched.contains("a=recvonly"));
        assert!(!patched.contains("a=sendrecv"));
    }

    #[test]
    fn test_patch_sdp_directions_mixed() {
        let sdp = "\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=recvonly\r\n\
m=video 0 UDP/TLS/RTP/SAVPF 96\r\n\
a=recvonly\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=recvonly\r\n";
        let patched = patch_sdp_directions(sdp);
        let lines: Vec<&str> = patched.lines().collect();
        // First audio section (port=9) → sendrecv
        assert_eq!(lines[1], "a=sendrecv");
        // Second video section (port=0) → stays recvonly
        assert_eq!(lines[3], "a=recvonly");
        // Third video section (port=9) → sendrecv
        assert_eq!(lines[5], "a=sendrecv");
    }

    // ── get_or_create_slot ──────────────────────────────────────────

    fn make_mids() -> Vec<(Mid, MediaKind)> {
        vec![
            (mid("0"), MediaKind::Audio),  // own audio
            (mid("1"), MediaKind::Video),  // own video
            (mid("2"), MediaKind::Audio),  // slot for peer 1
            (mid("3"), MediaKind::Video),  // slot for peer 1
            (mid("4"), MediaKind::Audio),  // slot for peer 2
            (mid("5"), MediaKind::Video),  // slot for peer 2
            (mid("6"), MediaKind::Video),  // screen for peer 1
            (mid("7"), MediaKind::Video),  // screen for peer 2
        ]
    }

    #[test]
    fn test_slot_creation() {
        let all_mids = make_mids();
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_audio = Some(mid("0"));
        let own_video = Some(mid("1"));

        let slot = get_or_create_slot(
            pid(1), &mut slots, &all_mids,
            own_audio, own_video, None, 111, 96,
        );
        assert!(slot.is_some());
        let slot = slot.unwrap();
        assert_eq!(slot.audio_mid, mid("2"));
        assert_eq!(slot.video_mid, mid("3"));
        assert_eq!(slot.audio_pt, 111);
        assert_eq!(slot.video_pt, 96);
    }

    #[test]
    fn test_slot_reuse() {
        let all_mids = make_mids();
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_audio = Some(mid("0"));
        let own_video = Some(mid("1"));

        // Create slot
        get_or_create_slot(pid(1), &mut slots, &all_mids, own_audio, own_video, None, 111, 96);
        // Get same slot again
        let slot = get_or_create_slot(pid(1), &mut slots, &all_mids, own_audio, own_video, None, 111, 96);
        assert!(slot.is_some());
        assert_eq!(slot.unwrap().audio_mid, mid("2")); // same mid
        assert_eq!(slots.len(), 1); // still just one slot
    }

    #[test]
    fn test_slot_different_peers_get_different_mids() {
        let all_mids = make_mids();
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_audio = Some(mid("0"));
        let own_video = Some(mid("1"));

        get_or_create_slot(pid(1), &mut slots, &all_mids, own_audio, own_video, None, 111, 96);
        get_or_create_slot(pid(2), &mut slots, &all_mids, own_audio, own_video, None, 111, 96);

        assert_eq!(slots.len(), 2);
        let s1 = slots.get(&pid(1)).unwrap();
        let s2 = slots.get(&pid(2)).unwrap();
        assert_ne!(s1.audio_mid, s2.audio_mid);
        assert_ne!(s1.video_mid, s2.video_mid);
    }

    #[test]
    fn test_slot_screen_mid_assigned() {
        let all_mids = make_mids();
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_audio = Some(mid("0"));
        let own_video = Some(mid("1"));

        let slot = get_or_create_slot(
            pid(1), &mut slots, &all_mids,
            own_audio, own_video, None, 111, 96,
        ).unwrap();
        // Should have a screen mid from one of the free video mids
        assert!(slot.screen_mid.is_some());
    }

    #[test]
    fn test_slot_no_free_mids() {
        // Only own mids, no slots available
        let all_mids = vec![
            (mid("0"), MediaKind::Audio),
            (mid("1"), MediaKind::Video),
        ];
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();

        let slot = get_or_create_slot(
            pid(1), &mut slots, &all_mids,
            Some(mid("0")), Some(mid("1")), None, 111, 96,
        );
        assert!(slot.is_none());
    }

    // ── Slot exhaustion & 3rd peer overflow ─────────────────────────

    #[test]
    fn test_slot_exhaustion_third_peer() {
        // Peer 1 takes audio+video+screen, Peer 2 takes audio+video.
        // Need enough mids so Peer 2 can get a slot but Peer 3 cannot.
        // own(a,v) + peer1(a,v,screen_v) + peer2(a,v) = 7 mids needed.
        // With only 7 mids, peer 3 has no audio left.
        let all_mids = vec![
            (mid("0"), MediaKind::Audio),  // own audio
            (mid("1"), MediaKind::Video),  // own video
            (mid("2"), MediaKind::Audio),  // peer 1 audio
            (mid("3"), MediaKind::Video),  // peer 1 video
            (mid("4"), MediaKind::Video),  // peer 1 screen
            (mid("5"), MediaKind::Audio),  // peer 2 audio
            (mid("6"), MediaKind::Video),  // peer 2 video
        ];
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_a = Some(mid("0"));
        let own_v = Some(mid("1"));

        assert!(get_or_create_slot(pid(1), &mut slots, &all_mids, own_a, own_v, None, 111, 96).is_some());
        assert!(get_or_create_slot(pid(2), &mut slots, &all_mids, own_a, own_v, None, 111, 96).is_some());
        // Peer 3 — no free audio mid
        assert!(get_or_create_slot(pid(3), &mut slots, &all_mids, own_a, own_v, None, 111, 96).is_none());
    }

    // ── Slot reuse after peer removal ───────────────────────────────

    #[test]
    fn test_slot_freed_after_peer_removal() {
        let all_mids = vec![
            (mid("0"), MediaKind::Audio),
            (mid("1"), MediaKind::Video),
            (mid("2"), MediaKind::Audio),
            (mid("3"), MediaKind::Video),
        ];
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_a = Some(mid("0"));
        let own_v = Some(mid("1"));

        // Peer 1 takes the only available slot
        assert!(get_or_create_slot(pid(1), &mut slots, &all_mids, own_a, own_v, None, 111, 96).is_some());
        // No room for peer 2
        assert!(get_or_create_slot(pid(2), &mut slots, &all_mids, own_a, own_v, None, 111, 96).is_none());

        // Remove peer 1 → frees mid("2") and mid("3")
        slots.remove(&pid(1));

        // Now peer 2 can get a slot
        let slot = get_or_create_slot(pid(2), &mut slots, &all_mids, own_a, own_v, None, 111, 96);
        assert!(slot.is_some());
        let slot = slot.unwrap();
        assert_eq!(slot.audio_mid, mid("2"));
        assert_eq!(slot.video_mid, mid("3"));
    }

    // ── Screen mid upgrade ──────────────────────────────────────────

    #[test]
    fn test_slot_screen_mid_upgrade_on_new_mid() {
        // Initially no screen mid available
        let mut all_mids = vec![
            (mid("0"), MediaKind::Audio),
            (mid("1"), MediaKind::Video),
            (mid("2"), MediaKind::Audio),
            (mid("3"), MediaKind::Video),
        ];
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_a = Some(mid("0"));
        let own_v = Some(mid("1"));

        let slot = get_or_create_slot(pid(1), &mut slots, &all_mids, own_a, own_v, None, 111, 96).unwrap();
        assert!(slot.screen_mid.is_none()); // no free video mid for screen

        // A new video mid becomes available (renegotiation added it)
        all_mids.push((mid("4"), MediaKind::Video));

        // Re-access the slot — should pick up screen mid
        let slot = get_or_create_slot(pid(1), &mut slots, &all_mids, own_a, own_v, None, 111, 96).unwrap();
        assert_eq!(slot.screen_mid, Some(mid("4")));
    }

    // ── own_screen_mid exclusion ────────────────────────────────────

    #[test]
    fn test_slot_excludes_own_screen_mid() {
        let all_mids = make_mids();
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_a = Some(mid("0"));
        let own_v = Some(mid("1"));
        let own_s = Some(mid("6")); // reserve mid("6") as own screen

        let slot = get_or_create_slot(pid(1), &mut slots, &all_mids, own_a, own_v, own_s, 111, 96).unwrap();
        // Slot should NOT use mid("6") for its video or screen
        assert_ne!(slot.video_mid, mid("6"));
        assert_ne!(slot.screen_mid, Some(mid("6")));
    }

    // ── Multiple peers with screen mids ─────────────────────────────

    #[test]
    fn test_two_peers_both_get_screen_mids() {
        let all_mids = make_mids(); // 8 mids: own(a,v) + peer1(a,v,screen) + peer2(a,v,screen)
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_a = Some(mid("0"));
        let own_v = Some(mid("1"));

        get_or_create_slot(pid(1), &mut slots, &all_mids, own_a, own_v, None, 111, 96);
        get_or_create_slot(pid(2), &mut slots, &all_mids, own_a, own_v, None, 111, 96);

        let s1 = slots.get(&pid(1)).unwrap();
        let s2 = slots.get(&pid(2)).unwrap();

        // Both should have screen mids
        assert!(s1.screen_mid.is_some());
        assert!(s2.screen_mid.is_some());
        // Screen mids should be different
        assert_ne!(s1.screen_mid, s2.screen_mid);
        // No mid should be shared across slots
        let all_used: Vec<Mid> = vec![
            s1.audio_mid, s1.video_mid, s1.screen_mid.unwrap(),
            s2.audio_mid, s2.video_mid, s2.screen_mid.unwrap(),
        ];
        let unique: std::collections::HashSet<Mid> = all_used.iter().copied().collect();
        assert_eq!(unique.len(), all_used.len(), "all mids should be unique across slots");
    }

    // ── Seq number tracking ─────────────────────────────────────────

    #[test]
    fn test_slot_seq_numbers_start_at_zero() {
        let all_mids = make_mids();
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();

        let slot = get_or_create_slot(pid(1), &mut slots, &all_mids, Some(mid("0")), Some(mid("1")), None, 111, 96).unwrap();
        assert_eq!(slot.audio_tx_seq, 0);
        assert_eq!(slot.video_tx_seq, 0);
        assert_eq!(slot.screen_tx_seq, 0);
    }

    // ── PT values ───────────────────────────────────────────────────

    #[test]
    fn test_slot_inherits_correct_pts() {
        let all_mids = make_mids();
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();

        let slot = get_or_create_slot(pid(1), &mut slots, &all_mids, Some(mid("0")), Some(mid("1")), None, 111, 96).unwrap();
        assert_eq!(slot.audio_pt, 111);
        assert_eq!(slot.video_pt, 96);

        // Different PTs
        slots.clear();
        let slot = get_or_create_slot(pid(1), &mut slots, &all_mids, Some(mid("0")), Some(mid("1")), None, 109, 100).unwrap();
        assert_eq!(slot.audio_pt, 109);
        assert_eq!(slot.video_pt, 100);
    }

    // ── parse_mids_from_sdp edge cases ──────────────────────────────

    #[test]
    fn test_parse_mids_from_sdp_many_sections() {
        // Simulate a large SDP with 7 peer slots (1 audio + 1 video each) + own
        let mut sdp = String::new();
        for i in 0..15 {
            let kind = if i % 2 == 0 { "audio" } else { "video" };
            sdp.push_str(&format!("m={kind} 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:{i}\r\n"));
        }
        let mids = parse_mids_from_sdp(&sdp);
        assert_eq!(mids.len(), 15);
    }

    #[test]
    fn test_parse_mids_from_sdp_disabled_sections() {
        let sdp = "\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:0\r\n\
m=video 0 UDP/TLS/RTP/SAVPF 96\r\n\
a=mid:1\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=mid:2\r\n";
        let mids = parse_mids_from_sdp(sdp);
        // parse_mids_from_sdp doesn't filter disabled sections — it parses all
        assert_eq!(mids.len(), 3);
    }

    #[test]
    fn test_parse_pts_from_sdp_multiple_audio_video() {
        // Only first audio and first video PT should be returned
        let sdp = "\
m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
a=mid:0\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
a=mid:1\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 109\r\n\
a=mid:2\r\n\
m=video 9 UDP/TLS/RTP/SAVPF 100\r\n\
a=mid:3\r\n";
        let (audio, video) = parse_pts_from_sdp(sdp);
        assert_eq!(audio, Some(111)); // first audio
        assert_eq!(video, Some(96));  // first video
    }

    // ── patch_sdp_directions edge cases ─────────────────────────────

    #[test]
    fn test_patch_sdp_directions_sendrecv_untouched() {
        let sdp = "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=sendrecv\r\n";
        let patched = patch_sdp_directions(sdp);
        assert!(patched.contains("a=sendrecv"));
    }

    #[test]
    fn test_patch_sdp_directions_sendonly_untouched() {
        let sdp = "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=sendonly\r\n";
        let patched = patch_sdp_directions(sdp);
        assert!(patched.contains("a=sendonly"));
        assert!(!patched.contains("a=sendrecv"));
    }

    #[test]
    fn test_patch_sdp_directions_no_m_section() {
        let sdp = "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n";
        let patched = patch_sdp_directions(sdp);
        assert_eq!(patched, "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n");
    }

    // ── Slot with only audio free (no video) ────────────────────────

    #[test]
    fn test_slot_fails_when_only_audio_free() {
        let all_mids = vec![
            (mid("0"), MediaKind::Audio),
            (mid("1"), MediaKind::Video),
            (mid("2"), MediaKind::Audio), // free audio, but no free video
        ];
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();

        let slot = get_or_create_slot(
            pid(1), &mut slots, &all_mids,
            Some(mid("0")), Some(mid("1")), None, 111, 96,
        );
        assert!(slot.is_none(), "should fail: free audio but no free video");
    }

    #[test]
    fn test_slot_fails_when_only_video_free() {
        let all_mids = vec![
            (mid("0"), MediaKind::Audio),
            (mid("1"), MediaKind::Video),
            (mid("2"), MediaKind::Video), // free video, but no free audio
        ];
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();

        let slot = get_or_create_slot(
            pid(1), &mut slots, &all_mids,
            Some(mid("0")), Some(mid("1")), None, 111, 96,
        );
        assert!(slot.is_none(), "should fail: free video but no free audio");
    }

    // ── Slot churn: create, remove, create different peer ───────────

    #[test]
    fn test_slot_churn_cycle() {
        let all_mids = vec![
            (mid("0"), MediaKind::Audio),
            (mid("1"), MediaKind::Video),
            (mid("2"), MediaKind::Audio),
            (mid("3"), MediaKind::Video),
        ];
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_a = Some(mid("0"));
        let own_v = Some(mid("1"));

        // Cycle: create → remove → create different peer → remove → create first again
        for round in 0..3 {
            let peer = pid(round as u128 + 1);
            let slot = get_or_create_slot(peer, &mut slots, &all_mids, own_a, own_v, None, 111, 96);
            assert!(slot.is_some(), "round {round}: should get slot");
            assert_eq!(slots.len(), 1);
            slots.remove(&peer);
            assert_eq!(slots.len(), 0);
        }
    }

    // ── Concurrent slots with screen mid contention ─────────────────

    #[test]
    fn test_three_peers_limited_screen_mids() {
        // 3 peer slots: each needs audio+video. Extra video mids go to screen.
        // Peer 1 gets screen greedily from the extra video mids available at creation time.
        let all_mids = vec![
            (mid("0"), MediaKind::Audio),   // own audio
            (mid("1"), MediaKind::Video),   // own video
            (mid("2"), MediaKind::Audio),   // peer 1 audio
            (mid("3"), MediaKind::Video),   // peer 1 video
            (mid("4"), MediaKind::Audio),   // peer 2 audio
            (mid("5"), MediaKind::Video),   // peer 2 video
            (mid("6"), MediaKind::Audio),   // peer 3 audio
            (mid("7"), MediaKind::Video),   // peer 3 video
            (mid("8"), MediaKind::Video),   // extra screen video 1
            (mid("9"), MediaKind::Video),   // extra screen video 2
            (mid("10"), MediaKind::Video),  // extra screen video 3
        ];
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_a = Some(mid("0"));
        let own_v = Some(mid("1"));

        get_or_create_slot(pid(1), &mut slots, &all_mids, own_a, own_v, None, 111, 96);
        get_or_create_slot(pid(2), &mut slots, &all_mids, own_a, own_v, None, 111, 96);
        get_or_create_slot(pid(3), &mut slots, &all_mids, own_a, own_v, None, 111, 96);

        assert_eq!(slots.len(), 3, "all 3 peers should get slots");
        // All peers should get screen mids (one each from the 3 extra video mids)
        let screens: Vec<_> = slots.values().filter(|s| s.screen_mid.is_some()).collect();
        assert_eq!(screens.len(), 3, "each peer should get a screen mid");
    }

    // ── needs_renegotiation ───────────────────────────────────────────

    #[test]
    fn test_needs_renegotiation_sufficient_mids() {
        // 8 mids (own audio+video + 3 pairs for peers), 1 slot used, 2 remote peers
        let all_mids = make_mids();
        let mut slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        let own_a = Some(mid("0"));
        let own_v = Some(mid("1"));
        get_or_create_slot(pid(1), &mut slots, &all_mids, own_a, own_v, None, 111, 96);

        // 2 remote peers, 1 slot used, 1 free pair available → no deficit
        assert_eq!(needs_renegotiation(&all_mids, 2, &slots, 2), None);
    }

    #[test]
    fn test_needs_renegotiation_deficit() {
        // Only own mids + 1 slot pair. 3 remote peers but only 1 slot.
        let all_mids = vec![
            (mid("0"), MediaKind::Audio),
            (mid("1"), MediaKind::Video),
            (mid("2"), MediaKind::Audio),
            (mid("3"), MediaKind::Video),
        ];
        let slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();

        // 3 remote peers, 0 slots used, only 1 free pair → deficit = 2
        let deficit = needs_renegotiation(&all_mids, 2, &slots, 3);
        assert!(deficit.is_some());
        assert_eq!(deficit.unwrap(), 2);
    }

    #[test]
    fn test_needs_renegotiation_zero_peers() {
        let all_mids = make_mids();
        let slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
        assert_eq!(needs_renegotiation(&all_mids, 2, &slots, 0), None);
    }

    #[test]
    fn test_media_buffer_cap() {
        // Verify the buffer cap constant is what we expect.
        assert_eq!(MEDIA_BUFFER_CAP, 500);
    }
}
