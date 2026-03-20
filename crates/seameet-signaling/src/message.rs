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
            | Self::Ready { room_id, .. } => Some(room_id),
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
            },
            SdpMessage::Leave {
                participant: id_a(),
                room_id: "room-42".into(),
            },
            SdpMessage::Ready {
                room_id: "r1".into(),
                initiator: true,
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
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        assert!(json.contains("\"type\":\"join\""));
    }

    #[test]
    fn test_sdp_message_room_id() {
        assert_eq!(
            SdpMessage::Join {
                participant: id_a(),
                room_id: "r1".into()
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
                initiator: false
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
    }
}
