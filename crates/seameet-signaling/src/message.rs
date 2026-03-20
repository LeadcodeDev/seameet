use seameet_core::ParticipantId;
use serde::{Deserialize, Serialize};

/// Messages exchanged over the signaling channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SdpMessage {
    /// SDP offer sent by a participant.
    Offer {
        /// Sender of the offer.
        from: ParticipantId,
        /// Optional target; `None` means broadcast.
        to: Option<ParticipantId>,
        /// The SDP payload.
        sdp: String,
    },
    /// SDP answer in response to an offer.
    Answer {
        /// Sender of the answer.
        from: ParticipantId,
        /// Target participant.
        to: ParticipantId,
        /// The SDP payload.
        sdp: String,
    },
    /// ICE candidate for connectivity checks.
    IceCandidate {
        /// Sender of the candidate.
        from: ParticipantId,
        /// Target participant.
        to: ParticipantId,
        /// The ICE candidate string.
        candidate: String,
        /// SDP media identifier.
        sdp_mid: Option<String>,
        /// SDP media line index.
        sdp_mline_index: Option<u16>,
    },
    /// Request to join a room.
    Join {
        /// The joining participant.
        participant: ParticipantId,
        /// Target room identifier.
        room_id: String,
    },
    /// Notification that a participant has left.
    Leave {
        /// The departing participant.
        participant: ParticipantId,
    },
    /// Error response from the server.
    Error {
        /// Error code.
        code: u16,
        /// Human-readable error message.
        message: String,
    },
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
                sdp: "v=0\r\n".into(),
            },
            SdpMessage::Answer {
                from: id_b(),
                to: id_a(),
                sdp: "v=0\r\n".into(),
            },
            SdpMessage::IceCandidate {
                from: id_a(),
                to: id_b(),
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
}
