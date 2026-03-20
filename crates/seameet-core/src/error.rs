use crate::participant::ParticipantId;

/// All errors that can occur within the SeaMeet system.
#[derive(Debug, thiserror::Error)]
pub enum SeaMeetError {
    /// An I/O error occurred.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A signaling-layer error.
    #[error("signaling error: {0}")]
    Signaling(String),

    /// A codec-layer error.
    #[error("codec error: {0}")]
    Codec(String),

    /// A peer-connection error.
    #[error("peer connection error: {0}")]
    PeerConnection(String),

    /// The room has reached its maximum capacity.
    #[error("room is full")]
    RoomFull,

    /// The requested participant was not found.
    #[error("participant not found: {0}")]
    ParticipantNotFound(ParticipantId),
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, SeaMeetError>;

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn io_error_displays() {
        let err = SeaMeetError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing",
        ));
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn signaling_error_displays() {
        let err = SeaMeetError::Signaling("timeout".into());
        assert_eq!(err.to_string(), "signaling error: timeout");
    }

    #[test]
    fn codec_error_displays() {
        let err = SeaMeetError::Codec("unsupported".into());
        assert_eq!(err.to_string(), "codec error: unsupported");
    }

    #[test]
    fn peer_connection_error_displays() {
        let err = SeaMeetError::PeerConnection("ice failed".into());
        assert_eq!(err.to_string(), "peer connection error: ice failed");
    }

    #[test]
    fn room_full_displays() {
        let err = SeaMeetError::RoomFull;
        assert_eq!(err.to_string(), "room is full");
    }

    #[test]
    fn participant_not_found_displays() {
        let id = ParticipantId::new(Uuid::nil());
        let err = SeaMeetError::ParticipantNotFound(id);
        assert!(err.to_string().contains("participant not found"));
    }
}
