use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Unique identifier for a participant in a room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParticipantId(Uuid);

impl ParticipantId {
    /// Creates a new [`ParticipantId`] from the given UUID.
    pub fn new(id: Uuid) -> Self {
        Self(id)
    }

    /// Generates a random [`ParticipantId`] (UUID v4).
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }

    /// Returns the inner UUID.
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_ids_are_unique() {
        let a = ParticipantId::random();
        let b = ParticipantId::random();
        assert_ne!(a, b);
    }

    #[test]
    fn display_matches_uuid() {
        let uuid = Uuid::new_v4();
        let id = ParticipantId::new(uuid);
        assert_eq!(id.to_string(), uuid.to_string());
    }

    #[test]
    fn serde_roundtrip() {
        let id = ParticipantId::random();
        let json = serde_json::to_string(&id).expect("serialize");
        let back: ParticipantId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }

    #[test]
    fn hash_eq_consistency() {
        use std::collections::HashSet;
        let id = ParticipantId::random();
        let mut set = HashSet::new();
        set.insert(id);
        assert!(set.contains(&id));
    }
}
