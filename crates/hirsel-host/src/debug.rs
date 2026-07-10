use std::time::{Duration, UNIX_EPOCH};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use hirsel_proto::{Blob, ChatMessage, HostToClient, Ping, PushPlatform, SendMode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AppState,
    attachments::{decode_blob_data_b64, normalize_mime, sanitize_blob_name},
    push::RecordedPush,
    storage::{Device, MonitorRecord, MonitorWakeOn, PushToken},
};

const PAIRING_CODE_TTL: Duration = Duration::from_secs(5 * 60);

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/debug/reset", post(reset))
        .route("/debug/upload", post(upload_blob))
        .route("/debug/owner-message", post(owner_message))
        .route("/debug/open-side-chat", post(open_side_chat))
        .route("/debug/side-message", post(side_message))
        .route("/debug/conclude", post(conclude))
        .route("/debug/confirm-conclusion", post(confirm_conclusion))
        .route("/debug/side-chats", get(side_chats))
        .route("/debug/read-ping", post(read_ping))
        .route("/debug/resolve-ping", post(resolve_ping))
        .route("/debug/register-push-token", post(register_push_token))
        .route("/debug/unregister-push-token", post(unregister_push_token))
        .route("/debug/pushes", get(recorded_pushes))
        .route("/debug/cancel-turn", post(cancel_turn))
        .route("/debug/cancel-queued", post(cancel_queued))
        .route("/debug/create-monitor", post(create_monitor))
        .route("/debug/broadcasts", get(broadcasts))
        .route("/debug/chat", get(chat))
        .route("/debug/pings", get(pings))
        .route("/debug/processes", get(processes))
        .route("/debug/pair", post(pair))
        .route("/debug/devices", get(devices))
        .route("/debug/revoke-device", post(revoke_device))
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
    mentions: Vec<u64>,
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
struct PingRequest {
    ping_id: u64,
}

#[derive(Debug, Deserialize)]
struct RegisterPushTokenRequest {
    platform: PushPlatform,
    token: String,
}

#[derive(Debug, Deserialize)]
struct UnregisterPushTokenRequest {
    token: String,
}

#[derive(Debug, Deserialize)]
struct OpenSideChatRequest {
    ping_id: u64,
}

#[derive(Debug, Deserialize)]
struct SideMessageRequest {
    sc: String,
    body: String,
    #[serde(default)]
    mentions: Vec<u64>,
}

#[derive(Debug, Deserialize)]
struct ConcludeRequest {
    sc: String,
}

#[derive(Debug, Deserialize)]
struct ConfirmConclusionRequest {
    sc: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct CreateMonitorRequest {
    cmd: String,
    #[serde(default)]
    every_secs: Option<u64>,
    wake_on: MonitorWakeOn,
    #[serde(default)]
    pattern: Option<String>,
    label: String,
}

#[derive(Debug, Deserialize)]
struct PairRequest {
    #[serde(alias = "label")]
    device_label: String,
}

#[derive(Debug, Deserialize)]
struct RevokeDeviceRequest {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    token_or_label: Option<String>,
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
struct PingsResponse {
    pings: Vec<Ping>,
}

#[derive(Debug, Serialize)]
struct RecordedPushesResponse {
    pushes: Vec<RecordedPush>,
}

#[derive(Debug, Serialize)]
struct CreateMonitorResponse {
    monitor: MonitorRecord,
}

#[derive(Debug, Serialize)]
struct OpenSideChatResponse {
    sc: String,
    ping_id: u64,
    messages: Vec<ChatMessage>,
    resumed: bool,
}

#[derive(Debug, Serialize)]
struct ConclusionResponse {
    sc: String,
    text: String,
}

#[derive(Debug, Serialize)]
struct SideChatsResponse {
    side_chats: Vec<crate::side_chat::SideChatView>,
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

#[derive(Debug, Serialize)]
struct PairResponse {
    code: String,
    ticket: String,
}

#[derive(Debug, Serialize)]
struct DevicesResponse {
    devices: Vec<DeviceResponse>,
}

#[derive(Debug, Serialize)]
struct DeviceResponse {
    label: String,
    node_id_prefix: String,
    created: String,
    last_seen: String,
    revoked: Option<String>,
}

impl From<Device> for DeviceResponse {
    fn from(device: Device) -> Self {
        Self {
            label: device.device_label,
            node_id_prefix: device.node_id.chars().take(12).collect(),
            created: device.created_ts.to_rfc3339(),
            last_seen: device.last_seen_ts.to_rfc3339(),
            revoked: device.revoked_ts.map(|timestamp| timestamp.to_rfc3339()),
        }
    }
}

async fn reset(State(state): State<AppState>) -> Result<Json<serde_json::Value>, DebugError> {
    state.side_chats.discard_all().await;
    state.storage.reset().await?;
    state.processes.reset()?;
    state.broadcast_log.clear();
    state.pushes.clear_recorded_pushes();
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn open_side_chat(
    State(state): State<AppState>,
    Json(request): Json<OpenSideChatRequest>,
) -> Result<Json<OpenSideChatResponse>, DebugError> {
    let (sc, messages, resumed) = state.side_chats.open(request.ping_id).await?;
    state.broadcast(HostToClient::SideChatOpen {
        sc: sc.clone(),
        ping_id: request.ping_id,
        messages: messages.clone(),
    });
    Ok(Json(OpenSideChatResponse {
        sc,
        ping_id: request.ping_id,
        messages,
        resumed,
    }))
}

async fn side_message(
    State(state): State<AppState>,
    Json(request): Json<SideMessageRequest>,
) -> Result<Json<serde_json::Value>, DebugError> {
    state
        .side_chats
        .send(&request.sc, request.body, request.mentions)
        .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn conclude(
    State(state): State<AppState>,
    Json(request): Json<ConcludeRequest>,
) -> Result<Json<ConclusionResponse>, DebugError> {
    let text = state.side_chats.conclude(&request.sc).await?;
    Ok(Json(ConclusionResponse {
        sc: request.sc,
        text,
    }))
}

async fn confirm_conclusion(
    State(state): State<AppState>,
    Json(request): Json<ConfirmConclusionRequest>,
) -> Result<Json<serde_json::Value>, DebugError> {
    state
        .side_chats
        .confirm(&request.sc, request.text, &state)
        .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn side_chats(State(state): State<AppState>) -> Result<Json<SideChatsResponse>, DebugError> {
    Ok(Json(SideChatsResponse {
        side_chats: state.side_chats.views().await?,
    }))
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
            request.mentions,
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

async fn read_ping(
    State(state): State<AppState>,
    Json(request): Json<PingRequest>,
) -> Result<Json<Ping>, DebugError> {
    let ping = state
        .storage
        .mark_ping_read(request.ping_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown ping: {}", request.ping_id))?;
    state.broadcast(HostToClient::PingUpsert { ping: ping.clone() });
    Ok(Json(ping))
}

async fn resolve_ping(
    State(state): State<AppState>,
    Json(request): Json<PingRequest>,
) -> Result<Json<Ping>, DebugError> {
    let ping = state
        .storage
        .resolve_ping(request.ping_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown ping: {}", request.ping_id))?;
    state.broadcast(HostToClient::PingUpsert { ping: ping.clone() });
    Ok(Json(ping))
}

async fn register_push_token(
    State(state): State<AppState>,
    Json(request): Json<RegisterPushTokenRequest>,
) -> Result<Json<PushToken>, DebugError> {
    Ok(Json(
        state
            .storage
            .register_push_token(request.platform, request.token)
            .await?,
    ))
}

async fn unregister_push_token(
    State(state): State<AppState>,
    Json(request): Json<UnregisterPushTokenRequest>,
) -> Result<Json<serde_json::Value>, DebugError> {
    let removed = state.storage.unregister_push_token(&request.token).await?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

async fn recorded_pushes(State(state): State<AppState>) -> Json<RecordedPushesResponse> {
    Json(RecordedPushesResponse {
        pushes: state.pushes.recorded_pushes(),
    })
}

async fn create_monitor(
    State(state): State<AppState>,
    Json(request): Json<CreateMonitorRequest>,
) -> Result<Json<CreateMonitorResponse>, DebugError> {
    let monitor = state
        .create_monitor(
            request.cmd,
            request.every_secs.unwrap_or(30),
            request.wake_on,
            request.pattern,
            request.label,
        )
        .await?;
    Ok(Json(CreateMonitorResponse { monitor }))
}

async fn chat(State(state): State<AppState>) -> Result<Json<ChatResponse>, DebugError> {
    Ok(Json(ChatResponse {
        messages: state.storage.all_chat().await?,
    }))
}

async fn pings(State(state): State<AppState>) -> Result<Json<PingsResponse>, DebugError> {
    Ok(Json(PingsResponse {
        pings: state.storage.all_pings().await?,
    }))
}

async fn broadcasts(State(state): State<AppState>) -> Json<BroadcastsResponse> {
    Json(BroadcastsResponse {
        events: state.broadcast_log.recent(),
    })
}

async fn processes(State(state): State<AppState>) -> Result<Json<serde_json::Value>, DebugError> {
    Ok(Json(serde_json::json!({
        "processes": state.process_snapshot().await?
    })))
}

async fn pair(
    State(state): State<AppState>,
    Json(request): Json<PairRequest>,
) -> Result<Json<PairResponse>, DebugError> {
    let ticket = state
        .iroh_ticket()
        .ok_or_else(|| anyhow::anyhow!("iroh endpoint is not available"))?;
    let code = state
        .storage
        .mint_pairing_code(request.device_label, PAIRING_CODE_TTL)
        .await?;
    Ok(Json(PairResponse { code, ticket }))
}

async fn devices(State(state): State<AppState>) -> Result<Json<DevicesResponse>, DebugError> {
    let devices = state
        .storage
        .list_devices()
        .await?
        .into_iter()
        .map(DeviceResponse::from)
        .collect();
    Ok(Json(DevicesResponse { devices }))
}

async fn revoke_device(
    State(state): State<AppState>,
    Json(request): Json<RevokeDeviceRequest>,
) -> Result<Json<serde_json::Value>, DebugError> {
    let token_or_label = match (request.token, request.label, request.token_or_label) {
        (Some(token), None, None) => token,
        (None, Some(label), None) => label,
        (None, None, Some(token_or_label)) => token_or_label,
        _ => {
            return Err(anyhow::anyhow!("provide exactly one device token or label").into());
        }
    };
    let revoked = state.storage.revoke_device(&token_or_label).await?;
    Ok(Json(serde_json::json!({ "revoked": revoked })))
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
