use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::response::Html;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use seameet::{ParticipantId, SdpMessage};
use str0m::change::SdpOffer;
use str0m::media::{Mid, MediaKind};
use str0m::net::Protocol;
use str0m::rtp::{ExtensionValues, Ssrc};
use str0m::{Candidate, Event, Input, Output, Rtc, RtcConfig};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

// ── Types ───────────────────────────────────────────────────────────────

/// Command sent from the WS handler to the media task.
enum PeerCmd {
    IceCandidate(String),
    Media(ForwardedMedia),
}

/// Raw RTP packet forwarded between participants.
struct ForwardedMedia {
    pt: u8,
    seq_no: u64,
    time: u32,
    marker: bool,
    payload: Vec<u8>,
    is_audio: bool,
}

struct SfuPeer {
    cmd_tx: mpsc::UnboundedSender<PeerCmd>,
}

type Peers = Arc<Mutex<HashMap<ParticipantId, SfuPeer>>>;

const INDEX_HTML: &str = include_str!("../static/index.html");

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "browser_demo=debug,str0m=info".parse().expect("filter")),
        )
        .init();

    let peers: Peers = Arc::new(Mutex::new(HashMap::new()));

    let http = tokio::spawn(serve_http());
    let ws = tokio::spawn(serve_ws(peers));

    info!("HTTP  → http://localhost:3000");
    info!("WS    → ws://localhost:3001");

    let _ = tokio::join!(http, ws);
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

// ── WebSocket signaling ─────────────────────────────────────────────────

async fn serve_ws(peers: Peers) {
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
        let peers = Arc::clone(&peers);
        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    warn!(%peer_addr, "WS handshake failed: {e}");
                    return;
                }
            };
            info!(%peer_addr, "WS connected");
            handle_ws(ws, peers).await;
        });
    }
}

async fn handle_ws(
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    peers: Peers,
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
                warn!("WS read error: {e}");
                break;
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
            SdpMessage::Join { participant, .. } => {
                participant_id = Some(participant);
                let ready = SdpMessage::Ready {
                    room_id: "sfu".into(),
                    initiator: true,
                };
                if let Ok(json) = serde_json::to_string(&ready) {
                    let _ = tx.send(json);
                }
                info!(participant = %participant, "joined");
            }

            SdpMessage::Offer { sdp, .. } => {
                let Some(pid) = participant_id else { continue };

                let socket = match UdpSocket::bind("0.0.0.0:0").await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("UDP bind failed: {e}");
                        continue;
                    }
                };
                let local_port = match socket.local_addr() {
                    Ok(a) => a.port(),
                    Err(_) => continue,
                };

                let local_ip = local_ip().await;
                let local_addr: SocketAddr = (local_ip, local_port).into();

                // RTP mode: we get raw RTP packets, no depacketization.
                // Disable H264 to avoid PT 109 conflict (H264 RTX defaults to
                // PT 109, which Chrome/Firefox use for Opus audio).
                let mut config = RtcConfig::new()
                    .set_rtp_mode(true);
                config.codec_config().enable_h264(false);
                let mut rtc = config.build();

                if let Ok(c) = Candidate::host(local_addr, Protocol::Udp) {
                    rtc.add_local_candidate(c);
                }

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

                let answer_msg = SdpMessage::Answer {
                    from: pid,
                    to: pid,
                    room_id: "sfu".into(),
                    sdp: answer.to_sdp_string(),
                };
                if let Ok(json) = serde_json::to_string(&answer_msg) {
                    let _ = tx.send(json);
                }

                info!(participant = %pid, %local_addr, "SDP negotiated");

                let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
                peers.lock().await.insert(pid, SfuPeer { cmd_tx });

                let peers_clone = Arc::clone(&peers);
                tokio::spawn(async move {
                    run_media(rtc, socket, local_addr, pid, cmd_rx, peers_clone).await;
                });
            }

            SdpMessage::IceCandidate { candidate, .. } => {
                let Some(pid) = participant_id else { continue };
                let p = peers.lock().await;
                if let Some(peer) = p.get(&pid) {
                    let _ = peer.cmd_tx.send(PeerCmd::IceCandidate(candidate));
                }
            }

            _ => {}
        }
    }

    if let Some(pid) = participant_id {
        info!(participant = %pid, "disconnected");
        peers.lock().await.remove(&pid);
    }

    drop(tx);
    let _ = writer.await;
}

// ── Media task (RTP mode) ───────────────────────────────────────────────

async fn run_media(
    mut rtc: Rtc,
    socket: UdpSocket,
    local_addr: SocketAddr,
    pid: ParticipantId,
    mut cmd_rx: mpsc::UnboundedReceiver<PeerCmd>,
    peers: Peers,
) {
    let mut buf = vec![0u8; 65535];
    let mut audio_mid: Option<Mid> = None;
    let mut video_mid: Option<Mid> = None;
    // For RTP mode sending: we need SSRCs for outgoing streams.
    let mut audio_tx_ssrc: Option<Ssrc> = None;
    let mut video_tx_ssrc: Option<Ssrc> = None;
    let mut media_started = false;

    loop {
        let timeout = drain_outputs(
            &mut rtc, &socket, &pid, &peers,
            &mut audio_mid, &mut video_mid,
            &mut audio_tx_ssrc, &mut video_tx_ssrc,
            &mut media_started,
        ).await;

        if !rtc.is_alive() {
            debug!(participant = %pid, "rtc no longer alive");
            break;
        }

        let wait = timeout
            .map(|t| t.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(5))
            .min(Duration::from_millis(5));

        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                if let Ok((n, source)) = result {
                    if let Ok(receive) = str0m::net::Receive::new(
                        Protocol::Udp, source, local_addr, &buf[..n],
                    ) {
                        let _ = rtc.handle_input(Input::Receive(Instant::now(), receive));
                    }
                }
            }

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(PeerCmd::IceCandidate(c)) => {
                        if let Ok(cand) = Candidate::from_sdp_string(&c) {
                            rtc.add_remote_candidate(cand);
                        }
                    }
                    Some(PeerCmd::Media(m)) => {
                        write_forwarded_rtp(&mut rtc, &m, audio_mid, video_mid, audio_tx_ssrc, video_tx_ssrc);
                    }
                    None => break,
                }
            }

            _ = tokio::time::sleep(wait) => {
                let _ = rtc.handle_input(Input::Timeout(Instant::now()));
            }
        }
    }

    peers.lock().await.remove(&pid);
    info!(participant = %pid, "media task ended");
}

async fn drain_outputs(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    pid: &ParticipantId,
    peers: &Peers,
    audio_mid: &mut Option<Mid>,
    video_mid: &mut Option<Mid>,
    audio_tx_ssrc: &mut Option<Ssrc>,
    video_tx_ssrc: &mut Option<Ssrc>,
    media_started: &mut bool,
) -> Option<Instant> {
    loop {
        match rtc.poll_output() {
            Ok(Output::Transmit(t)) => {
                let _ = socket.try_send_to(&t.contents, t.destination);
            }
            Ok(Output::Event(event)) => {
                debug!(participant = %pid, event = ?std::mem::discriminant(&event), "str0m event");
                match event {
                    Event::Connected => {
                        info!(participant = %pid, "WebRTC CONNECTED");
                    }
                    Event::MediaAdded(m) => {
                        match m.kind {
                            MediaKind::Audio => *audio_mid = Some(m.mid),
                            MediaKind::Video => *video_mid = Some(m.mid),
                        }
                        info!(participant = %pid, mid = ?m.mid, kind = ?m.kind, "media added");

                        // Declare outgoing TX streams for forwarding.
                        if !*media_started && audio_mid.is_some() && video_mid.is_some() {
                            *media_started = true;
                            let a_ssrc = Ssrc::new();
                            let v_ssrc = Ssrc::new();
                            {
                                let mut api = rtc.direct_api();
                                if let Some(mid) = *audio_mid {
                                    api.declare_stream_tx(a_ssrc, None, mid, None);
                                }
                                if let Some(mid) = *video_mid {
                                    api.declare_stream_tx(v_ssrc, None, mid, None);
                                }
                            }
                            *audio_tx_ssrc = Some(a_ssrc);
                            *video_tx_ssrc = Some(v_ssrc);
                            info!(participant = %pid, ?a_ssrc, ?v_ssrc, "declared TX streams");
                        }
                    }
                    Event::RtpPacket(pkt) => {
                        info!(participant = %pid, pt = *pkt.header.payload_type, seq = ?pkt.seq_no, "GOT RTP PACKET!");
                        // Heuristic: audio PT is typically 109 (Opus), video starts at 120.
                        let is_audio_pt = *pkt.header.payload_type < 112;

                        let forward = ForwardedMedia {
                            pt: *pkt.header.payload_type,
                            seq_no: (*pkt.seq_no).into(),
                            time: pkt.header.timestamp,
                            marker: pkt.header.marker,
                            payload: pkt.payload,
                            is_audio: is_audio_pt,
                        };

                        let p = peers.lock().await;
                        for (id, peer) in p.iter() {
                            if id != pid {
                                let _ = peer.cmd_tx.send(PeerCmd::Media(ForwardedMedia {
                                    pt: forward.pt,
                                    seq_no: forward.seq_no,
                                    time: forward.time,
                                    marker: forward.marker,
                                    payload: forward.payload.clone(),
                                    is_audio: forward.is_audio,
                                }));
                            }
                        }
                    }
                    Event::IceConnectionStateChange(state) => {
                        debug!(participant = %pid, ?state, "ICE state");
                    }
                    _ => {}
                }
            }
            Ok(Output::Timeout(t)) => return Some(t),
            Err(e) => {
                debug!("poll_output error: {e}");
                return None;
            }
        }
    }
}

/// Writes a forwarded RTP packet into the target Rtc using direct_api.
fn write_forwarded_rtp(
    rtc: &mut Rtc,
    media: &ForwardedMedia,
    _audio_mid: Option<Mid>,
    _video_mid: Option<Mid>,
    audio_tx_ssrc: Option<Ssrc>,
    video_tx_ssrc: Option<Ssrc>,
) {
    let ssrc = if media.is_audio { audio_tx_ssrc } else { video_tx_ssrc };
    let Some(ssrc) = ssrc else { return };

    let mut api = rtc.direct_api();
    let Some(stream_tx) = api.stream_tx(&ssrc) else { return };

    let pt = media.pt.into();
    let seq_no = media.seq_no.into();

    if let Err(e) = stream_tx.write_rtp(
        pt,
        seq_no,
        media.time,
        Instant::now(),
        media.marker,
        ExtensionValues::default(),
        !media.is_audio, // nackable for video only
        media.payload.clone(),
    ) {
        debug!("write_rtp error: {e}");
    }
}

async fn local_ip() -> std::net::IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("bind temp socket");
    let _ = socket.connect("8.8.8.8:80").await;
    socket
        .local_addr()
        .map(|a| a.ip())
        .unwrap_or_else(|_| std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
}
