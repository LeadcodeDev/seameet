mod http;
mod session;

use std::collections::HashMap;
use std::env::{var, VarError};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use seameet::{ParticipantId, SeaMeetServer};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::http::{router, AppState, AuthOutcome, AuthProvider};
use crate::session::SessionState;

const RATE_LIMIT_MSGS: u32 = 200;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(1);
const PURGE_INTERVAL: Duration = Duration::from_secs(5);

/// Permissive provider for development: every request is accepted as
/// anonymous, the server mints a random ParticipantId.
struct OpenAuth;

impl AuthProvider for OpenAuth {
    fn authorize(
        &self,
        _bearer: Option<&str>,
        _room_id: &str,
        _display_name: Option<&str>,
    ) -> AuthOutcome {
        AuthOutcome::Anonymous
    }
}

/// Shared-secret auth: the bearer token must match `SEAMEET_AUTH_TOKEN`.
/// Useful for closed deployments and local testing without a real IdP.
struct SharedSecretAuth {
    expected: String,
}

impl AuthProvider for SharedSecretAuth {
    fn authorize(
        &self,
        bearer: Option<&str>,
        _room_id: &str,
        _display_name: Option<&str>,
    ) -> AuthOutcome {
        match bearer {
            Some(token) if token == self.expected => AuthOutcome::Anonymous,
            Some(_) => AuthOutcome::Rejected("invalid token".to_owned()),
            None => AuthOutcome::Rejected("missing token".to_owned()),
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "meet=info,tower_http=info,str0m=warn,str0m::rtp_=error"
                    .parse()
                    .expect("filter")
            }),
        )
        .init();

    let udp_port = var("UDP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10000);

    let http_addr: SocketAddr = var("HTTP_ADDR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "0.0.0.0:3002".parse().unwrap());

    // ── Session state (shared between HTTP and WS auth) ────────────────
    let session_secret = match var("SEAMEET_SESSION_SECRET") {
        Ok(s) if s.len() >= 32 => s.into_bytes(),
        Ok(_) => {
            panic!("SEAMEET_SESSION_SECRET must be at least 32 bytes");
        }
        Err(_) => {
            warn!(
                "SEAMEET_SESSION_SECRET not set; generating a random secret. \
                 Tokens will be invalidated on every restart."
            );
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let mut secret = vec![0u8; 32];
            for (i, b) in secret.iter_mut().enumerate() {
                *b = ((nanos.wrapping_mul((i as u32) + 1)) ^ 0xA5) as u8;
            }
            secret
        }
    };
    let sessions = SessionState::new(&session_secret);

    // ── Auth provider selection ────────────────────────────────────────
    let auth: Arc<dyn AuthProvider> = match var("SEAMEET_AUTH_TOKEN") {
        Ok(expected) if !expected.is_empty() => {
            info!("auth: shared-secret enabled via SEAMEET_AUTH_TOKEN");
            Arc::new(SharedSecretAuth { expected })
        }
        _ => {
            warn!(
                "SEAMEET_AUTH_TOKEN not set; running in OPEN mode. \
                 Do not expose this on a public network."
            );
            Arc::new(OpenAuth)
        }
    };

    // ── SFU builder ────────────────────────────────────────────────────
    let mut builder = SeaMeetServer::builder()
        .ws_addr("0.0.0.0:3001")
        .udp_port(udp_port)
        .require_e2ee(true);

    if let Ok(ip) = var("PUBLIC_IP").and_then(|s| s.parse().map_err(|_| VarError::NotPresent)) {
        builder = builder.public_ip(ip);
    }

    // The WS auth hook validates the session token issued by the HTTP layer
    // and confirms it matches the `Join` payload. This is what binds REST
    // creation to WebSocket connection — no client can forge a Join.
    let sessions_for_ws = sessions.clone();
    builder = builder.on_authenticate(move |pid, room_id, token| {
        let sessions = sessions_for_ws.clone();
        async move {
            let token = token.ok_or_else(|| "missing session token".to_owned())?;
            sessions
                .consume(&token, pid, &room_id)
                .await
                .map_err(|e| e.to_owned())
        }
    });

    let rate_state: Arc<RwLock<HashMap<ParticipantId, (Instant, u32)>>> =
        Arc::new(RwLock::new(HashMap::new()));
    builder = builder.on_rate_check(move |pid| {
        let rate_state = Arc::clone(&rate_state);
        async move {
            let mut state = rate_state.write().await;
            let now = Instant::now();
            let entry = state.entry(pid).or_insert((now, 0));
            if now.duration_since(entry.0) >= RATE_LIMIT_WINDOW {
                *entry = (now, 0);
            }
            entry.1 += 1;
            entry.1 <= RATE_LIMIT_MSGS
        }
    });

    let server = builder.build().await.expect("server init");

    info!("HTTP → http://{http_addr}");
    info!("WS   → ws://0.0.0.0:3001  (terminate TLS upstream in production)");
    info!("UDP  → 0.0.0.0:{}", server.udp_port());

    // ── Background: pending purge ──────────────────────────────────────
    let sessions_for_purge = sessions.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PURGE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            sessions_for_purge.purge_expired().await;
        }
    });

    // ── Background: SFU events ─────────────────────────────────────────
    let mut events = server.events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            info!(?event);
        }
    });

    // ── HTTP server ────────────────────────────────────────────────────
    let app = router(AppState {
        sessions: sessions.clone(),
        auth: auth.clone(),
    });
    let listener = tokio::net::TcpListener::bind(http_addr)
        .await
        .expect("HTTP bind");
    let http_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            warn!(error = %e, "HTTP server stopped");
        }
    });

    // ── Run SFU until shutdown ─────────────────────────────────────────
    server.run().await;

    http_handle.abort();
}
