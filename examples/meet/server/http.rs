use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use seameet_core::ParticipantId;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use uuid::Uuid;

use crate::session::SessionState;

const MAX_ROOM_ID_LEN: usize = 64;
const MAX_DISPLAY_NAME_LEN: usize = 64;

/// Authentication contract for the HTTP layer. Integrators implement this
/// to plug their own remote auth provider (e.g. JWT validation against a
/// service like Ferriskey/Keycloak/Auth0). The example ships a permissive
/// default plus a shared-secret implementation toggled by env var.
pub trait AuthProvider: Send + Sync + 'static {
    fn authorize(
        &self,
        bearer: Option<&str>,
        room_id: &str,
        display_name: Option<&str>,
    ) -> AuthOutcome;
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum AuthOutcome {
    /// Stable identity provided by the integrator (e.g. derived from JWT sub).
    /// Reserved for integrators wiring a real IdP — not used by the
    /// `OpenAuth` / `SharedSecretAuth` providers shipped with this example.
    Authenticated(ParticipantId),
    /// Anonymous join accepted; server generates a fresh ParticipantId.
    Anonymous,
    /// Refused. The reason is returned to the caller as `401 Unauthorized`.
    Rejected(String),
}

#[derive(Clone)]
pub struct AppState {
    pub sessions: SessionState,
    pub auth: Arc<dyn AuthProvider>,
}

#[derive(Debug, Deserialize)]
pub struct CreateParticipantBody {
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateParticipantResponse {
    pub room: RoomDto,
    pub participant: ParticipantDto,
    pub session: SessionDto,
}

#[derive(Debug, Serialize)]
pub struct RoomDto {
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct ParticipantDto {
    pub id: ParticipantId,
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionDto {
    pub token: String,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/rooms/:room_id/participants", post(create_participant))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn create_participant(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CreateParticipantBody>,
) -> Response {
    if room_id.is_empty() || room_id.len() > MAX_ROOM_ID_LEN
        || room_id.chars().any(|c| c.is_control())
    {
        return error(StatusCode::BAD_REQUEST, "invalid_room_id", "invalid room id");
    }

    let display_name = body.display_name.as_deref().map(|name| {
        name.chars()
            .filter(|c| !c.is_control())
            .take(MAX_DISPLAY_NAME_LEN)
            .collect::<String>()
    });

    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(str::trim))
        .filter(|s| !s.is_empty());

    let outcome = state
        .auth
        .authorize(bearer, &room_id, display_name.as_deref());
    let pid = match outcome {
        AuthOutcome::Authenticated(pid) => pid,
        AuthOutcome::Anonymous => ParticipantId::new(Uuid::new_v4()),
        AuthOutcome::Rejected(reason) => {
            warn!(room = %room_id, reason = %reason, "auth rejected");
            return error(StatusCode::UNAUTHORIZED, "unauthorized", &reason);
        }
    };

    let token = match state
        .sessions
        .create_session(pid, room_id.clone(), display_name.clone())
        .await
    {
        Ok(t) => t,
        Err(e) => {
            warn!(error = %e, "failed to mint session token");
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "session_error",
                "could not mint session",
            );
        }
    };

    info!(participant = %pid, room = %room_id, "participant created");

    let body = CreateParticipantResponse {
        room: RoomDto { id: room_id },
        participant: ParticipantDto {
            id: pid,
            display_name,
        },
        session: SessionDto { token },
    };
    (StatusCode::CREATED, Json(body)).into_response()
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": code, "message": message })),
    )
        .into_response()
}
