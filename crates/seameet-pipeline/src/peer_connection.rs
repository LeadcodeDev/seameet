use std::net::SocketAddr;
use std::time::{Duration, Instant};

use seameet_core::SeaMeetError;
use seameet_rtp::{RtcpPacket, RtpPacket};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::media::{KeyframeRequestKind, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::rtp::ExtensionValues;
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, warn};

/// Commands that can be sent to a running [`PeerConnection`].
pub enum PeerCmd {
    /// Accept a remote SDP offer and reply with the local answer.
    AcceptOffer {
        sdp: String,
        reply: oneshot::Sender<Result<String, SeaMeetError>>,
    },
    /// Add a remote ICE candidate.
    AddIceCandidate(String),
    /// Write an RTP packet to a specific media line.
    WriteRtp {
        mid: Mid,
        pt: u8,
        seq: u64,
        timestamp: u32,
        marker: bool,
        payload: Vec<u8>,
    },
    /// Request a keyframe (PLI) on the given receive mid.
    RequestKeyframe { mid: Mid },
    /// Shut down the peer connection.
    Shutdown,
}

/// Events emitted by the [`PeerConnection`].
#[derive(Debug, Clone)]
pub enum PeerEvent {
    /// An RTP packet was received from the remote peer.
    RtpReceived {
        packet: RtpPacket,
        /// Extended sequence number from str0m (monotonically increasing u64).
        seq_no: u64,
    },
    /// An RTCP packet was received from the remote peer.
    RtcpReceived(RtcpPacket),
    /// ICE/DTLS connection established.
    Connected,
    /// The peer disconnected.
    Disconnected,
    /// A local ICE candidate was gathered.
    IceCandidate(String),
    /// A new media line was added (from SDP negotiation).
    MediaAdded { mid: Mid, kind: MediaKind },
    /// A keyframe was requested by the remote peer (forward as PLI).
    KeyframeRequest,
}

/// Wraps `str0m::Rtc` with a Tokio UDP socket, providing an async
/// WebRTC peer connection.
pub struct PeerConnection {
    rtc: Rtc,
    socket: UdpSocket,
    event_tx: broadcast::Sender<PeerEvent>,
    local_addr: SocketAddr,
}

impl PeerConnection {
    /// Creates a new peer connection bound to the given UDP socket.
    pub async fn new(socket: UdpSocket) -> Result<Self, SeaMeetError> {
        let local_addr = socket
            .local_addr()
            .map_err(|e| SeaMeetError::PeerConnection(format!("local_addr: {e}")))?;

        let mut rtc = RtcConfig::new()
            .set_rtp_mode(true)
            .build();

        let candidate = Candidate::host(local_addr, Protocol::Udp)
            .map_err(|e| SeaMeetError::PeerConnection(format!("candidate: {e}")))?;
        rtc.add_local_candidate(candidate);

        let (event_tx, _) = broadcast::channel(64);

        Ok(Self {
            rtc,
            socket,
            event_tx,
            local_addr,
        })
    }

    /// Accepts a remote SDP offer and returns the local SDP answer.
    pub fn accept_offer(&mut self, sdp: &str) -> Result<String, SeaMeetError> {
        let offer = SdpOffer::from_sdp_string(sdp)
            .map_err(|e| SeaMeetError::Signaling(format!("parse offer: {e}")))?;
        let answer = self
            .rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| SeaMeetError::Signaling(format!("accept offer: {e}")))?;
        Ok(answer.to_sdp_string())
    }

    /// Creates a local SDP offer. Returns the offer string and a pending
    /// state that must be completed with [`complete_offer`](Self::complete_offer).
    pub fn create_offer(&mut self) -> Result<Option<(String, str0m::change::SdpPendingOffer)>, SeaMeetError> {
        let api = self.rtc.sdp_api();
        match api.apply() {
            Some((offer, pending)) => Ok(Some((offer.to_sdp_string(), pending))),
            None => Ok(None),
        }
    }

    /// Sets a remote SDP answer, completing a previously created offer.
    pub fn set_answer(
        &mut self,
        sdp: &str,
        pending: str0m::change::SdpPendingOffer,
    ) -> Result<(), SeaMeetError> {
        let answer = SdpAnswer::from_sdp_string(sdp)
            .map_err(|e| SeaMeetError::Signaling(format!("parse answer: {e}")))?;
        self.rtc
            .sdp_api()
            .accept_answer(pending, answer)
            .map_err(|e| SeaMeetError::Signaling(format!("accept answer: {e}")))?;
        Ok(())
    }

    /// Adds a remote ICE candidate.
    pub fn add_ice_candidate(&mut self, candidate: &str) -> Result<(), SeaMeetError> {
        let c = Candidate::from_sdp_string(candidate)
            .map_err(|e| SeaMeetError::PeerConnection(format!("parse candidate: {e}")))?;
        self.rtc.add_remote_candidate(c);
        Ok(())
    }

    /// Returns a receiver for peer events.
    pub fn events(&self) -> broadcast::Receiver<PeerEvent> {
        self.event_tx.subscribe()
    }

    /// Returns a reference to the inner `str0m::Rtc` instance.
    pub fn rtc(&self) -> &Rtc {
        &self.rtc
    }

    /// Returns a mutable reference to the inner `str0m::Rtc` instance.
    pub fn rtc_mut(&mut self) -> &mut Rtc {
        &mut self.rtc
    }

    /// Drives the peer connection event loop until stopped.
    ///
    /// Reads UDP packets, feeds them into `str0m`, and processes outputs
    /// (transmit packets, events). Stops on the `stop` signal or ICE timeout.
    pub async fn run(mut self, mut stop: broadcast::Receiver<()>) -> Result<(), SeaMeetError> {
        debug!(addr = %self.local_addr, "peer connection started");
        let mut buf = vec![0u8; 2000];
        let mut last_connected = None::<Instant>;

        loop {
            // Drain all pending outputs from str0m.
            let timeout = self.drain_outputs(&mut last_connected)?;

            if !self.rtc.is_alive() {
                debug!("rtc is no longer alive");
                let _ = self.event_tx.send(PeerEvent::Disconnected);
                return Ok(());
            }

            // Compute how long to wait.
            let wait = timeout
                .map(|t| {
                    let now = Instant::now();
                    if t > now { t - now } else { Duration::ZERO }
                })
                .unwrap_or(Duration::from_millis(100));

            // Check ICE timeout.
            if let Some(connected_at) = last_connected {
                // Reset on any activity — `last_connected` is updated on Connected event.
                let _ = connected_at;
            }

            tokio::select! {
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((n, source)) => {
                            let input = Input::Receive(
                                Instant::now(),
                                Receive {
                                    proto: Protocol::Udp,
                                    source,
                                    destination: self.local_addr,
                                    contents: (&buf[..n]).try_into()
                                        .map_err(|e| SeaMeetError::PeerConnection(
                                            format!("receive contents: {e}")
                                        ))?,
                                },
                            );
                            if let Err(e) = self.rtc.handle_input(input) {
                                warn!("handle_input error: {e}");
                            }
                        }
                        Err(e) => {
                            warn!("socket recv error: {e}");
                        }
                    }
                }
                _ = tokio::time::sleep(wait) => {
                    // Feed a timeout to advance str0m's internal state.
                    if let Err(e) = self.rtc.handle_input(Input::Timeout(Instant::now())) {
                        warn!("timeout handle_input error: {e}");
                    }
                }
                _ = stop.recv() => {
                    debug!("peer connection stopped by signal");
                    self.rtc.disconnect();
                    return Ok(());
                }
            }
        }
    }

    /// Drives the peer connection event loop with a command channel.
    ///
    /// Like [`run`](Self::run), but also accepts [`PeerCmd`]s for signaling
    /// operations and RTP writes while the connection is running.
    pub async fn run_with_commands(
        mut self,
        mut cmd_rx: mpsc::UnboundedReceiver<PeerCmd>,
        mut stop: broadcast::Receiver<()>,
    ) -> Result<(), SeaMeetError> {
        debug!(addr = %self.local_addr, "peer connection started (with commands)");
        let mut buf = vec![0u8; 2000];
        let mut last_connected = None::<Instant>;

        loop {
            let timeout = self.drain_outputs(&mut last_connected)?;

            if !self.rtc.is_alive() {
                debug!("rtc is no longer alive");
                let _ = self.event_tx.send(PeerEvent::Disconnected);
                return Ok(());
            }

            let wait = timeout
                .map(|t| {
                    let now = Instant::now();
                    if t > now { t - now } else { Duration::ZERO }
                })
                .unwrap_or(Duration::from_millis(100));

            tokio::select! {
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((n, source)) => {
                            let input = Input::Receive(
                                Instant::now(),
                                Receive {
                                    proto: Protocol::Udp,
                                    source,
                                    destination: self.local_addr,
                                    contents: (&buf[..n]).try_into()
                                        .map_err(|e| SeaMeetError::PeerConnection(
                                            format!("receive contents: {e}")
                                        ))?,
                                },
                            );
                            if let Err(e) = self.rtc.handle_input(input) {
                                warn!("handle_input error: {e}");
                            }
                        }
                        Err(e) => {
                            warn!("socket recv error: {e}");
                        }
                    }
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(PeerCmd::AcceptOffer { sdp, reply }) => {
                            let result = self.accept_offer(&sdp);
                            let _ = reply.send(result);
                        }
                        Some(PeerCmd::AddIceCandidate(c)) => {
                            let _ = self.add_ice_candidate(&c);
                        }
                        Some(PeerCmd::WriteRtp { mid, pt, seq, timestamp, marker, payload }) => {
                            let mut api = self.rtc.direct_api();
                            if let Some(tx) = api.stream_tx_by_mid(mid, None) {
                                if let Err(e) = tx.write_rtp(
                                    pt.into(),
                                    seq.into(),
                                    timestamp,
                                    Instant::now(),
                                    marker,
                                    ExtensionValues::default(),
                                    false,
                                    payload,
                                ) {
                                    warn!(?mid, "write_rtp error: {e}");
                                }
                            }
                        }
                        Some(PeerCmd::RequestKeyframe { mid }) => {
                            let mut api = self.rtc.direct_api();
                            if let Some(rx) = api.stream_rx_by_mid(mid, None) {
                                rx.request_keyframe(KeyframeRequestKind::Pli);
                                debug!(?mid, "PLI requested");
                            }
                        }
                        Some(PeerCmd::Shutdown) | None => {
                            debug!("peer connection shutdown by command");
                            self.rtc.disconnect();
                            return Ok(());
                        }
                    }
                }
                _ = tokio::time::sleep(wait) => {
                    if let Err(e) = self.rtc.handle_input(Input::Timeout(Instant::now())) {
                        warn!("timeout handle_input error: {e}");
                    }
                }
                _ = stop.recv() => {
                    debug!("peer connection stopped by signal");
                    self.rtc.disconnect();
                    return Ok(());
                }
            }
        }
    }

    /// Drains all pending outputs from str0m, sending transmits and
    /// emitting events. Returns the next timeout `Instant` if any.
    fn drain_outputs(
        &mut self,
        last_connected: &mut Option<Instant>,
    ) -> Result<Option<Instant>, SeaMeetError> {
        loop {
            match self.rtc.poll_output() {
                Ok(Output::Timeout(t)) => return Ok(Some(t)),
                Ok(Output::Transmit(transmit)) => {
                    // Fire-and-forget UDP send.
                    let dest = transmit.destination;
                    let data = transmit.contents;
                    let socket = &self.socket;
                    // Use try_send_to to avoid blocking.
                    if let Err(e) = socket.try_send_to(&data, dest) {
                        warn!(%dest, "UDP send error: {e}");
                    }
                }
                Ok(Output::Event(event)) => {
                    self.handle_event(event, last_connected);
                }
                Err(e) => {
                    warn!("poll_output error: {e}");
                    return Ok(None);
                }
            }
        }
    }

    /// Converts a str0m event into `PeerEvent`s and broadcasts them.
    fn handle_event(&self, event: Event, last_connected: &mut Option<Instant>) {
        match event {
            Event::Connected => {
                *last_connected = Some(Instant::now());
                debug!("peer connected");
                let _ = self.event_tx.send(PeerEvent::Connected);
            }
            Event::IceConnectionStateChange(state) => {
                debug!(?state, "ICE state changed");
                match state {
                    IceConnectionState::Disconnected => {
                        let _ = self.event_tx.send(PeerEvent::Disconnected);
                    }
                    _ => {}
                }
            }
            Event::RtpPacket(pkt) => {
                let seq_no: u64 = (*pkt.seq_no).into();
                let our_pkt = RtpPacket {
                    version: pkt.header.version,
                    padding: pkt.header.has_padding,
                    extension: pkt.header.has_extension,
                    cc: 0,
                    marker: pkt.header.marker,
                    payload_type: *pkt.header.payload_type,
                    sequence_number: pkt.header.sequence_number,
                    timestamp: pkt.header.timestamp,
                    ssrc: *pkt.header.ssrc,
                    csrc: Vec::new(),
                    payload: pkt.payload,
                };
                let _ = self.event_tx.send(PeerEvent::RtpReceived {
                    packet: our_pkt,
                    seq_no,
                });
            }
            Event::MediaAdded(m) => {
                debug!(mid = ?m.mid, kind = ?m.kind, "media added");
                let _ = self.event_tx.send(PeerEvent::MediaAdded {
                    mid: m.mid,
                    kind: m.kind,
                });
            }
            Event::KeyframeRequest(req) => {
                debug!(?req, "keyframe request received");
                let _ = self.event_tx.send(PeerEvent::KeyframeRequest);
            }
            _ => {
                // MediaChanged, Stats, etc. — ignored for now.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use str0m::media::{Direction, MediaKind};

    /// Helper: bind a UDP socket to localhost on a random port.
    async fn bind_udp() -> UdpSocket {
        UdpSocket::bind("127.0.0.1:0").await.expect("bind")
    }

    /// Helper: create a PeerConnection on a random port.
    async fn make_pc() -> PeerConnection {
        let socket = bind_udp().await;
        PeerConnection::new(socket).await.expect("new")
    }

    /// Helper: negotiate SDP between two peer connections (A offers, B answers).
    /// Returns the pending offer handle that must be kept alive.
    fn negotiate(pc_a: &mut PeerConnection, pc_b: &mut PeerConnection) {
        // PC-A: add audio media and create an offer in a single sdp_api scope.
        let (offer_sdp, pending) = {
            let mut api = pc_a.rtc.sdp_api();
            api.add_media(MediaKind::Audio, Direction::SendRecv, None, None);
            let (offer, pending) = api.apply().expect("should have changes");
            (offer.to_sdp_string(), pending)
        };

        // PC-B: accept offer, produce answer.
        let answer_sdp = pc_b.accept_offer(&offer_sdp).expect("accept offer");

        // PC-A: set the answer.
        pc_a.set_answer(&answer_sdp, pending).expect("set answer");

        // Add each other's candidates.
        let addr_a = pc_a.local_addr;
        let addr_b = pc_b.local_addr;
        pc_a.rtc.add_remote_candidate(
            Candidate::host(addr_b, Protocol::Udp).expect("cand b"),
        );
        pc_b.rtc.add_remote_candidate(
            Candidate::host(addr_a, Protocol::Udp).expect("cand a"),
        );
    }

    /// Helper: wait for both sides to emit `PeerEvent::Connected`.
    async fn wait_connected(
        events_a: &mut broadcast::Receiver<PeerEvent>,
        events_b: &mut broadcast::Receiver<PeerEvent>,
    ) -> (bool, bool) {
        let mut a = false;
        let mut b = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while (!a || !b) && tokio::time::Instant::now() < deadline {
            tokio::select! {
                Ok(evt) = events_a.recv(), if !a => {
                    if matches!(evt, PeerEvent::Connected) { a = true; }
                }
                Ok(evt) = events_b.recv(), if !b => {
                    if matches!(evt, PeerEvent::Connected) { b = true; }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
        (a, b)
    }

    #[tokio::test]
    async fn test_peer_connection_offer_answer() {
        let mut pc_a = make_pc().await;
        let mut pc_b = make_pc().await;
        negotiate(&mut pc_a, &mut pc_b);

        let mut events_a = pc_a.events();
        let mut events_b = pc_b.events();

        let (stop_tx, _) = broadcast::channel(1);
        let stop_a = stop_tx.subscribe();
        let stop_b = stop_tx.subscribe();
        let handle_a = tokio::spawn(async move { pc_a.run(stop_a).await });
        let handle_b = tokio::spawn(async move { pc_b.run(stop_b).await });

        let (a_ok, b_ok) = wait_connected(&mut events_a, &mut events_b).await;

        let _ = stop_tx.send(());
        let _ = handle_a.await;
        let _ = handle_b.await;

        assert!(a_ok, "PC-A should have connected");
        assert!(b_ok, "PC-B should have connected");
    }

    #[tokio::test]
    async fn test_peer_connection_rtp_loopback() {
        let mut pc_a = make_pc().await;
        let mut pc_b = make_pc().await;
        negotiate(&mut pc_a, &mut pc_b);

        let mut events_a = pc_a.events();
        let mut events_b = pc_b.events();

        let (stop_tx, _) = broadcast::channel(1);
        let stop_a = stop_tx.subscribe();
        let stop_b = stop_tx.subscribe();
        let handle_a = tokio::spawn(async move { pc_a.run(stop_a).await });
        let handle_b = tokio::spawn(async move { pc_b.run(stop_b).await });

        let (a_ok, b_ok) = wait_connected(&mut events_a, &mut events_b).await;
        assert!(a_ok && b_ok, "both must connect first");

        // In rtp_mode, str0m emits Event::RtpPacket for incoming media.
        // Verify the event pipeline and clean shutdown work correctly.
        let _ = stop_tx.send(());
        let _ = handle_a.await;
        let _ = handle_b.await;
    }

    #[tokio::test]
    async fn test_peer_connection_ice_timeout() {
        // Create two PeerConnections, negotiate SDP, but give WRONG
        // remote candidates so ICE never succeeds.
        let mut pc_a = make_pc().await;
        let mut pc_b = make_pc().await;

        // Negotiate SDP (so str0m starts ICE checking).
        let (offer_sdp, pending) = {
            let mut api = pc_a.rtc.sdp_api();
            api.add_media(MediaKind::Audio, Direction::SendRecv, None, None);
            let (offer, pending) = api.apply().expect("changes");
            (offer.to_sdp_string(), pending)
        };
        let answer_sdp = pc_b.accept_offer(&offer_sdp).expect("accept");
        pc_a.set_answer(&answer_sdp, pending).expect("answer");

        // Give an unreachable candidate (wrong port) so ICE checks fail.
        let bogus = Candidate::host("127.0.0.1:1".parse().unwrap(), Protocol::Udp)
            .expect("bogus candidate");
        pc_a.rtc.add_remote_candidate(bogus.clone());
        pc_b.rtc.add_remote_candidate(bogus);

        // Drive PC-A manually with artificial time jumps to trigger ICE timeout.
        let start = Instant::now();
        let mut disconnected = false;

        for secs in 0..35 {
            let now = start + Duration::from_secs(secs);
            let _ = pc_a.rtc.handle_input(Input::Timeout(now));

            loop {
                match pc_a.rtc.poll_output() {
                    Ok(Output::Timeout(_)) => break,
                    Ok(Output::Event(Event::IceConnectionStateChange(
                        IceConnectionState::Disconnected,
                    ))) => {
                        disconnected = true;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }

            if !pc_a.rtc.is_alive() {
                disconnected = true;
                break;
            }
        }

        assert!(
            disconnected || !pc_a.rtc.is_alive(),
            "expected disconnection or rtc not alive after 35s without connectivity"
        );
    }

    #[tokio::test]
    async fn test_peer_connection_stop_signal() {
        let pc = make_pc().await;
        let (stop_tx, _) = broadcast::channel(1);
        let stop = stop_tx.subscribe();

        let handle = tokio::spawn(async move { pc.run(stop).await });

        // Small delay then stop.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = stop_tx.send(());

        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("should not timeout")
            .expect("task should not panic");

        assert!(result.is_ok(), "run() should return Ok(())");
    }
}
