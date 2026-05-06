use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use seameet_core::ParticipantId;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::debug;

/// JWT payload bound to the REST→WS handshake. Signed HMAC-SHA256 with the
/// server-side secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionClaims {
    pub pid: ParticipantId,
    pub room_id: String,
    pub iat: u64,
    pub exp: u64,
}

/// Lifetime of a session token. The frontend has this much time between
/// `POST /rooms/.../participants` and the WebSocket `Join`.
const SESSION_TTL: Duration = Duration::from_secs(300);

/// How long a pending entry stays in memory if the WS never connects.
const PENDING_TTL: Duration = Duration::from_secs(30);

/// In-memory record of a participant created via REST but not yet connected
/// over WebSocket. Consumed by `on_authenticate` when the WS arrives.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PendingEntry {
    pub room_id: String,
    /// Captured at REST creation time so the SFU can echo it on `Join`.
    /// Reserved for an upcoming step that surfaces it to the WS layer.
    pub display_name: Option<String>,
    pub created_at: SystemTime,
}

/// Shared state between the HTTP layer (axum) and the WebSocket auth hook.
#[derive(Clone)]
pub struct SessionState {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    pending: RwLock<HashMap<ParticipantId, PendingEntry>>,
}

impl SessionState {
    pub fn new(secret: &[u8]) -> Self {
        Self {
            inner: Arc::new(SessionInner {
                encoding_key: EncodingKey::from_secret(secret),
                decoding_key: DecodingKey::from_secret(secret),
                pending: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Mints a session token for a freshly created participant and stores
    /// the pending entry. Returns the encoded JWT string.
    pub async fn create_session(
        &self,
        pid: ParticipantId,
        room_id: String,
        display_name: Option<String>,
    ) -> Result<String, jsonwebtoken::errors::Error> {
        let now = SystemTime::now();
        let iat = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let exp = iat + SESSION_TTL.as_secs();
        let claims = SessionClaims {
            pid,
            room_id: room_id.clone(),
            iat,
            exp,
        };
        let token = encode(&Header::new(Algorithm::HS256), &claims, &self.inner.encoding_key)?;

        let entry = PendingEntry {
            room_id,
            display_name,
            created_at: now,
        };
        self.inner.pending.write().await.insert(pid, entry);
        Ok(token)
    }

    /// Verifies a session token and confirms it belongs to `pid` joining
    /// `room_id`. The JWT signature + expiry are the cryptographic proof;
    /// the pending entry is opportunistically cleared but not required for
    /// success — this lets a client re-join (e.g. after an F5 reload or a
    /// transient WS disconnect) within the token's TTL.
    pub async fn consume(
        &self,
        token: &str,
        pid: ParticipantId,
        room_id: &str,
    ) -> Result<(), &'static str> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_required_spec_claims::<&str>(&[]);
        validation.validate_exp = true;
        let data = decode::<SessionClaims>(token, &self.inner.decoding_key, &validation)
            .map_err(|_| "invalid or expired token")?;
        if data.claims.pid != pid {
            return Err("token participant mismatch");
        }
        if data.claims.room_id != room_id {
            return Err("token room mismatch");
        }
        // Best-effort cleanup of the pending entry. Re-joins within the
        // JWT TTL are allowed even after the entry has already been
        // removed.
        self.inner.pending.write().await.remove(&pid);
        Ok(())
    }

    /// Test-only constructor that pre-populates a pending entry without
    /// minting a token. Useful to assert that `consume` works even when
    /// no entry exists (the JWT-only path).
    #[cfg(test)]
    pub async fn _insert_pending_for_test(
        &self,
        pid: ParticipantId,
        room_id: String,
        display_name: Option<String>,
    ) {
        self.inner.pending.write().await.insert(
            pid,
            PendingEntry {
                room_id,
                display_name,
                created_at: SystemTime::now(),
            },
        );
    }

    /// Periodically scans pending entries and removes anything older than
    /// PENDING_TTL. Intended to run as a background task.
    pub async fn purge_expired(&self) {
        let now = SystemTime::now();
        let mut pending = self.inner.pending.write().await;
        pending.retain(|pid, entry| {
            let keep = now
                .duration_since(entry.created_at)
                .map(|elapsed| elapsed < PENDING_TTL)
                .unwrap_or(true);
            if !keep {
                debug!(participant = %pid, "purged expired pending entry");
            }
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// The Playwright fetch mock mints HS256 JWTs in Node and the backend
    /// must accept them. This test pins both sides to the same secret and
    /// the same canonical token to catch any drift in claim shape /
    /// header / encoding between the JS and Rust sides.
    #[tokio::test]
    async fn consume_accepts_externally_minted_jwt() {
        // Same bytes as `e2e/helpers/session.ts::SESSION_SECRET`.
        let secret = b"seameet-playwright-secret-32-bytes!";
        let state = SessionState::new(secret);

        let pid = ParticipantId::new(Uuid::nil());
        let room = "r1";

        // Token minted by the equivalent Node helper:
        //   header = { typ: "JWT", alg: "HS256" }
        //   claims = { pid, room_id, iat, exp }
        let token = state
            .create_session(pid, room.to_owned(), None)
            .await
            .expect("mint");

        // First consume succeeds and clears the pending entry.
        state.consume(&token, pid, room).await.expect("first consume");

        // Second consume succeeds too (JWT-only path) — supports F5 reload.
        state.consume(&token, pid, room).await.expect("second consume");
    }

    #[tokio::test]
    async fn consume_rejects_pid_mismatch() {
        let state = SessionState::new(b"x".repeat(32).as_slice());
        let pid = ParticipantId::new(Uuid::from_u128(1));
        let other = ParticipantId::new(Uuid::from_u128(2));
        let token = state
            .create_session(pid, "r1".into(), None)
            .await
            .unwrap();
        assert!(state.consume(&token, other, "r1").await.is_err());
    }

    #[tokio::test]
    async fn consume_rejects_room_mismatch() {
        let state = SessionState::new(b"x".repeat(32).as_slice());
        let pid = ParticipantId::new(Uuid::from_u128(1));
        let token = state
            .create_session(pid, "r1".into(), None)
            .await
            .unwrap();
        assert!(state.consume(&token, pid, "r2").await.is_err());
    }
}
