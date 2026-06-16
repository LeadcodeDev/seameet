use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use seameet_core::ParticipantId;
use serde::Deserialize;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::http::{AuthOutcome, AuthProvider};

/// Default UUID v5 namespace for mapping JWT `sub` → `ParticipantId`. Pinning
/// this means the same authenticated user always gets the same pid across
/// sessions, which is what makes reconnect identity coherent in front of a
/// real IdP. Override with `SEAMEET_PID_NAMESPACE` if you want to scope pids
/// per-deployment (e.g. so a user from staging cannot collide with prod).
pub const DEFAULT_PID_NAMESPACE: Uuid = Uuid::from_u128(0x73656d65_6574_5f70_6964_5f6e7331_3030);

#[derive(Debug, Clone)]
pub struct JwksConfig {
    pub issuer: String,
    pub audience: String,
    pub jwks_url: String,
    pub pid_namespace: Uuid,
}

#[derive(Debug, Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    r#use: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OidcClaims {
    sub: String,
}

/// Minimal JWKS-backed `AuthProvider`. Validates RS256 JWTs against keys
/// fetched from a JWKS URL, checks issuer and audience, and maps `sub` to a
/// stable `ParticipantId` via UUID v5.
pub struct JwksAuth {
    config: JwksConfig,
    keys: Arc<RwLock<HashMap<String, DecodingKey>>>,
    http: reqwest::Client,
}

impl JwksAuth {
    pub fn new(config: JwksConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            keys: Arc::new(RwLock::new(HashMap::new())),
            http,
        }
    }

    pub fn config(&self) -> &JwksConfig {
        &self.config
    }

    /// Fetch the JWKS document and atomically swap the key cache. Errors
    /// from the network or from a malformed document leave the existing
    /// cache untouched, so a transient outage doesn't lock everyone out.
    pub async fn refresh(&self) -> Result<usize, String> {
        let response = self
            .http
            .get(&self.config.jwks_url)
            .send()
            .await
            .map_err(|e| format!("jwks fetch failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("jwks fetch returned {}", response.status()));
        }
        let body = response
            .text()
            .await
            .map_err(|e| format!("jwks read body failed: {e}"))?;
        let next = parse_jwks(&body)?;
        let count = next.len();
        *self
            .keys
            .write()
            .map_err(|_| "key cache poisoned".to_owned())? = next;
        Ok(count)
    }

    /// Spawn a background task that refreshes the JWKS at a fixed interval.
    /// Failures are logged at WARN; the loop continues so the next tick can
    /// recover. The first refresh is awaited synchronously inside the task
    /// so we don't deny tokens while waiting for the first poll.
    pub fn spawn_refresh_loop(self: Arc<Self>, interval: Duration) -> JoinHandle<()> {
        tokio::spawn(async move {
            match self.refresh().await {
                Ok(n) => info!(keys = n, "JWKS initial fetch ok"),
                Err(e) => warn!(error = %e, "JWKS initial fetch failed; will retry"),
            }
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // first tick fires immediately, drain it
            loop {
                ticker.tick().await;
                match self.refresh().await {
                    Ok(n) => debug!(keys = n, "JWKS refreshed"),
                    Err(e) => warn!(error = %e, "JWKS refresh failed"),
                }
            }
        })
    }

    /// Test/integration hook: install a key into the cache without going
    /// through the JWKS HTTP path.
    #[cfg(test)]
    pub fn insert_key(&self, kid: String, key: DecodingKey) {
        if let Ok(mut keys) = self.keys.write() {
            keys.insert(kid, key);
        }
    }

    fn validate(&self, bearer: &str) -> Result<ParticipantId, String> {
        let header = decode_header(bearer).map_err(|e| format!("bad token header: {e}"))?;
        let Some(kid) = header.kid else {
            return Err("token missing kid".to_owned());
        };
        let alg = header.alg;
        if alg != Algorithm::RS256 {
            return Err(format!("unsupported alg {alg:?}"));
        }

        let keys = self
            .keys
            .read()
            .map_err(|_| "key cache poisoned".to_owned())?;
        let key = keys
            .get(&kid)
            .ok_or_else(|| format!("unknown kid {kid}"))?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_audience(&[&self.config.audience]);
        validation.validate_exp = true;

        let data = decode::<OidcClaims>(bearer, key, &validation)
            .map_err(|e| format!("token rejected: {e}"))?;
        let pid = map_sub_to_pid(self.config.pid_namespace, &data.claims.sub);
        Ok(pid)
    }
}

impl AuthProvider for JwksAuth {
    fn authorize(
        &self,
        bearer: Option<&str>,
        _room_id: &str,
        _display_name: Option<&str>,
    ) -> AuthOutcome {
        let Some(token) = bearer else {
            return AuthOutcome::Rejected("missing token".to_owned());
        };
        match self.validate(token) {
            Ok(pid) => AuthOutcome::Authenticated(pid),
            Err(reason) => AuthOutcome::Rejected(reason),
        }
    }
}

pub fn map_sub_to_pid(namespace: Uuid, sub: &str) -> ParticipantId {
    ParticipantId::new(Uuid::new_v5(&namespace, sub.as_bytes()))
}

fn parse_jwks(body: &str) -> Result<HashMap<String, DecodingKey>, String> {
    let set: JwkSet =
        serde_json::from_str(body).map_err(|e| format!("jwks json parse failed: {e}"))?;
    let mut out = HashMap::with_capacity(set.keys.len());
    for jwk in set.keys {
        if jwk.kty != "RSA" {
            continue;
        }
        if matches!(jwk.r#use.as_deref(), Some(u) if u != "sig") {
            continue;
        }
        if matches!(jwk.alg.as_deref(), Some(a) if a != "RS256") {
            continue;
        }
        let Some(kid) = jwk.kid else { continue };
        let (Some(n), Some(e)) = (jwk.n, jwk.e) else { continue };
        let key = DecodingKey::from_rsa_components(&n, &e)
            .map_err(|err| format!("jwk {kid} invalid rsa components: {err}"))?;
        out.insert(kid, key);
    }
    if out.is_empty() {
        return Err("jwks document had no usable RSA RS256 keys".to_owned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde::Serialize;
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEST_KID: &str = "test-key-1";
    const TEST_PRIV_PEM: &[u8] = include_bytes!("test_fixtures/jwks_test_priv.pem");
    const TEST_PUB_PEM: &[u8] = include_bytes!("test_fixtures/jwks_test_pub.pem");
    const TEST_N: &str = "vhIBPBbz6qoNX-uAOqL8Oommo0q2lCKz7snvAEdw3bboaMHON_PhY_7eFwEuNqvTm7SulaiR4rli-GcKQ3m5N5WimTULubDul3k15FVuxSJCQb7-_oUome3DI_ciT9j5qF8mW3WBKtQahjlV5WcfsCtvvxuBAPeHTDZ2GbA1TgpQCCrROIeZmVXDpsCcAFgWNBwrk_gcWb0vGMPDuiXaSQpD341k-5H8iZXBXCm7jqu6bgjhPbx7MgfurdfvFd1YQqGjsyp_7bYt4wKQnNXLEg0-Sk7CoofGzmomIGR10ltdh7gAZkFdQyVk0gvFV9-casVXpI3HHl7dylgJfhhCzw";
    const TEST_E: &str = "AQAB";

    #[derive(Serialize)]
    struct Claims<'a> {
        iss: &'a str,
        aud: &'a str,
        sub: &'a str,
        exp: u64,
        iat: u64,
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn mint(claims: Claims<'_>) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(TEST_KID.to_owned());
        let key = EncodingKey::from_rsa_pem(TEST_PRIV_PEM).expect("priv pem");
        encode(&header, &claims, &key).expect("encode")
    }

    fn config() -> JwksConfig {
        JwksConfig {
            issuer: "https://idp.example.com/".to_owned(),
            audience: "seameet".to_owned(),
            jwks_url: "https://idp.example.com/.well-known/jwks.json".to_owned(),
            pid_namespace: DEFAULT_PID_NAMESPACE,
        }
    }

    fn auth_with_pem_key() -> JwksAuth {
        let auth = JwksAuth::new(config());
        let key = DecodingKey::from_rsa_pem(TEST_PUB_PEM).expect("pub pem");
        auth.insert_key(TEST_KID.to_owned(), key);
        auth
    }

    #[tokio::test]
    async fn accepts_valid_rs256_token_and_maps_sub_to_pid() {
        let auth = auth_with_pem_key();
        let token = mint(Claims {
            iss: "https://idp.example.com/",
            aud: "seameet",
            sub: "user-42",
            exp: now() + 60,
            iat: now(),
        });

        let outcome = auth.authorize(Some(&token), "any-room", None);
        match outcome {
            AuthOutcome::Authenticated(pid) => {
                let expected = map_sub_to_pid(DEFAULT_PID_NAMESPACE, "user-42");
                assert_eq!(pid, expected);
            }
            other => panic!("expected Authenticated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_expired_token() {
        let auth = auth_with_pem_key();
        // jsonwebtoken applies a default 60s leeway on exp, so back-date
        // the expiry well past that window to make the rejection deterministic.
        let token = mint(Claims {
            iss: "https://idp.example.com/",
            aud: "seameet",
            sub: "user-42",
            exp: now() - 600,
            iat: now() - 1200,
        });
        assert!(matches!(
            auth.authorize(Some(&token), "any-room", None),
            AuthOutcome::Rejected(_)
        ));
    }

    #[tokio::test]
    async fn rejects_wrong_issuer() {
        let auth = auth_with_pem_key();
        let token = mint(Claims {
            iss: "https://attacker.example.com/",
            aud: "seameet",
            sub: "user-42",
            exp: now() + 60,
            iat: now(),
        });
        assert!(matches!(
            auth.authorize(Some(&token), "any-room", None),
            AuthOutcome::Rejected(_)
        ));
    }

    #[tokio::test]
    async fn rejects_wrong_audience() {
        let auth = auth_with_pem_key();
        let token = mint(Claims {
            iss: "https://idp.example.com/",
            aud: "other-app",
            sub: "user-42",
            exp: now() + 60,
            iat: now(),
        });
        assert!(matches!(
            auth.authorize(Some(&token), "any-room", None),
            AuthOutcome::Rejected(_)
        ));
    }

    #[tokio::test]
    async fn rejects_unknown_kid() {
        let auth = JwksAuth::new(config()); // no keys installed
        let token = mint(Claims {
            iss: "https://idp.example.com/",
            aud: "seameet",
            sub: "user-42",
            exp: now() + 60,
            iat: now(),
        });
        assert!(matches!(
            auth.authorize(Some(&token), "any-room", None),
            AuthOutcome::Rejected(_)
        ));
    }

    #[tokio::test]
    async fn rejects_missing_bearer() {
        let auth = auth_with_pem_key();
        assert!(matches!(
            auth.authorize(None, "any-room", None),
            AuthOutcome::Rejected(_)
        ));
    }

    #[tokio::test]
    async fn rejects_malformed_token() {
        let auth = auth_with_pem_key();
        assert!(matches!(
            auth.authorize(Some("not-a-jwt"), "any-room", None),
            AuthOutcome::Rejected(_)
        ));
    }

    #[test]
    fn map_sub_to_pid_is_deterministic() {
        let a = map_sub_to_pid(DEFAULT_PID_NAMESPACE, "alice");
        let b = map_sub_to_pid(DEFAULT_PID_NAMESPACE, "alice");
        let c = map_sub_to_pid(DEFAULT_PID_NAMESPACE, "bob");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn map_sub_to_pid_changes_with_namespace() {
        let ns1 = Uuid::from_u128(1);
        let ns2 = Uuid::from_u128(2);
        let a = map_sub_to_pid(ns1, "alice");
        let b = map_sub_to_pid(ns2, "alice");
        assert_ne!(a, b);
    }

    #[test]
    fn parse_jwks_extracts_rs256_signing_keys() {
        let body = format!(
            r#"{{"keys": [
                {{"kty":"RSA","kid":"{TEST_KID}","alg":"RS256","use":"sig","n":"{TEST_N}","e":"{TEST_E}"}},
                {{"kty":"oct","kid":"oct-skip","k":"AAAA"}},
                {{"kty":"RSA","kid":"enc-skip","alg":"RS256","use":"enc","n":"{TEST_N}","e":"{TEST_E}"}}
            ]}}"#
        );
        let parsed = parse_jwks(&body).expect("parse ok");
        assert_eq!(parsed.len(), 1);
        assert!(parsed.contains_key(TEST_KID));
    }

    #[test]
    fn parse_jwks_rejects_empty_keyset() {
        let body = r#"{"keys": []}"#;
        assert!(parse_jwks(body).is_err());
    }

    #[tokio::test]
    async fn jwks_document_round_trip_lets_real_token_validate() {
        let auth = JwksAuth::new(config());
        let body = format!(
            r#"{{"keys": [
                {{"kty":"RSA","kid":"{TEST_KID}","alg":"RS256","use":"sig","n":"{TEST_N}","e":"{TEST_E}"}}
            ]}}"#
        );
        let parsed = parse_jwks(&body).expect("parse ok");
        *auth.keys.write().unwrap() = parsed;

        let token = mint(Claims {
            iss: "https://idp.example.com/",
            aud: "seameet",
            sub: "user-42",
            exp: now() + 60,
            iat: now(),
        });
        assert!(matches!(
            auth.authorize(Some(&token), "any-room", None),
            AuthOutcome::Authenticated(_)
        ));
    }
}
