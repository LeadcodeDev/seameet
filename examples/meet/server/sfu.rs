use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use seameet::ParticipantId;
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

pub struct SfuPeer {
    pub cmd_tx: mpsc::UnboundedSender<PeerCmd>,
    pub ws_tx: mpsc::UnboundedSender<String>,
}

pub type Peers = Arc<RwLock<HashMap<ParticipantId, SfuPeer>>>;
/// Maps remote UDP address → ParticipantId for efficient packet routing.
pub type RouteTable = Arc<RwLock<HashMap<SocketAddr, ParticipantId>>>;

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
) {
    let mut own_audio_mid: Option<Mid> = None;
    let mut own_video_mid: Option<Mid> = None;
    let mut own_screen_mid: Option<Mid> = None;
    let mut screen_share_active = false;
    // Track the camera video SSRC to distinguish from screen share.
    // The first video SSRC we see is always camera (screen comes later).
    let mut camera_video_ssrc = None;
    let mut media_started = false;
    let mut ice_connected = false;
    let mut keyframes_requested_on_connect = false;
    let mut route_registered = false;

    let mut source_slots: HashMap<ParticipantId, SourceSlot> = HashMap::new();
    let mut last_pli = Instant::now();
    let mut all_mids: Vec<(Mid, MediaKind)> = Vec::new();
    let mut rtp_rx_count: u64 = 0;
    let mut rtp_tx_count: u64 = 0;

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
        // Ensure str0m processes pending state (timers, SDP events) even
        // if no UDP input has arrived yet.  Without this, MediaAdded events
        // may never fire because poll_output returns Timeout immediately.
        let _ = rtc.handle_input(Input::Timeout(Instant::now()));

        let mut new_media = Vec::new();
        let timeout = drain_outputs(
            &mut rtc,
            &socket,
            &pid,
            &peers,
            own_audio_pt,
            own_screen_mid,
            &mut camera_video_ssrc,
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
                // NOTE: own_screen_mid is NOT set eagerly here.
                // Receive-side video mids (for watching other peers) would be
                // misidentified as screen share. Instead, own_screen_mid is
                // detected dynamically in drain_outputs when we receive RTP
                // on a video mid that isn't own_video_mid.
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
                                MediaKind::Video if screen_share_active && own_screen_mid.is_none() => {
                                    own_screen_mid = Some(*mid);
                                    info!(participant = %pid, ?mid, "screen share mid set from renegotiation");
                                }
                                _ => {}
                            }
                        }

                        // Log slot state after renegotiation (don't remove slots —
                        // that would reset seq counters and break SRTP).
                        info!(
                            participant = %pid,
                            existing_slots = source_slots.len(),
                            new_mids = new_mids.len(),
                            total_mids = all_mids.len(),
                            "renegotiation complete — source slots preserved"
                        );

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
                        if active && own_screen_mid.is_none() {
                            // The renegotiation that added the screen transceiver
                            // likely already completed before this signal arrived.
                            // Find the screen mid retroactively: it's the last video
                            // mid in all_mids that isn't own_video_mid.
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
                            info!(participant = %pid, ?own_screen_mid, "screen share mid found retroactively");
                        }
                        if !active {
                            own_screen_mid = None;
                        }
                        info!(participant = %pid, active, ?own_screen_mid, "screen share state changed");
                    }
                    None => break,
                }
            }

            _ = tokio::time::sleep(wait) => {
                let _ = rtc.handle_input(Input::Timeout(Instant::now()));

                if media_started && last_pli.elapsed() >= Duration::from_secs(2) {
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
    own_screen_mid: Option<Mid>,
    camera_video_ssrc: &mut Option<u32>,
    rtp_rx_count: &mut u64,
    new_media: &mut Vec<(Mid, MediaKind)>,
    ice_connected: &mut bool,
) -> Option<Instant> {
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
                    let is_audio_pt = *pkt.header.payload_type == own_audio_pt;

                    // Identify screen share by SSRC, not by mid lookup.
                    // stream_rx(&ssrc) can return wrong mid after renegotiation,
                    // so we track the camera SSRC instead: the first video SSRC
                    // is always camera; any different video SSRC when screen share
                    // is active must be the screen track.
                    let ssrc_raw: u32 = (*pkt.header.ssrc).into();
                    if !is_audio_pt && camera_video_ssrc.is_none() {
                        *camera_video_ssrc = Some(ssrc_raw);
                    }
                    let is_screen = !is_audio_pt
                        && own_screen_mid.is_some()
                        && camera_video_ssrc.map_or(false, |cam| ssrc_raw != cam);

                    if *rtp_rx_count <= 5 || *rtp_rx_count % 500 == 0 {
                        info!(
                            participant = %pid,
                            pt = *pkt.header.payload_type,
                            is_audio = is_audio_pt,
                            is_screen,
                            marker = pkt.header.marker,
                            len = pkt.payload.len(),
                            total = *rtp_rx_count,
                            "RTP packet received from browser"
                        );
                    }

                    let forward = ForwardedMedia {
                        pt: *pkt.header.payload_type,
                        seq_no: (*pkt.seq_no).into(),
                        time: pkt.header.timestamp,
                        marker: pkt.header.marker,
                        payload: pkt.payload,
                        is_audio: is_audio_pt,
                        is_screen,
                        source_pid: *pid,
                    };

                    let p = peers.read().await;
                    for (id, peer) in p.iter() {
                        if id != pid {
                            let _ = peer.cmd_tx.send(PeerCmd::Media(ForwardedMedia {
                                pt: forward.pt,
                                seq_no: forward.seq_no,
                                time: forward.time,
                                marker: forward.marker,
                                payload: forward.payload.clone(),
                                is_audio: forward.is_audio,
                                is_screen: forward.is_screen,
                                source_pid: forward.source_pid,
                            }));
                        }
                    }
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
                    // Reset ice_connected on ICE restart (Checking/New state).
                    use str0m::IceConnectionState;
                    if matches!(state, IceConnectionState::Checking | IceConnectionState::New) {
                        *ice_connected = false;
                    }
                }
                Event::RawPacket(_) => {}
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
        Instant::now(),
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
