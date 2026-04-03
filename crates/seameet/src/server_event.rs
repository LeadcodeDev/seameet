use seameet_core::ParticipantId;

/// Events emitted by [`super::SeaMeetServer`] during its lifetime.
///
/// Subscribe via [`super::SeaMeetServer::events`]. The channel is backed by
/// [`tokio::sync::broadcast`], so lagged receivers simply miss events — the
/// server is never blocked.
#[derive(Debug, Clone)]
pub enum ServerEvent {
    PeerConnected {
        participant: ParticipantId,
        room_id: String,
    },
    PeerDisconnected {
        participant: ParticipantId,
        rooms: Vec<String>,
    },
    RoomCreated {
        room_id: String,
    },
    RoomDestroyed {
        room_id: String,
    },
    AuthRejected {
        participant: ParticipantId,
        room_id: String,
        reason: String,
    },
    RateLimited {
        participant: ParticipantId,
    },
    MessageReceived {
        participant: ParticipantId,
        message_type: String,
    },
}
