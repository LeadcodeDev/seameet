mod auth_jwks;
mod http;

use std::collections::HashMap;
use std::env::{var, VarError};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use seameet::{ParticipantId, SeaMeetServer};
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth_jwks::{JwksAuth, JwksConfig, DEFAULT_PID_NAMESPACE};
use crate::http::{router, AppState, AuthOutcome, AuthProvider};

const JWKS_REFRESH_INTERVAL: Duration = Duration::from_secs(300);

const RATE_LIMIT_MSGS: u32 = 200;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(1);

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

/// Build a JwksAuth from env vars if `SEAMEET_JWKS_URL`,
/// `SEAMEET_OIDC_ISSUER`, and `SEAMEET_OIDC_AUDIENCE` are all set. Returns
/// `None` when any required value is missing — the caller falls back to the
/// shared-secret or open provider.
fn build_jwks_auth() -> Option<JwksAuth> {
    let jwks_url = var("SEAMEET_JWKS_URL").ok().filter(|s| !s.is_empty())?;
    let issuer = var("SEAMEET_OIDC_ISSUER").ok().filter(|s| !s.is_empty())?;
    let audience = var("SEAMEET_OIDC_AUDIENCE").ok().filter(|s| !s.is_empty())?;
    let pid_namespace = var("SEAMEET_PID_NAMESPACE")
        .ok()
        .and_then(|s| Uuid::from_str(s.trim()).ok())
        .unwrap_or(DEFAULT_PID_NAMESPACE);
    Some(JwksAuth::new(JwksConfig {
        issuer,
        audience,
        jwks_url,
        pid_namespace,
    }))
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

    // ── Auth provider selection ────────────────────────────────────────
    let auth: Arc<dyn AuthProvider> = if let Some(jwks_auth) = build_jwks_auth() {
        info!(
            issuer = %jwks_auth.config().issuer,
            audience = %jwks_auth.config().audience,
            jwks_url = %jwks_auth.config().jwks_url,
            "auth: JWKS (OIDC) provider enabled",
        );
        let arc = Arc::new(jwks_auth);
        Arc::clone(&arc).spawn_refresh_loop(JWKS_REFRESH_INTERVAL);
        arc
    } else {
        match var("SEAMEET_AUTH_TOKEN") {
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

    // The WS auth hook receives the bearer token sent by the client in the
    // `Join` payload. THIS IS WHERE INTEGRATORS PLUG THEIR IAM.
    //
    // In a real deployment, the body of this closure should decode and
    // verify the JWT signature against your IdP's JWKS, check `iss`/`aud`/
    // `exp`, and confirm the `sub` claim resolves to the `pid` parameter.
    // The `JwksAuth` provider in `auth_jwks.rs` shows one such pattern.
    //
    // In this example we simply accept any non-empty token. The contract
    // we still demonstrate: the client MUST send a token. If it doesn't,
    // the join is rejected — that's the integration point you can't skip.
    builder = builder.on_authenticate(|_pid, _room_id, token| async move {
        match token {
            Some(t) if !t.is_empty() => Ok(()),
            _ => Err("missing token".to_owned()),
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

    // ── Background: SFU events ─────────────────────────────────────────
    let mut events = server.events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            info!(?event);
        }
    });

    // ── HTTP server ────────────────────────────────────────────────────
    let app = router(AppState { auth: auth.clone() });
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
