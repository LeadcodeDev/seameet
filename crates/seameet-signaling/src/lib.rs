//! Signaling layer for the SeaMeet real-time communication framework.
//!
//! Provides transport-agnostic signaling for SDP offer/answer exchange,
//! ICE candidate forwarding, and room management.
//!
//! The core types ([`engine`], [`transport`], [`message`]) are always
//! available. WebSocket adapters are gated behind the `tungstenite` feature flag
//! (enabled by default).

pub mod engine;
pub mod message;
pub mod traits;
pub mod transport;

#[cfg(feature = "tungstenite")]
pub mod ws;
#[cfg(feature = "tungstenite")]
pub mod ws_listener;
#[cfg(feature = "tungstenite")]
pub mod room_server;

pub use message::SdpMessage;
pub use traits::SignalingBackend;

#[cfg(feature = "tungstenite")]
pub use ws::{WsSignaling, WsSignalingConfig};

#[cfg(test)]
mod tests {
    use super::*;
    use engine::SignalingState;
    use seameet_core::ParticipantId;
    use std::collections::HashSet;
    use std::time::Duration;

    #[cfg(feature = "tungstenite")]
    use room_server::RoomServer;
    #[cfg(feature = "tungstenite")]
    use ws::WsSignaling;

    /// Starts a local room server and returns the `ws://` URL.
    #[cfg(feature = "tungstenite")]
    async fn start_server() -> String {
        let server = RoomServer::bind("127.0.0.1:0").await.expect("bind");
        let addr = server.local_addr().expect("local_addr");
        server.run();
        format!("ws://{addr}")
    }

    /// Helper: receive the next SdpMessage, skipping Ready, Join, PeerJoined, and RoomStatus control messages.
    #[cfg(feature = "tungstenite")]
    async fn recv_skip_control(ws: &mut WsSignaling) -> SdpMessage {
        loop {
            let msg = ws.recv().await.expect("recv");
            if matches!(msg, SdpMessage::Ready { .. } | SdpMessage::Join { .. } | SdpMessage::PeerJoined { .. } | SdpMessage::RoomStatus { .. }) {
                continue;
            }
            return msg;
        }
    }

    #[cfg(feature = "tungstenite")]
    #[tokio::test]
    async fn test_ws_connect() {
        let url = start_server().await;
        let ws = WsSignaling::connect(&url).await;
        assert!(ws.is_ok());
    }

    #[cfg(feature = "tungstenite")]
    #[tokio::test]
    async fn test_offer_answer_exchange() {
        let url = start_server().await;
        let id_a = ParticipantId::random();
        let id_b = ParticipantId::random();

        let mut a = WsSignaling::connect(&url).await.expect("connect A");
        let mut b = WsSignaling::connect(&url).await.expect("connect B");

        a.send(SdpMessage::Join {
            participant: id_a,
            room_id: "room-1".into(),
            display_name: None,
        })
        .await
        .expect("A join");

        b.send(SdpMessage::Join {
            participant: id_b,
            room_id: "room-1".into(),
            display_name: None,
        })
        .await
        .expect("B join");

        tokio::time::sleep(Duration::from_millis(50)).await;

        a.send(SdpMessage::Offer {
            from: id_a,
            to: Some(id_b),
            room_id: "room-1".into(),
            sdp: "offer-sdp".into(),
        })
        .await
        .expect("A send offer");

        let msg = recv_skip_control(&mut b).await;
        match msg {
            SdpMessage::Offer { from, sdp, .. } => {
                assert_eq!(from, id_a);
                assert_eq!(sdp, "offer-sdp");
            }
            other => panic!("expected Offer, got {other:?}"),
        }

        b.send(SdpMessage::Answer {
            from: id_b,
            to: id_a,
            room_id: "room-1".into(),
            sdp: "answer-sdp".into(),
        })
        .await
        .expect("B send answer");

        let msg = recv_skip_control(&mut a).await;
        match msg {
            SdpMessage::Answer { from, sdp, .. } => {
                assert_eq!(from, id_b);
                assert_eq!(sdp, "answer-sdp");
            }
            other => panic!("expected Answer, got {other:?}"),
        }
    }

    #[cfg(feature = "tungstenite")]
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
            display_name: None,
        })
        .await
        .expect("A join");
        b.send(SdpMessage::Join {
            participant: id_b,
            room_id: "room-ice".into(),
            display_name: None,
        })
        .await
        .expect("B join");

        tokio::time::sleep(Duration::from_millis(50)).await;

        a.send(SdpMessage::IceCandidate {
            from: id_a,
            to: id_b,
            room_id: "room-ice".into(),
            candidate: "candidate:1 1 udp 2130706431 10.0.0.1 5000 typ host".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
        })
        .await
        .expect("A send ICE");

        let msg = recv_skip_control(&mut b).await;
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

    #[cfg(feature = "tungstenite")]
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
            display_name: None,
        })
        .await
        .expect("A join");
        b.send(SdpMessage::Join {
            participant: id_b,
            room_id: "room-leave".into(),
            display_name: None,
        })
        .await
        .expect("B join");

        tokio::time::sleep(Duration::from_millis(50)).await;

        a.close().await.expect("A close");
        drop(a);

        // After disconnect, B receives a RoomStatus without A (replaces PeerLeft).
        // Skip any earlier RoomStatus messages from the join phase.
        let msg = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let msg = b.recv().await.expect("recv");
                if let SdpMessage::RoomStatus { room_id, participants } = &msg {
                    if room_id == "room-leave" && !participants.iter().any(|p| p.id == id_a) {
                        return msg;
                    }
                }
            }
        })
        .await
        .expect("timeout waiting for room_status without A");

        match msg {
            SdpMessage::RoomStatus {
                room_id,
                participants,
            } => {
                assert_eq!(room_id, "room-leave");
                assert!(participants.iter().any(|p| p.id == id_b), "B should still be in room_status");
            }
            other => panic!("expected RoomStatus, got {other:?}"),
        }
    }

    #[cfg(feature = "tungstenite")]
    #[tokio::test]
    async fn test_reconnection_backoff() {
        use std::time::Instant;
        use ws::WsSignalingConfig;

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

        assert!(
            elapsed >= Duration::from_millis(250),
            "expected backoff delays, elapsed: {elapsed:?}"
        );
    }

    // ── New multi-room tests ────────────────────────────────────────────

    #[cfg(feature = "tungstenite")]
    #[tokio::test]
    async fn test_single_ws_two_rooms() {
        let url = start_server().await;
        let id_a = ParticipantId::random();
        let id_b = ParticipantId::random();

        // A joins two rooms on the same WS connection.
        let mut a = WsSignaling::connect(&url).await.expect("connect A");
        let b = WsSignaling::connect(&url).await.expect("connect B");

        a.send(SdpMessage::Join {
            participant: id_a,
            room_id: "alpha".into(),
            display_name: None,
        })
        .await
        .expect("A join alpha");
        a.send(SdpMessage::Join {
            participant: id_a,
            room_id: "beta".into(),
            display_name: None,
        })
        .await
        .expect("A join beta");

        // B joins only "alpha".
        b.send(SdpMessage::Join {
            participant: id_b,
            room_id: "alpha".into(),
            display_name: None,
        })
        .await
        .expect("B join alpha");

        tokio::time::sleep(Duration::from_millis(50)).await;

        // B sends an offer in "alpha".
        b.send(SdpMessage::Offer {
            from: id_b,
            to: Some(id_a),
            room_id: "alpha".into(),
            sdp: "offer-alpha".into(),
        })
        .await
        .expect("B offer");

        // A should receive the offer (skip Ready/Join control messages).
        let msg = recv_skip_control(&mut a).await;
        match msg {
            SdpMessage::Offer {
                from,
                room_id,
                sdp,
                ..
            } => {
                assert_eq!(from, id_b);
                assert_eq!(room_id, "alpha");
                assert_eq!(sdp, "offer-alpha");
            }
            other => panic!("expected Offer, got {other:?}"),
        }
    }

    #[cfg(feature = "tungstenite")]
    #[tokio::test]
    async fn test_disconnect_leaves_all_rooms() {
        let url = start_server().await;
        let id_a = ParticipantId::random();
        let id_b = ParticipantId::random();

        let a = WsSignaling::connect(&url).await.expect("connect A");
        let mut b = WsSignaling::connect(&url).await.expect("connect B");

        // B joins 3 rooms.
        for room in ["r1", "r2", "r3"] {
            b.send(SdpMessage::Join {
                participant: id_b,
                room_id: room.into(),
                display_name: None,
            })
            .await
            .expect("B join");
        }

        // A joins same 3 rooms.
        for room in ["r1", "r2", "r3"] {
            a.send(SdpMessage::Join {
                participant: id_a,
                room_id: room.into(),
                display_name: None,
            })
            .await
            .expect("A join");
        }

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Disconnect A.
        a.close().await.expect("A close");
        drop(a);

        // B should receive RoomStatus (without A) for all 3 rooms.
        let mut leave_rooms = HashSet::new();
        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_secs(2), b.recv()).await {
                Ok(Ok(SdpMessage::RoomStatus {
                    room_id,
                    participants,
                })) => {
                    if !participants.iter().any(|p| p.id == id_a) {
                        leave_rooms.insert(room_id);
                        if leave_rooms.len() == 3 {
                            break;
                        }
                    }
                }
                Ok(Ok(_)) => continue,
                _ => break,
            }
        }

        assert!(
            leave_rooms.contains("r1"),
            "expected RoomStatus without A in r1, got: {leave_rooms:?}"
        );
        assert!(
            leave_rooms.contains("r2"),
            "expected RoomStatus without A in r2, got: {leave_rooms:?}"
        );
        assert!(
            leave_rooms.contains("r3"),
            "expected RoomStatus without A in r3, got: {leave_rooms:?}"
        );
    }

    #[cfg(feature = "tungstenite")]
    #[tokio::test]
    async fn test_initiator_flag() {
        let url = start_server().await;
        let id_a = ParticipantId::random();
        let id_b = ParticipantId::random();

        let mut a = WsSignaling::connect(&url).await.expect("connect A");
        let mut b = WsSignaling::connect(&url).await.expect("connect B");

        // A joins first → should be initiator.
        a.send(SdpMessage::Join {
            participant: id_a,
            room_id: "init-room".into(),
            display_name: None,
        })
        .await
        .expect("A join");

        let msg = a.recv().await.expect("A recv ready");
        match msg {
            SdpMessage::Ready {
                room_id,
                initiator,
                ..
            } => {
                assert_eq!(room_id, "init-room");
                assert!(initiator, "first joiner should be initiator");
            }
            other => panic!("expected Ready, got {other:?}"),
        }

        // B joins second → should NOT be initiator.
        b.send(SdpMessage::Join {
            participant: id_b,
            room_id: "init-room".into(),
            display_name: None,
        })
        .await
        .expect("B join");

        let msg = b.recv().await.expect("B recv ready");
        match msg {
            SdpMessage::Ready {
                room_id,
                initiator,
                ..
            } => {
                assert_eq!(room_id, "init-room");
                assert!(!initiator, "second joiner should not be initiator");
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[cfg(feature = "tungstenite")]
    #[tokio::test]
    async fn test_room_id_routing() {
        let url = start_server().await;
        let id_a = ParticipantId::random();
        let id_b = ParticipantId::random();
        let id_c = ParticipantId::random();

        let a = WsSignaling::connect(&url).await.expect("connect A");
        let mut b = WsSignaling::connect(&url).await.expect("connect B");
        let mut c = WsSignaling::connect(&url).await.expect("connect C");

        // A and B in room "alpha". A and C in room "beta".
        a.send(SdpMessage::Join {
            participant: id_a,
            room_id: "alpha".into(),
            display_name: None,
        })
        .await
        .expect("A join alpha");
        a.send(SdpMessage::Join {
            participant: id_a,
            room_id: "beta".into(),
            display_name: None,
        })
        .await
        .expect("A join beta");
        b.send(SdpMessage::Join {
            participant: id_b,
            room_id: "alpha".into(),
            display_name: None,
        })
        .await
        .expect("B join alpha");
        c.send(SdpMessage::Join {
            participant: id_c,
            room_id: "beta".into(),
            display_name: None,
        })
        .await
        .expect("C join beta");

        tokio::time::sleep(Duration::from_millis(100)).await;

        // A sends an offer in "alpha" to B.
        a.send(SdpMessage::Offer {
            from: id_a,
            to: Some(id_b),
            room_id: "alpha".into(),
            sdp: "offer-for-b".into(),
        })
        .await
        .expect("A offer in alpha");

        // B should receive it.
        let msg = recv_skip_control(&mut b).await;
        match msg {
            SdpMessage::Offer { sdp, .. } => {
                assert_eq!(sdp, "offer-for-b");
            }
            other => panic!("expected Offer for B, got {other:?}"),
        }

        // C should NOT receive it (different room, different target).
        // Drain C's pending messages — should only be Ready/Join, no Offer.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut saw_offer = false;
        loop {
            match tokio::time::timeout(Duration::from_millis(100), c.recv()).await {
                Ok(Ok(SdpMessage::Offer { .. })) => {
                    saw_offer = true;
                    break;
                }
                Ok(Ok(_)) => continue,
                _ => break,
            }
        }
        assert!(!saw_offer, "C should not receive offers from room alpha");
    }

    #[cfg(feature = "tungstenite")]
    #[tokio::test]
    async fn test_screen_share_routing() {
        let url = start_server().await;
        let id_a = ParticipantId::random();
        let id_b = ParticipantId::random();

        let a = WsSignaling::connect(&url).await.expect("connect A");
        let mut b = WsSignaling::connect(&url).await.expect("connect B");

        a.send(SdpMessage::Join {
            participant: id_a,
            room_id: "screen-room".into(),
            display_name: None,
        })
        .await
        .expect("A join");
        b.send(SdpMessage::Join {
            participant: id_b,
            room_id: "screen-room".into(),
            display_name: None,
        })
        .await
        .expect("B join");

        tokio::time::sleep(Duration::from_millis(50)).await;

        a.send(SdpMessage::ScreenShareStarted {
            from: id_a,
            room_id: "screen-room".into(),
            track_id: 99,
        })
        .await
        .expect("A screen share");

        let msg = recv_skip_control(&mut b).await;
        match msg {
            SdpMessage::ScreenShareStarted {
                from, track_id, ..
            } => {
                assert_eq!(from, id_a);
                assert_eq!(track_id, 99);
            }
            other => panic!("expected ScreenShareStarted, got {other:?}"),
        }
    }
}
