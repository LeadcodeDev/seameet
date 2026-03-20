//! Signaling layer for the SeaMeet real-time communication framework.
//!
//! Provides WebSocket-based signaling for SDP offer/answer exchange,
//! ICE candidate forwarding, and room management.

pub mod message;
pub mod room_server;
pub mod traits;
pub mod ws;

pub use message::SdpMessage;
pub use traits::SignalingBackend;
pub use ws::{WsSignaling, WsSignalingConfig};

#[cfg(test)]
mod tests {
    use super::*;
    use room_server::RoomServer;
    use seameet_core::ParticipantId;
    use std::time::Duration;

    /// Starts a local room server and returns the `ws://` URL.
    async fn start_server() -> String {
        let server = RoomServer::bind("127.0.0.1:0").await.expect("bind");
        let addr = server.local_addr().expect("local_addr");
        server.run();
        format!("ws://{addr}")
    }

    #[tokio::test]
    async fn test_ws_connect() {
        let url = start_server().await;
        let ws = WsSignaling::connect(&url).await;
        assert!(ws.is_ok());
    }

    #[tokio::test]
    async fn test_offer_answer_exchange() {
        let url = start_server().await;
        let id_a = ParticipantId::random();
        let id_b = ParticipantId::random();

        let mut a = WsSignaling::connect(&url).await.expect("connect A");
        let mut b = WsSignaling::connect(&url).await.expect("connect B");

        // Both join the same room.
        a.send(SdpMessage::Join {
            participant: id_a,
            room_id: "room-1".into(),
        })
        .await
        .expect("A join");

        b.send(SdpMessage::Join {
            participant: id_b,
            room_id: "room-1".into(),
        })
        .await
        .expect("B join");

        // Small delay so the server registers both peers.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // A sends Offer to B.
        a.send(SdpMessage::Offer {
            from: id_a,
            to: Some(id_b),
            sdp: "offer-sdp".into(),
        })
        .await
        .expect("A send offer");

        let msg = b.recv().await.expect("B recv offer");
        match msg {
            SdpMessage::Offer { from, sdp, .. } => {
                assert_eq!(from, id_a);
                assert_eq!(sdp, "offer-sdp");
            }
            other => panic!("expected Offer, got {other:?}"),
        }

        // B responds with Answer to A.
        b.send(SdpMessage::Answer {
            from: id_b,
            to: id_a,
            sdp: "answer-sdp".into(),
        })
        .await
        .expect("B send answer");

        let msg = a.recv().await.expect("A recv answer");
        match msg {
            SdpMessage::Answer { from, sdp, .. } => {
                assert_eq!(from, id_b);
                assert_eq!(sdp, "answer-sdp");
            }
            other => panic!("expected Answer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_ice_candidate_forward() {
        let url = start_server().await;
        let id_a = ParticipantId::random();
        let id_b = ParticipantId::random();

        let a = WsSignaling::connect(&url).await.expect("connect A");
        let mut b = WsSignaling::connect(&url).await.expect("connect B");

        a.send(SdpMessage::Join {
            participant: id_a,
            room_id: "room-ice".into(),
        })
        .await
        .expect("A join");
        b.send(SdpMessage::Join {
            participant: id_b,
            room_id: "room-ice".into(),
        })
        .await
        .expect("B join");

        tokio::time::sleep(Duration::from_millis(50)).await;

        a.send(SdpMessage::IceCandidate {
            from: id_a,
            to: id_b,
            candidate: "candidate:1 1 udp 2130706431 10.0.0.1 5000 typ host".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
        })
        .await
        .expect("A send ICE");

        let msg = b.recv().await.expect("B recv ICE");
        match msg {
            SdpMessage::IceCandidate {
                from, candidate, ..
            } => {
                assert_eq!(from, id_a);
                assert!(candidate.contains("host"));
            }
            other => panic!("expected IceCandidate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_auto_leave_on_disconnect() {
        let url = start_server().await;
        let id_a = ParticipantId::random();
        let id_b = ParticipantId::random();

        let a = WsSignaling::connect(&url).await.expect("connect A");
        let mut b = WsSignaling::connect(&url).await.expect("connect B");

        a.send(SdpMessage::Join {
            participant: id_a,
            room_id: "room-leave".into(),
        })
        .await
        .expect("A join");
        b.send(SdpMessage::Join {
            participant: id_b,
            room_id: "room-leave".into(),
        })
        .await
        .expect("B join");

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop A's connection.
        a.close().await.expect("A close");
        drop(a);

        // B should receive a Leave message for A.
        let msg = tokio::time::timeout(Duration::from_secs(5), b.recv())
            .await
            .expect("timeout waiting for leave")
            .expect("B recv leave");

        match msg {
            SdpMessage::Leave { participant } => {
                assert_eq!(participant, id_a);
            }
            other => panic!("expected Leave, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_reconnection_backoff() {
        use std::time::Instant;

        // Try to connect to a port that is not listening.
        let config = WsSignalingConfig {
            recv_timeout: Duration::from_secs(5),
            max_reconnect_attempts: 2,
            base_backoff: Duration::from_millis(100),
        };

        let start = Instant::now();
        let result = WsSignaling::connect_with_config("ws://127.0.0.1:1", config).await;
        let elapsed = start.elapsed();

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to connect after 2 attempts"));

        // With base 100ms and 2 attempts: delay 100ms + 200ms = 300ms minimum.
        assert!(
            elapsed >= Duration::from_millis(250),
            "expected backoff delays, elapsed: {elapsed:?}"
        );
    }
}
