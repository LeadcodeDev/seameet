use std::env::{var, VarError};

use seameet::SeaMeetServer;
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

    let udp_port = std::env::var("UDP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10000);

    let mut builder = SeaMeetServer::builder()
        .ws_addr("0.0.0.0:3001")
        .udp_port(udp_port);

    if let Ok(ip) = var("PUBLIC_IP").and_then(|s| s.parse().map_err(|_| VarError::NotPresent)) {
        builder = builder.public_ip(ip);
    }

    let server = builder.build().await.expect("server init");

    info!("WS  → ws://localhost:3001");
    info!("UDP → 0.0.0.0:{}", server.udp_port());

    let mut events = server.events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            info!(?event);
        }
    });

    server.run().await;
}
