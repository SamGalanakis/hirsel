use std::time::UNIX_EPOCH;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use hirsel_proto::{Blob, ChatMessage, HostToClient, InboxItem, SendMode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AppState,
    attachments::{decode_blob_data_b64, normalize_mime, sanitize_blob_name},
};

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/debug/reset", post(reset))
        .route("/debug/upload", post(upload_blob))
        .route("/debug/owner-message", post(owner_message))
        .route("/debug/read-item", post(read_item))
        .route("/debug/cancel-turn", post(cancel_turn))
        .route("/debug/cancel-queued", post(cancel_queued))
        .route("/debug/broadcasts", get(broadcasts))
        .route("/debug/chat", get(chat))
        .route("/debug/inbox", get(inbox))
        .route("/debug/processes", get(processes))
        .route("/debug/health", get(health))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct OwnerMessageRequest {
    #[serde(default)]
    client_id: Option<String>,
    body: String,
    #[serde(rename = "ref")]
    anchor: Option<u64>,
    #[serde(default)]
    attachments: Vec<String>,
    #[serde(default)]
    mode: SendMode,
}

#[derive(Debug, Deserialize)]
struct CancelQueuedRequest {
    client_id: String,
}

#[derive(Debug, Deserialize)]
struct UploadBlobRequest {
    name: String,
    mime: String,
    data_b64: String,
}

#[derive(Debug, Deserialize)]
struct ReadItemRequest {
    item_id: u64,
}

#[derive(Debug, Serialize)]
struct OwnerMessageResponse {
    client_id: String,
    message: ChatMessage,
}

#[derive(Debug, Serialize)]
struct CancelQueuedResponse {
    ok: bool,
    id: u64,
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
struct BroadcastsResponse {
    events: Vec<HostToClient>,
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
    state.broadcast_log.clear();
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn owner_message(
    State(state): State<AppState>,
    Json(request): Json<OwnerMessageRequest>,
) -> Result<Json<OwnerMessageResponse>, DebugError> {
    let client_id = request
        .client_id
        .unwrap_or_else(|| format!("debug-{}", Uuid::new_v4()));
    let submission = state
        .submit_owner_message(
            client_id,
            request.body,
            request.anchor,
            request.attachments,
            request.mode,
        )
        .await?;
    Ok(Json(OwnerMessageResponse {
        client_id: submission.client_id,
        message: submission.message,
    }))
}

async fn cancel_turn(State(state): State<AppState>) -> Result<Json<serde_json::Value>, DebugError> {
    state.cancel_turn().await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn cancel_queued(
    State(state): State<AppState>,
    Json(request): Json<CancelQueuedRequest>,
) -> Result<Json<CancelQueuedResponse>, DebugError> {
    let id = state.cancel_queued_message(&request.client_id).await?;
    Ok(Json(CancelQueuedResponse { ok: true, id }))
}

async fn upload_blob(
    State(state): State<AppState>,
    Json(request): Json<UploadBlobRequest>,
) -> Result<Json<Blob>, DebugError> {
    let client_id = format!("debug-upload-{}", Uuid::new_v4());
    let data = decode_blob_data_b64(&request.data_b64)?;
    let stored = state
        .storage
        .store_blob(
            &client_id,
            sanitize_blob_name(&request.name),
            normalize_mime(&request.mime),
            data,
        )
        .await?;
    Ok(Json(stored.blob))
}

async fn read_item(
    State(state): State<AppState>,
    Json(request): Json<ReadItemRequest>,
) -> Result<Json<InboxItem>, DebugError> {
    let item = state
        .storage
        .mark_read(request.item_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown inbox item: {}", request.item_id))?;
    state.broadcast(HostToClient::InboxUpsert { item: item.clone() });
    Ok(Json(item))
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

async fn broadcasts(State(state): State<AppState>) -> Json<BroadcastsResponse> {
    Json(BroadcastsResponse {
        events: state.broadcast_log.recent(),
    })
}

async fn processes(State(state): State<AppState>) -> Result<Json<serde_json::Value>, DebugError> {
    Ok(Json(serde_json::json!({
        "processes": state.processes.snapshot()?
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
