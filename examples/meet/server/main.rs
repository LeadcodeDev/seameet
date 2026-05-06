use std::collections::HashMap;
use std::env::{var, VarError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use seameet::{ParticipantId, SeaMeetServer};
use tokio::sync::RwLock;
use tracing::{info, warn};

const RATE_LIMIT_MSGS: u32 = 60;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "meet=info,str0m=warn,str0m::rtp_=error"
                    .parse()
                    .expect("filter")
            }),
        )
        .init();

    let udp_port = var("UDP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10000);

    let mut builder = SeaMeetServer::builder()
        .ws_addr("0.0.0.0:3001")
        .udp_port(udp_port);

    if let Ok(ip) = var("PUBLIC_IP").and_then(|s| s.parse().map_err(|_| VarError::NotPresent)) {
        builder = builder.public_ip(ip);
    }

    builder = match var("SEAMEET_AUTH_TOKEN") {
        Ok(expected) if !expected.is_empty() => {
            info!("auth: shared-secret enabled via SEAMEET_AUTH_TOKEN");
            let expected = Arc::new(expected);
            builder.on_authenticate(move |_pid, _room_id, token| {
                let expected = Arc::clone(&expected);
                async move {
                    match token.as_deref() {
                        Some(t) if t == expected.as_str() => Ok(()),
                        Some(_) => Err("invalid token".to_owned()),
                        None => Err("missing token".to_owned()),
                    }
                }
            })
        }
        _ => {
            warn!(
                "SEAMEET_AUTH_TOKEN not set; running in OPEN mode. \
                 Do not expose this on a public network."
            );
            builder.allow_unauthenticated_joins()
        }
    };

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

    info!("WS  → ws://localhost:3001  (terminate TLS upstream in production)");
    info!("UDP → 0.0.0.0:{}", server.udp_port());

    let mut events = server.events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            info!(?event);
        }
    });

    server.run().await;
}
