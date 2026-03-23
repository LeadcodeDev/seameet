use std::collections::HashMap;

use seameet_core::ParticipantId;
use serde::{Deserialize, Serialize};

/// Messages exchanged over the signaling channel.
///
/// Every variant except [`Error`](SdpMessage::Error) carries a `room_id`
/// so that a single WebSocket connection can multiplex multiple rooms.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SdpMessage {
    /// Request to join a room.
    Join {
        /// The joining participant.
        participant: ParticipantId,
        /// Target room identifier.
        room_id: String,
        /// Optional human-readable name for this participant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
    },
    /// Notification that a participant has left a room.
    Leave {
        /// The departing participant.
        participant: ParticipantId,
        /// The room being left.
        room_id: String,
    },
    /// SDP offer sent by a participant.
    Offer {
        /// Sender of the offer.
        from: ParticipantId,
        /// Optional target; `None` means broadcast.
        to: Option<ParticipantId>,
        /// The room this offer belongs to.
        room_id: String,
        /// The SDP payload.
        sdp: String,
    },
    /// SDP answer in response to an offer.
    Answer {
        /// Sender of the answer.
        from: ParticipantId,
        /// Target participant.
        to: ParticipantId,
        /// The room this answer belongs to.
        room_id: String,
        /// The SDP payload.
        sdp: String,
    },
    /// ICE candidate for connectivity checks.
    IceCandidate {
        /// Sender of the candidate.
        from: ParticipantId,
        /// Target participant.
        to: ParticipantId,
        /// The room this candidate belongs to.
        room_id: String,
        /// The ICE candidate string.
        candidate: String,
        /// SDP media identifier.
        sdp_mid: Option<String>,
        /// SDP media line index.
        sdp_mline_index: Option<u16>,
    },
    /// Sent by the server to indicate room readiness.
    Ready {
        /// The room that is ready.
        room_id: String,
        /// Whether this peer should initiate the offer.
        initiator: bool,
        /// List of peers already present in the room.
        #[serde(default)]
        peers: Vec<ParticipantId>,
        /// Display names of the existing peers, keyed by participant ID.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        display_names: HashMap<String, String>,
    },
    /// Sent by the server when a new peer joins the room.
    PeerJoined {
        /// The joining participant.
        participant: ParticipantId,
        /// The room identifier.
        room_id: String,
        /// Optional human-readable name for this participant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
    },
    /// Sent by the server when a peer leaves the room.
    PeerLeft {
        /// The departing participant.
        participant: ParticipantId,
        /// The room identifier.
        room_id: String,
    },
    /// Emitted by the client when a screen share starts.
    ScreenShareStarted {
        /// The participant sharing their screen.
        from: ParticipantId,
        /// The room this share belongs to.
        room_id: String,
        /// Client-assigned track identifier.
        track_id: u32,
    },
    /// Emitted by the client when a screen share stops.
    ScreenShareStopped {
        /// The participant that stopped sharing.
        from: ParticipantId,
        /// The room this share belongs to.
        room_id: String,
        /// Track identifier that was stopped.
        track_id: u32,
    },
    /// Emitted by the client when they mute their microphone.
    MuteAudio {
        /// The participant muting.
        from: ParticipantId,
        /// The room this applies to.
        room_id: String,
    },
    /// Emitted by the client when they unmute their microphone.
    UnmuteAudio {
        /// The participant unmuting.
        from: ParticipantId,
        /// The room this applies to.
        room_id: String,
    },
    /// Emitted by the client when they change video settings.
    VideoConfigChanged {
        /// The participant changing config.
        from: ParticipantId,
        /// The room this applies to.
        room_id: String,
        /// Video width in pixels.
        width: u32,
        /// Video height in pixels.
        height: u32,
        /// Frames per second.
        fps: u32,
    },
    /// Error response from the server.
    Error {
        /// Error code.
        code: u16,
        /// Human-readable error message.
        message: String,
    },
}

impl SdpMessage {
    /// Returns the `room_id` of the message, or `None` for [`Error`](SdpMessage::Error).
    pub fn room_id(&self) -> Option<&str> {
        match self {
            Self::Join { room_id, .. }
            | Self::Leave { room_id, .. }
            | Self::Offer { room_id, .. }
            | Self::Answer { room_id, .. }
            | Self::IceCandidate { room_id, .. }
            | Self::Ready { room_id, .. }
            | Self::PeerJoined { room_id, .. }
            | Self::PeerLeft { room_id, .. }
            | Self::ScreenShareStarted { room_id, .. }
            | Self::ScreenShareStopped { room_id, .. }
            | Self::MuteAudio { room_id, .. }
            | Self::UnmuteAudio { room_id, .. }
            | Self::VideoConfigChanged { room_id, .. } => Some(room_id),
            Self::Error { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seameet_core::ParticipantId;

    fn id_a() -> ParticipantId {
        ParticipantId::new(uuid::Uuid::nil())
    }

    fn id_b() -> ParticipantId {
        ParticipantId::new(uuid::Uuid::from_u128(1))
    }

    #[test]
    fn test_sdp_message_serde() {
        let messages = vec![
            SdpMessage::Offer {
                from: id_a(),
                to: Some(id_b()),
                room_id: "r1".into(),
                sdp: "v=0\r\n".into(),
            },
            SdpMessage::Answer {
                from: id_b(),
                to: id_a(),
                room_id: "r1".into(),
                sdp: "v=0\r\n".into(),
            },
            SdpMessage::IceCandidate {
                from: id_a(),
                to: id_b(),
                room_id: "r1".into(),
                candidate: "candidate:1 1 udp 2130706431 10.0.0.1 5000 typ host".into(),
                sdp_mid: Some("0".into()),
                sdp_mline_index: Some(0),
            },
            SdpMessage::Join {
                participant: id_a(),
                room_id: "room-42".into(),
                display_name: None,
            },
            SdpMessage::Leave {
                participant: id_a(),
                room_id: "room-42".into(),
            },
            SdpMessage::Ready {
                room_id: "r1".into(),
                initiator: true,
                peers: vec![],
                display_names: HashMap::new(),
            },
            SdpMessage::Error {
                code: 404,
                message: "not found".into(),
            },
        ];

        for msg in &messages {
            let json = serde_json::to_string(msg).expect("serialize");
            let back: SdpMessage = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(&back, msg);
        }
    }

    #[test]
    fn test_serde_tag_field() {
        let msg = SdpMessage::Join {
            participant: id_a(),
            room_id: "test".into(),
            display_name: None,
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains("\"type\":\"join\""));
    }

    #[test]
    fn test_sdp_message_room_id() {
        assert_eq!(
            SdpMessage::Join {
                participant: id_a(),
                room_id: "r1".into(),
                display_name: None,
            }
            .room_id(),
            Some("r1")
        );
        assert_eq!(
            SdpMessage::Leave {
                participant: id_a(),
                room_id: "r2".into()
            }
            .room_id(),
            Some("r2")
        );
        assert_eq!(
            SdpMessage::Offer {
                from: id_a(),
                to: None,
                room_id: "r3".into(),
                sdp: String::new()
            }
            .room_id(),
            Some("r3")
        );
        assert_eq!(
            SdpMessage::Ready {
                room_id: "r4".into(),
                initiator: false,
                peers: vec![],
                display_names: HashMap::new(),
            }
            .room_id(),
            Some("r4")
        );
        assert_eq!(
            SdpMessage::Error {
                code: 0,
                message: String::new()
            }
            .room_id(),
            None
        );
        assert_eq!(
            SdpMessage::ScreenShareStarted {
                from: id_a(),
                room_id: "r5".into(),
                track_id: 1
            }
            .room_id(),
            Some("r5")
        );
    }

    #[test]
    fn test_screen_share_started_message_serde() {
        let msg = SdpMessage::ScreenShareStarted {
            from: id_a(),
            room_id: "r1".into(),
            track_id: 42,
        };
        let json = serde_json::to_string(&msg).expect("ser");
        assert!(json.contains("\"type\":\"screen_share_started\""));
        let back: SdpMessage = serde_json::from_str(&json).expect("de");
        assert_eq!(back, msg);

        let msg2 = SdpMessage::ScreenShareStopped {
            from: id_a(),
            room_id: "r1".into(),
            track_id: 42,
        };
        let json2 = serde_json::to_string(&msg2).expect("ser");
        let back2: SdpMessage = serde_json::from_str(&json2).expect("de");
        assert_eq!(back2, msg2);
    }

    #[test]
    fn test_mute_audio_serde() {
        let msg = SdpMessage::MuteAudio {
            from: id_a(),
            room_id: "r1".into(),
        };
        let json = serde_json::to_string(&msg).expect("ser");
        assert!(json.contains("\"type\":\"mute_audio\""));
        let back: SdpMessage = serde_json::from_str(&json).expect("de");
        assert_eq!(back, msg);
        assert_eq!(msg.room_id(), Some("r1"));

        let msg2 = SdpMessage::UnmuteAudio {
            from: id_a(),
            room_id: "r1".into(),
        };
        let json2 = serde_json::to_string(&msg2).expect("ser");
        assert!(json2.contains("\"type\":\"unmute_audio\""));
        let back2: SdpMessage = serde_json::from_str(&json2).expect("de");
        assert_eq!(back2, msg2);
        assert_eq!(msg2.room_id(), Some("r1"));
    }

    #[test]
    fn test_video_config_changed_serde() {
        let msg = SdpMessage::VideoConfigChanged {
            from: id_a(),
            room_id: "r1".into(),
            width: 1920,
            height: 1080,
            fps: 30,
        };
        let json = serde_json::to_string(&msg).expect("ser");
        assert!(json.contains("\"type\":\"video_config_changed\""));
        assert!(json.contains("\"width\":1920"));
        assert!(json.contains("\"height\":1080"));
        assert!(json.contains("\"fps\":30"));
        let back: SdpMessage = serde_json::from_str(&json).expect("de");
        assert_eq!(back, msg);
        assert_eq!(msg.room_id(), Some("r1"));
    }
}
