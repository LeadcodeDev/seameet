use crate::participant::ParticipantId;
use std::any::Any;
use std::fmt;
use std::time::Duration;

/// Events emitted by a room during its lifecycle.
#[non_exhaustive]
pub enum RoomEvent {
    /// A participant joined the room.
    ParticipantJoined(ParticipantId),
    /// A participant left the room.
    ParticipantLeft(ParticipantId),
    /// The room is now empty (all participants have left).
    RoomEmpty,
    /// The room session has ended.
    RoomEnded {
        /// Participants that were in the room.
        participants: Vec<ParticipantId>,
        /// Total duration of the room session.
        duration: Duration,
    },
    /// A participant started speaking.
    SpeechStarted {
        /// The speaking participant.
        participant: ParticipantId,
    },
    /// A participant stopped speaking.
    SpeechEnded {
        /// The participant that stopped speaking.
        participant: ParticipantId,
        /// How long the speech lasted.
        duration: Duration,
    },
    /// A participant has been silent for a given duration.
    Silence {
        /// The silent participant.
        participant: ParticipantId,
        /// How long the silence has lasted.
        duration: Duration,
    },
    /// Network quality degraded for a participant.
    NetworkDegraded {
        /// The affected participant.
        participant: ParticipantId,
        /// Estimated packet loss percentage (0.0–100.0).
        packet_loss_pct: f32,
    },
    /// A custom, application-defined event.
    Custom {
        /// The participant that triggered the event.
        participant: ParticipantId,
        /// Arbitrary payload.
        payload: Box<dyn Any + Send>,
    },
}

impl fmt::Debug for RoomEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParticipantJoined(id) => f.debug_tuple("ParticipantJoined").field(id).finish(),
            Self::ParticipantLeft(id) => f.debug_tuple("ParticipantLeft").field(id).finish(),
            Self::RoomEmpty => write!(f, "RoomEmpty"),
            Self::RoomEnded {
                participants,
                duration,
            } => f
                .debug_struct("RoomEnded")
                .field("participants", participants)
                .field("duration", duration)
                .finish(),
            Self::SpeechStarted { participant } => f
                .debug_struct("SpeechStarted")
                .field("participant", participant)
                .finish(),
            Self::SpeechEnded {
                participant,
                duration,
            } => f
                .debug_struct("SpeechEnded")
                .field("participant", participant)
                .field("duration", duration)
                .finish(),
            Self::Silence {
                participant,
                duration,
            } => f
                .debug_struct("Silence")
                .field("participant", participant)
                .field("duration", duration)
                .finish(),
            Self::NetworkDegraded {
                participant,
                packet_loss_pct,
            } => f
                .debug_struct("NetworkDegraded")
                .field("participant", participant)
                .field("packet_loss_pct", packet_loss_pct)
                .finish(),
            Self::Custom { participant, .. } => f
                .debug_struct("Custom")
                .field("participant", participant)
                .field("payload", &"<dyn Any>")
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_id() -> ParticipantId {
        ParticipantId::new(Uuid::nil())
    }

    #[test]
    fn debug_participant_joined() {
        let evt = RoomEvent::ParticipantJoined(test_id());
        let dbg = format!("{:?}", evt);
        assert!(dbg.contains("ParticipantJoined"));
    }

    #[test]
    fn debug_room_ended() {
        let evt = RoomEvent::RoomEnded {
            participants: vec![test_id()],
            duration: Duration::from_secs(60),
        };
        let dbg = format!("{:?}", evt);
        assert!(dbg.contains("RoomEnded"));
        assert!(dbg.contains("60"));
    }

    #[test]
    fn debug_custom_event() {
        let evt = RoomEvent::Custom {
            participant: test_id(),
            payload: Box::new(42u32),
        };
        let dbg = format!("{:?}", evt);
        assert!(dbg.contains("Custom"));
        assert!(dbg.contains("<dyn Any>"));
    }

    #[test]
    fn debug_network_degraded() {
        let evt = RoomEvent::NetworkDegraded {
            participant: test_id(),
            packet_loss_pct: 12.5,
        };
        let dbg = format!("{:?}", evt);
        assert!(dbg.contains("12.5"));
    }

    #[test]
    fn debug_all_variants() {
        let id = test_id();
        let events: Vec<RoomEvent> = vec![
            RoomEvent::ParticipantJoined(id),
            RoomEvent::ParticipantLeft(id),
            RoomEvent::RoomEmpty,
            RoomEvent::SpeechStarted { participant: id },
            RoomEvent::SpeechEnded {
                participant: id,
                duration: Duration::from_secs(5),
            },
            RoomEvent::Silence {
                participant: id,
                duration: Duration::from_secs(10),
            },
        ];
        for evt in &events {
            let _ = format!("{:?}", evt);
        }
    }
}
