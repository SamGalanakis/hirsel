use std::time::UNIX_EPOCH;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use hirsel_proto::{ChatMessage, HostToClient, InboxItem};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AppState, lash_runtime::OwnerTurn};

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/debug/reset", post(reset))
        .route("/debug/owner-message", post(owner_message))
        .route("/debug/chat", get(chat))
        .route("/debug/inbox", get(inbox))
        .route("/debug/processes", get(processes))
        .route("/debug/health", get(health))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct OwnerMessageRequest {
    body: String,
    #[serde(rename = "ref")]
    anchor: Option<u64>,
}

#[derive(Debug, Serialize)]
struct OwnerMessageResponse {
    message: ChatMessage,
}

#[derive(Debug, Serialize)]
struct ChatResponse {
    messages: Vec<ChatMessage>,
}

#[derive(Debug, Serialize)]
struct InboxResponse {
    items: Vec<InboxItem>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    latest_msg_id: u64,
    debug: bool,
    started_at_unix: u64,
}

async fn reset(State(state): State<AppState>) -> Result<Json<serde_json::Value>, DebugError> {
    state.storage.reset().await?;
    state.processes.reset()?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn owner_message(
    State(state): State<AppState>,
    Json(request): Json<OwnerMessageRequest>,
) -> Result<Json<OwnerMessageResponse>, DebugError> {
    let client_id = format!("debug-{}", Uuid::new_v4());
    let (message, inserted) = state
        .storage
        .append_owner_message(&client_id, request.body, request.anchor)
        .await?;
    if inserted {
        let _ = state.broadcaster.send(HostToClient::Msg {
            message: message.clone(),
        });
        state
            .agent
            .enqueue(OwnerTurn {
                message_id: message.id,
                client_id,
                body: message.body.clone(),
                anchor: message.r#ref,
            })
            .await?;
    }
    Ok(Json(OwnerMessageResponse { message }))
}

async fn chat(State(state): State<AppState>) -> Result<Json<ChatResponse>, DebugError> {
    Ok(Json(ChatResponse {
        messages: state.storage.all_chat().await?,
    }))
}

async fn inbox(State(state): State<AppState>) -> Result<Json<InboxResponse>, DebugError> {
    Ok(Json(InboxResponse {
        items: state.storage.all_inbox().await?,
    }))
}

async fn processes(State(state): State<AppState>) -> Result<Json<serde_json::Value>, DebugError> {
    Ok(Json(serde_json::json!({
        "processes": state.processes.list()?
    })))
}

async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, DebugError> {
    let started_at_unix = state
        .started_at
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    Ok(Json(HealthResponse {
        ok: true,
        latest_msg_id: state.storage.latest_msg_id().await?,
        debug: state.debug_enabled,
        started_at_unix,
    }))
}

struct DebugError(anyhow::Error);

impl<E> From<E> for DebugError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for DebugError {
    fn into_response(self) -> axum::response::Response {
        tracing::warn!(error = %self.0, "debug endpoint failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": self.0.to_string() })),
        )
            .into_response()
    }
}
