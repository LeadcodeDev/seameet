use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;

use seameet_core::{ParticipantId, SeaMeetError};
use seameet_signaling::engine::{run_connection, SignalingHooks, SignalingState};
use seameet_signaling::transport::TransportListener;
use seameet_signaling::ws_listener::WsListener;
use seameet_signaling::SdpMessage;
use seameet_sfu::{SfuConfig, SfuServer};
use tokio::sync::{broadcast, RwLock};

use crate::server_event::ServerEvent;

// ── Closure type aliases ─────────────────────────────────────────────

type AuthFn = Box<
    dyn Fn(ParticipantId, String, Option<String>) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

type RateCheckFn = Box<
    dyn Fn(ParticipantId) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync,
>;

// ── ClosureHooks<H> ─────────────────────────────────────────────────

pub(crate) struct ClosureHooks<H: SignalingHooks> {
    inner: Arc<H>,
    auth_fn: Option<AuthFn>,
    rate_fn: Option<RateCheckFn>,
    event_tx: broadcast::Sender<ServerEvent>,
    max_room_members: Option<usize>,
    max_chat_history: Option<usize>,
    max_room_id_len: Option<usize>,
    max_display_name_len: Option<usize>,
}

impl<H: SignalingHooks> SignalingHooks for ClosureHooks<H> {
    async fn on_authenticate(
        &self,
        participant: ParticipantId,
        room_id: &str,
        token: Option<&str>,
    ) -> Result<(), String> {
        let result = if let Some(f) = &self.auth_fn {
            let room_owned = room_id.to_owned();
            let token_owned = token.map(|t| t.to_owned());
            f(participant, room_owned, token_owned).await
        } else {
            self.inner.on_authenticate(participant, room_id, token).await
        };

        if let Err(ref reason) = result {
            let _ = self.event_tx.send(ServerEvent::AuthRejected {
                participant,
                room_id: room_id.to_owned(),
                reason: reason.clone(),
            });
        }

        result
    }

    async fn on_rate_check(&self, pid: ParticipantId) -> bool {
        let allowed = if let Some(f) = &self.rate_fn {
            f(pid).await
        } else {
            self.inner.on_rate_check(pid).await
        };

        if !allowed {
            let _ = self.event_tx.send(ServerEvent::RateLimited { participant: pid });
        }

        allowed
    }

    async fn on_message(
        &self,
        sdp: &SdpMessage,
        raw: &str,
        pid: ParticipantId,
        self_tx: &tokio::sync::mpsc::UnboundedSender<String>,
        state: &Arc<RwLock<SignalingState>>,
    ) -> bool {
        // Snapshot room existence before dispatch for Join messages.
        let pre_room_exists = if let SdpMessage::Join { room_id, .. } = sdp {
            let st = state.read().await;
            st.room(room_id).is_some()
        } else {
            false
        };

        let suppressed = self.inner.on_message(sdp, raw, pid, self_tx, state).await;

        // Emit events after inner processing.
        match sdp {
            SdpMessage::Join { room_id, .. } => {
                if !pre_room_exists {
                    let _ = self.event_tx.send(ServerEvent::RoomCreated {
                        room_id: room_id.clone(),
                    });
                }
                let _ = self.event_tx.send(ServerEvent::PeerConnected {
                    participant: pid,
                    room_id: room_id.clone(),
                });
            }
            _ => {}
        }

        let message_type = message_type_name(sdp);
        let _ = self.event_tx.send(ServerEvent::MessageReceived {
            participant: pid,
            message_type: message_type.to_owned(),
        });

        suppressed
    }

    async fn on_disconnect(
        &self,
        pid: ParticipantId,
        affected_rooms: &[(String, bool)],
        state: &Arc<RwLock<SignalingState>>,
    ) {
        self.inner.on_disconnect(pid, affected_rooms, state).await;

        let rooms: Vec<String> = affected_rooms.iter().map(|(r, _)| r.clone()).collect();
        let _ = self.event_tx.send(ServerEvent::PeerDisconnected {
            participant: pid,
            rooms,
        });

        for (room_id, emptied) in affected_rooms {
            if *emptied {
                let _ = self.event_tx.send(ServerEvent::RoomDestroyed {
                    room_id: room_id.clone(),
                });
            }
        }
    }

    fn max_room_members(&self) -> usize {
        self.max_room_members
            .unwrap_or_else(|| self.inner.max_room_members())
    }

    fn max_chat_history(&self) -> usize {
        self.max_chat_history
            .unwrap_or_else(|| self.inner.max_chat_history())
    }

    fn max_room_id_len(&self) -> usize {
        self.max_room_id_len
            .unwrap_or_else(|| self.inner.max_room_id_len())
    }

    fn max_display_name_len(&self) -> usize {
        self.max_display_name_len
            .unwrap_or_else(|| self.inner.max_display_name_len())
    }
}

fn message_type_name(sdp: &SdpMessage) -> &'static str {
    match sdp {
        SdpMessage::Join { .. } => "join",
        SdpMessage::Leave { .. } => "leave",
        SdpMessage::Offer { .. } => "offer",
        SdpMessage::Answer { .. } => "answer",
        SdpMessage::IceCandidate { .. } => "ice_candidate",
        SdpMessage::Ready { .. } => "ready",
        SdpMessage::PeerJoined { .. } => "peer_joined",
        SdpMessage::PeerLeft { .. } => "peer_left",
        SdpMessage::ScreenShareStarted { .. } => "screen_share_started",
        SdpMessage::ScreenShareStopped { .. } => "screen_share_stopped",
        SdpMessage::MuteAudio { .. } => "mute_audio",
        SdpMessage::UnmuteAudio { .. } => "unmute_audio",
        SdpMessage::MuteVideo { .. } => "mute_video",
        SdpMessage::UnmuteVideo { .. } => "unmute_video",
        SdpMessage::VideoConfigChanged { .. } => "video_config_changed",
        SdpMessage::RequestRenegotiation { .. } => "request_renegotiation",
        SdpMessage::RoomStatus { .. } => "room_status",
        SdpMessage::E2eePublicKey { .. } => "e2ee_public_key",
        SdpMessage::E2eeSenderKey { .. } => "e2ee_sender_key",
        SdpMessage::E2eeKeyRotation { .. } => "e2ee_key_rotation",
        SdpMessage::ChatMessage { .. } => "chat_message",
        SdpMessage::ActiveSpeaker { .. } => "active_speaker",
        SdpMessage::Error { .. } => "error",
    }
}

// ── SeaMeetServerBuilder ─────────────────────────────────────────────

pub struct SeaMeetServerBuilder {
    ws_addr: String,
    udp_port: u16,
    public_ip: Option<IpAddr>,
    bwe_kbps: Option<u32>,
    auth_fn: Option<AuthFn>,
    rate_fn: Option<RateCheckFn>,
    max_room_members: Option<usize>,
    max_chat_history: Option<usize>,
    max_room_id_len: Option<usize>,
    max_display_name_len: Option<usize>,
    event_capacity: usize,
}

impl SeaMeetServerBuilder {
    fn new() -> Self {
        Self {
            ws_addr: "0.0.0.0:3001".to_owned(),
            udp_port: 10000,
            public_ip: None,
            bwe_kbps: None,
            auth_fn: None,
            rate_fn: None,
            max_room_members: None,
            max_chat_history: None,
            max_room_id_len: None,
            max_display_name_len: None,
            event_capacity: 256,
        }
    }

    // ── Transport ────────────────────────────────────────────────────

    pub fn ws_addr(mut self, addr: impl Into<String>) -> Self {
        self.ws_addr = addr.into();
        self
    }

    pub fn udp_port(mut self, port: u16) -> Self {
        self.udp_port = port;
        self
    }

    pub fn public_ip(mut self, ip: IpAddr) -> Self {
        self.public_ip = Some(ip);
        self
    }

    pub fn bwe_kbps(mut self, kbps: u32) -> Self {
        self.bwe_kbps = Some(kbps);
        self
    }

    // ── Limits ───────────────────────────────────────────────────────

    pub fn max_room_members(mut self, n: usize) -> Self {
        self.max_room_members = Some(n);
        self
    }

    pub fn max_chat_history(mut self, n: usize) -> Self {
        self.max_chat_history = Some(n);
        self
    }

    pub fn max_room_id_len(mut self, n: usize) -> Self {
        self.max_room_id_len = Some(n);
        self
    }

    pub fn max_display_name_len(mut self, n: usize) -> Self {
        self.max_display_name_len = Some(n);
        self
    }

    // ── Events ───────────────────────────────────────────────────────

    pub fn event_capacity(mut self, cap: usize) -> Self {
        self.event_capacity = cap;
        self
    }

    // ── Security (closures) ──────────────────────────────────────────

    pub fn on_authenticate<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(ParticipantId, String, Option<String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.auth_fn = Some(Box::new(move |pid, room, token| Box::pin(f(pid, room, token))));
        self
    }

    pub fn on_rate_check<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(ParticipantId) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = bool> + Send + 'static,
    {
        self.rate_fn = Some(Box::new(move |pid| Box::pin(f(pid))));
        self
    }

    // ── Build ────────────────────────────────────────────────────────

    pub async fn build(self) -> Result<SeaMeetServer, SeaMeetError> {
        let sfu_config = SfuConfig {
            udp_port: self.udp_port,
            public_ip: self.public_ip,
            bwe_kbps: self.bwe_kbps,
            ..Default::default()
        };

        let sfu = SfuServer::new(sfu_config).await?;
        let state = sfu.signaling_state();
        let udp_port = sfu.udp_local_addr().port();

        let (event_tx, _) = broadcast::channel::<ServerEvent>(self.event_capacity);

        let hooks = Arc::new(ClosureHooks {
            inner: sfu,
            auth_fn: self.auth_fn,
            rate_fn: self.rate_fn,
            event_tx: event_tx.clone(),
            max_room_members: self.max_room_members,
            max_chat_history: self.max_chat_history,
            max_room_id_len: self.max_room_id_len,
            max_display_name_len: self.max_display_name_len,
        });

        let listener = WsListener::bind(&self.ws_addr).await?;

        Ok(SeaMeetServer {
            listener,
            state,
            hooks,
            event_tx,
            udp_port,
        })
    }
}

// ── SeaMeetServer ────────────────────────────────────────────────────

pub struct SeaMeetServer {
    listener: WsListener,
    state: Arc<RwLock<SignalingState>>,
    hooks: Arc<ClosureHooks<SfuServer>>,
    event_tx: broadcast::Sender<ServerEvent>,
    udp_port: u16,
}

impl SeaMeetServer {
    pub fn builder() -> SeaMeetServerBuilder {
        SeaMeetServerBuilder::new()
    }

    /// Subscribe to server events. Multiple receivers are supported.
    pub fn events(&self) -> broadcast::Receiver<ServerEvent> {
        self.event_tx.subscribe()
    }

    /// The actual UDP port the SFU is listening on.
    pub fn udp_port(&self) -> u16 {
        self.udp_port
    }

    /// Run the accept loop. This consumes the server and runs until the
    /// listener is closed.
    pub async fn run(mut self) {
        while let Some(conn) = self.listener.accept().await {
            tokio::spawn(run_connection(
                conn,
                Arc::clone(&self.state),
                Arc::clone(&self.hooks),
            ));
        }
    }
}
