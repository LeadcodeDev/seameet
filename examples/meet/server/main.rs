use std::sync::Arc;

use seameet::sfu::{SfuConfig, SfuServer};
use seameet::{run_connection, TransportListener, WsListener};
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "meet=debug,str0m=warn,str0m::rtp_=error"
                    .parse()
                    .expect("filter")
            }),
        )
        .init();

    let config = SfuConfig {
        udp_port: std::env::var("UDP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10000),
        public_ip: std::env::var("PUBLIC_IP").ok().and_then(|s| s.parse().ok()),
        ..Default::default()
    };

    let sfu = SfuServer::new(config).await.expect("SFU init");
    let state = sfu.signaling_state();

    let mut listener = WsListener::bind("0.0.0.0:3001").await.expect("bind WS");
    info!("WS  → ws://localhost:3001");
    info!("UDP → 0.0.0.0:{}", sfu.udp_local_addr().port());

    while let Some(conn) = listener.accept().await {
        tokio::spawn(run_connection(conn, Arc::clone(&state), Arc::clone(&sfu)));
    }
}
