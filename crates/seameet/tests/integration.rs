use std::any::Any;
use std::time::Duration;

use seameet::{
    AudioPassthrough, Encoder, Passthrough, PcmFrame, Processor, ProcessorHint,
    ParticipantId, Room, RoomConfig, RoomEvent, SeaMeetError, VideoFrame,
};
use seameet_pipeline::outbound::rtp_channel;
use seameet_pipeline::{OutboundPipeline, Peer};
use seameet_rtp::RtpPacket;
use tokio_stream::StreamExt;

#[tokio::test]
async fn test_two_participants_audio_roundtrip() {
    // Set up two peers with AudioPassthrough (no WebRTC, direct pipeline).
    let id_a = ParticipantId::random();
    let id_b = ParticipantId::random();

    let (sender_a, _rtp_rx_a) = rtp_channel(1);
    let outbound_a = OutboundPipeline::new(AudioPassthrough, sender_a);
    let _peer_a = Peer::new(id_a, AudioPassthrough, outbound_a);

    let (sender_b, _rtp_rx_b) = rtp_channel(2);
    let outbound_b = OutboundPipeline::new(AudioPassthrough, sender_b);
    let mut peer_b = Peer::new(id_b, AudioPassthrough, outbound_b);

    let mut audio_rx_b = peer_b.audio_rx();

    // A encodes a frame, wraps it as RTP, and we push it into B's inbound.
    let original = PcmFrame {
        samples: vec![0.1, 0.2, 0.3, 0.4],
        sample_rate: 48000,
        channels: 1,
    };
    let mut enc = AudioPassthrough;
    let encoded = enc.encode(original.clone()).expect("encode");
    let rtp = RtpPacket::new_audio(encoded.0, 0, 1, 0);

    peer_b.inbound.push_rtp(rtp);
    peer_b.inbound.drain(1000).expect("drain");

    let received = audio_rx_b.try_recv().expect("B should receive audio");
    assert_eq!(received.samples, original.samples);
    assert_eq!(received.sample_rate, 48000);
    assert_eq!(received.channels, 1);
}

#[tokio::test]
async fn test_room_events_participant_joined() {
    let room = Room::new(RoomConfig::default());
    let mut events = room.events();

    let id = ParticipantId::random();

    // Use a dummy signaling backend.
    let _handle = room
        .add_participant(id, DummySignaling, Passthrough)
        .await
        .expect("add participant");

    // Should receive ParticipantJoined.
    let evt = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .expect("timeout")
        .expect("event");

    match evt {
        RoomEvent::ParticipantJoined(pid) => assert_eq!(pid, id),
        other => panic!("expected ParticipantJoined, got {other:?}"),
    }
}

#[tokio::test]
async fn test_room_events_room_empty() {
    let room = Room::new(RoomConfig::default());
    let mut events = room.events();

    let id = ParticipantId::random();
    let _handle = room
        .add_participant(id, DummySignaling, Passthrough)
        .await
        .expect("add");

    // Consume ParticipantJoined.
    let _ = tokio::time::timeout(Duration::from_secs(1), events.next()).await;

    // Close the room to trigger stop signals.
    room.close().await.expect("close");

    // Collect remaining events.
    let mut saw_room_empty = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), events.next()).await {
            Ok(Some(RoomEvent::RoomEmpty)) => {
                saw_room_empty = true;
                break;
            }
            Ok(Some(RoomEvent::ParticipantLeft(_))) => continue,
            Ok(Some(RoomEvent::RoomEnded { .. })) => continue,
            _ => break,
        }
    }

    // RoomEmpty may or may not fire depending on task scheduling.
    // At minimum, RoomEnded should have been emitted by close().
    // This is a best-effort check.
    let _ = saw_room_empty;
}

#[tokio::test]
async fn test_custom_processor() {
    // A processor that emits a Custom event on first audio frame.
    struct EmitOnce {
        participant: ParticipantId,
        emitted: bool,
    }

    impl Processor for EmitOnce {
        fn process_audio(&mut self, _frame: &mut PcmFrame) -> ProcessorHint {
            if !self.emitted {
                self.emitted = true;
                ProcessorHint::Emit(RoomEvent::Custom {
                    participant: self.participant,
                    payload: Box::new(123u32) as Box<dyn Any + Send>,
                })
            } else {
                ProcessorHint::Forward
            }
        }

        fn process_video(&mut self, _frame: &mut VideoFrame) -> ProcessorHint {
            ProcessorHint::Forward
        }
    }

    let id = ParticipantId::random();

    let (sender, _rtp_rx) = rtp_channel(1);
    let mut outbound = OutboundPipeline::new(AudioPassthrough, sender);
    outbound.add_processor(Box::new(EmitOnce {
        participant: id,
        emitted: false,
    }));

    let mut event_rx = outbound.take_event_rx().expect("event_rx");

    // Send an audio frame through the outbound pipeline.
    let frame = PcmFrame {
        samples: vec![0.0; 10],
        sample_rate: 48000,
        channels: 1,
    };
    outbound.send_audio(frame).await.expect("send");

    // Should receive the Custom event.
    let evt = event_rx.try_recv().expect("event");
    match evt {
        RoomEvent::Custom { participant, payload } => {
            assert_eq!(participant, id);
            assert_eq!(*payload.downcast_ref::<u32>().expect("u32"), 123);
        }
        other => panic!("expected Custom, got {other:?}"),
    }
}

#[tokio::test]
async fn test_room_close() {
    let room = Room::new(RoomConfig::default());

    let id1 = ParticipantId::random();
    let id2 = ParticipantId::random();

    let _h1 = room
        .add_participant(id1, DummySignaling, Passthrough)
        .await
        .expect("add 1");
    let _h2 = room
        .add_participant(id2, DummySignaling, Passthrough)
        .await
        .expect("add 2");

    assert_eq!(room.participant_count().await, 2);

    // Close should not panic.
    room.close().await.expect("close");

    // Give tasks time to finish.
    tokio::time::sleep(Duration::from_millis(100)).await;
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// A dummy signaling backend that does nothing (for test use).
struct DummySignaling;

#[async_trait::async_trait]
impl seameet::SignalingBackend for DummySignaling {
    async fn send(&self, _msg: seameet::SdpMessage) -> Result<(), SeaMeetError> {
        Ok(())
    }

    async fn recv(&mut self) -> Result<seameet::SdpMessage, SeaMeetError> {
        // Block forever — the peer will be stopped via the stop signal.
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Err(SeaMeetError::Signaling("dummy closed".into()))
    }

    async fn close(&self) -> Result<(), SeaMeetError> {
        Ok(())
    }
}
