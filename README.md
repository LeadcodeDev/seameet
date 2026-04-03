# seameet

A modular WebRTC SFU (Selective Forwarding Unit) written in Rust. seameet receives, processes, and re-emits audio/video streams between browser participants in real time.

Built on top of [str0m](https://crates.io/crates/str0m) for the WebRTC stack and [tokio](https://tokio.rs) for async I/O.

[![Crates.io](https://img.shields.io/crates/v/seameet.svg)](https://crates.io/crates/seameet)
[![docs.rs](https://docs.rs/rustmotion/badge.svg)](https://docs.rs/seameet)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## Why an SFU?

In a video conference, every participant produces audio and video streams that need to reach everyone else. There are three ways to architect this:

```
Mesh (P2P)                        MCU                             SFU

 A ──────── B               A ──→ ┌─────┐ ──→ A          A ──→ ┌─────┐ ──→ B
 │ ╲      ╱ │               B ──→ │ Mix │ ──→ B          B ──→ │ Fwd │ ──→ A
 │  ╲    ╱  │               C ──→ │     │ ──→ C          C ──→ │     │ ──→ A
 │   ╲  ╱   │               D ──→ └─────┘ ──→ D          D ──→ └─────┘ ──→ B
 C ──────── D                  decode+mix+encode              forward as-is

 Each uploads N-1 times.    Server decodes, mixes           Each uploads once.
 No server needed, but      into one stream, and            Server only routes
 collapses above 3-4        re-encodes. Very high           packets — no transcoding.
 participants.              CPU cost on server.             Scales well.
```

The SFU model is the sweet spot for real-time conferencing: it removes the upload bottleneck from clients (each stream is sent only once) while keeping server costs low (no transcoding). The server can also make smart forwarding decisions like only relaying the active speaker in high resolution.

seameet implements the SFU approach. The signaling layer (offer/answer, ICE, mute/unmute) runs over WebSocket, and media flows over UDP using RTP/RTCP.

## Architecture

seameet is organized as a Cargo workspace of 7 crates, each with a single responsibility:

| Crate               | Role                                                                                                             |
| ------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `seameet-core`      | Shared types, traits, and errors (`ParticipantId`, `TrackId`, `RoomEvent`, `Processor`, `Encoder`/`Decoder`)     |
| `seameet-signaling` | WebSocket signaling engine — dispatches join, offer/answer, ICE candidates, mute/unmute, E2EE key exchange, chat |
| `seameet-rtp`       | RTP/RTCP packet parsing and serialization                                                                        |
| `seameet-codec`     | Audio/video codecs: Opus encoder/decoder, VP8 encoder/decoder, passthrough mode                                  |
| `seameet-pipeline`  | Inbound/outbound media pipelines with composable `ProcessorChain`s, wraps `str0m` PeerConnections                |
| `seameet-sfu`       | SFU server logic — receives streams and selectively forwards them to other peers                                 |
| `seameet`           | Public facade crate — re-exports the full API behind feature flags                                               |

```
┌────────────────────────────────────────────────────────────────────┐
│                          seameet (facade)                          │
│                                                                    │
│  ┌──────────┐  ┌───────────┐  ┌─────────┐  ┌────────────────┐      │
│  │  codec   │  │ signaling │  │   rtp   │  │    pipeline    │      │
│  │ Opus,VP8 │  │ WebSocket │  │ RTP/RTCP│  │ In/Out chains  │      │
│  └────┬─────┘  └─────┬─────┘  └────┬────┘  └───────┬────────┘      │
│       │              │             │                │              │
│       └──────────────┴─────────────┴────────────────┘              │
│                              │                                     │
│                       ┌──────┴──────┐                              │
│                       │    core     │                              │
│                       │ types+traits│                              │
│                       └─────────────┘                              │
│                                                                    │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │                     sfu (optional)                           │  │
│  │  SeaMeetServer builder, room routing, ServerEvent stream     │  │
│  └──────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────┘
```

## Usage

### Room API

The `Room` is the high-level abstraction for managing participants and media. You create a room, add participants with a signaling backend and a media processor, and react to events asynchronously.

A `Room` internally spawns per-participant tasks that drive the PeerConnection (UDP I/O, SDP commands) and forward RTP packets between all peers following the SFU pattern. Decoded audio frames are published on broadcast channels you can subscribe to via `RoomHandle`.

```rust
use seameet::{Room, RoomConfig, RoomEvent, Passthrough, WsSignaling, ParticipantId};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> Result<(), seameet::SeaMeetError> {
    // 1. Create a room with default settings (30 fps, 20ms audio frames)
    let room = Room::new(RoomConfig {
        name: Some("team-standup".into()),
        max_participants: 10,
        ..Default::default()
    });

    // 2. Subscribe to room-level events (joins, leaves, speech detection)
    let mut events = room.events();

    // 3. Connect a participant via WebSocket signaling.
    //    `Passthrough` means no server-side encoding — raw RTP is forwarded.
    let signaling = WsSignaling::connect("ws://localhost:8080").await?;
    let handle = room.add_participant(
        ParticipantId::random(),
        signaling,
        Passthrough,
    ).await?;

    // 4. The handle gives access to decoded media streams
    let mut audio = handle.audio_rx();
    let mut video = handle.video_rx();

    // 5. React to room lifecycle events
    while let Some(event) = events.next().await {
        match event {
            RoomEvent::ParticipantJoined(pid) => {
                println!("{pid} joined the room");
            }
            RoomEvent::SpeechStarted { participant } => {
                println!("{participant} started speaking");
            }
            RoomEvent::RoomEnded { .. } => break,
            _ => {}
        }
    }
    Ok(())
}
```

The sequence of operations when a participant joins a room:

```
Browser                      Room                     PeerConnection
  │                           │                            │
  │  WsSignaling::connect()   │                            │
  │──────────────────────────→│                            │
  │                           │  add_participant(id, ws)   │
  │                           │───────────────────────────→│ bind UDP socket
  │                           │                            │ spawn drive task
  │                           │                            │ spawn forward task
  │                           │  RoomHandle                │
  │                           │←───────────────────────────│
  │                           │                            │
  │          SDP offer        │                            │
  │──────────────────────────→│  handle.accept_offer(sdp)  │
  │                           │───────────────────────────→│
  │          SDP answer       │                            │
  │←──────────────────────────│←───────────────────────────│
  │                           │                            │
  │       ICE candidates      │                            │
  │←─────────────────────────→│  add_ice_candidate()       │
  │                           │───────────────────────────→│
  │                           │                            │
  │        RTP media ════════════════════════════════════════
  │                           │                            │
  │                           │  RoomEvent::ParticipantJoined
  │                           │──→ event stream            │
```

### SFU server

For production use, `SeaMeetServer` wraps the SFU into a ready-to-run server with a builder API. It handles WebSocket listener setup, UDP binding, and connection lifecycle automatically.

The builder exposes hooks for authentication, rate limiting, and custom connection handling. Server events (peer connect/disconnect, room create/destroy, auth rejection) are emitted on a broadcast channel.

```rust
use seameet::SeaMeetServer;

#[tokio::main]
async fn main() {
    let server = SeaMeetServer::builder()
        // Transport
        .ws_addr("0.0.0.0:3001")       // WebSocket signaling endpoint
        .udp_port(10000)                // RTP media port
        // Limits
        .max_room_members(20)
        .max_chat_history(200)
        // Authentication hook
        .on_authenticate(|participant, room_id, token| async move {
            match token {
                Some(t) if t == "secret" => Ok(()),
                _ => Err("invalid token".into()),
            }
        })
        .build()
        .await
        .expect("server init");

    // Subscribe to server lifecycle events
    let mut events = server.events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            println!("{event:?}");
            // ServerEvent::PeerConnected { participant, room_id }
            // ServerEvent::RoomCreated { room_id }
            // ServerEvent::AuthRejected { participant, room_id, reason }
            // ServerEvent::RateLimited { participant }
            // ...
        }
    });

    server.run().await;
}
```

The connection lifecycle from browser to SFU:

```
Browser                   WsListener              SignalingEngine            SfuServer
  │                          │                          │                       │
  │   WS connect             │                          │                       │
  │─────────────────────────→│  accept()                │                       │
  │                          │─────────────────────────→│                       │
  │                          │                          │                       │
  │   { "type": "join",      │                          │                       │
  │     "room_id": "abc",    │  on_message(Join)        │                       │
  │     "token": "secret" }  │─────────────────────────→│                       │
  │                          │                          │  on_authenticate()    │
  │                          │                          │──────────────────────→│
  │                          │                          │  Ok(())               │
  │                          │                          │←──────────────────────│
  │                          │                          │                       │
  │                          │                          │  create/join room     │
  │                          │                          │──────────────────────→│
  │   { "type": "ready",     │                          │                       │
  │     "peers": [...] }     │                          │                       │
  │←─────────────────────────│←─────────────────────────│                       │
  │                          │                          │                       │
  │   SDP offer/answer       │                          │   ICE + DTLS          │
  │←────────────────────────→│←────────────────────────→│←─────────────────────→│
  │                          │                          │                       │
  │   RTP/RTCP (UDP) ═══════════════════════════════════════════════════════════
  │                          │                          │                       │
  │   { "type": "mute_video" }                         │                        │
  │─────────────────────────→│─────────────────────────→│  broadcast to room    │
  │                          │                          │──────────────────────→│
```

## Demo app

A complete video-conferencing application lives in `examples/meet/` with a React + TypeScript frontend and the Rust SFU server.

### Development

```bash
# Start the SFU server (uses features = ["sfu", "tungstenite"])
cd examples/meet/server
cargo run

# In another terminal, start the frontend dev server
cd examples/meet/frontend
pnpm install
npm run dev
```

The server listens on `ws://localhost:3001` (signaling) and UDP port `10000` (media). The frontend dev server runs on `http://localhost:5173`.

### Docker

```bash
# Build and run the SFU server
docker build -f examples/meet/server/Dockerfile -t seameet-server .
docker run -p 3001:3001 -p 10000:10000/udp seameet-server

# Build and run the frontend
docker build -f examples/meet/frontend/Dockerfile -t seameet-frontend .
docker run -p 5173:5173 seameet-frontend
```

Environment variables for the server:

| Variable    | Default                 | Description                                        |
| ----------- | ----------------------- | -------------------------------------------------- |
| `UDP_PORT`  | `10000`                 | UDP port for RTP media                             |
| `PUBLIC_IP` | auto-detect             | Public IP for ICE candidates (required behind NAT) |
| `RUST_LOG`  | `meet=debug,str0m=warn` | Log filter                                         |

## Feature flags

The `seameet` facade crate uses feature flags to keep the dependency tree lean:

| Flag          | Default | What it enables                                       |
| ------------- | ------- | ----------------------------------------------------- |
| `tungstenite` | yes     | WebSocket signaling transport via `tokio-tungstenite` |
| `sfu`         | no      | `SeaMeetServer` builder and the `seameet-sfu` crate   |
| `opus-ffi`    | no      | Native Opus encoder/decoder through FFI bindings      |
| `vp8-ffi`     | no      | Native VP8 encoder/decoder through FFI bindings       |

```toml
# Minimal — just the Room API with WebSocket signaling
seameet = "0.1"

# Full SFU server
seameet = { version = "0.1", features = ["sfu"] }

# With native codecs
seameet = { version = "0.1", features = ["sfu", "opus-ffi", "vp8-ffi"] }
```
