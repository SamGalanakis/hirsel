mod artifacts;

use std::time::{Duration, UNIX_EPOCH};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header::WWW_AUTHENTICATE},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
};
use hirsel_proto::{
    Blob, ChatMessage, HostToClient, ModelSelection, PushPlatform, SendMode, Thread,
    ThreadAttention, ViewInstance,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AppState,
    attachments::{decode_blob_data_b64, normalize_mime, sanitize_blob_name},
    auth::owner_bearer_matches,
    push::RecordedPush,
    storage::{Device, MonitorCondition, MonitorRecord, PushToken},
};

const PAIRING_CODE_TTL: Duration = Duration::from_secs(5 * 60);

pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/debug/reset", post(reset))
        .route("/debug/upload", post(upload_blob))
        .route("/debug/owner-message", post(owner_message))
        .route("/debug/seed-adaptive-thread", post(seed_adaptive_thread))
        .route("/debug/publish-artifact", post(artifacts::publish_artifact))
        .route("/debug/trigger-digest", post(trigger_digest))
        .route("/debug/fork-wake", post(fork_wake))
        .route("/debug/register-push-token", post(register_push_token))
        .route("/debug/unregister-push-token", post(unregister_push_token))
        .route("/debug/pushes", get(recorded_pushes))
        .route("/debug/cancel-turn", post(cancel_turn))
        .route("/debug/cancel-queued", post(cancel_queued))
        .route("/debug/create-monitor", post(create_monitor))
        .route("/debug/set-model", post(set_model))
        .route(
            "/debug/subagent-models",
            get(subagent_models).post(set_subagent_model),
        )
        .route("/debug/show-view", post(show_view))
        .route("/debug/views", get(views))
        .route("/debug/view-event", post(view_event))
        .route("/debug/broadcasts", get(broadcasts))
        .route("/debug/chat", get(chat))
        .route("/debug/processes", get(processes))
        .route("/debug/pair", post(pair))
        .route("/debug/devices", get(devices))
        .route("/debug/revoke-device", post(revoke_device))
        .route("/debug/health", get(health))
        .layer(middleware::from_fn_with_state(state.clone(), require_owner))
        .with_state(state)
}

async fn require_owner(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    if owner_bearer_matches(request.headers(), &state.token, state.debug_enabled) {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(WWW_AUTHENTICATE, "Bearer")],
            "owner authentication required",
        )
            .into_response()
    }
}

#[derive(Debug, Deserialize)]
struct OwnerMessageRequest {
    artifact_ids: Vec<u64>,
    #[serde(default)]
    client_id: Option<String>,
    body: String,
    thread_id: u64,
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
struct TriggerDigestRequest {
    thread_id: u64,
    #[serde(default = "default_digest_job_id")]
    job_id: String,
    #[serde(default = "default_digest_text")]
    text: String,
    #[serde(default = "default_digest_status")]
    status: String,
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
struct CreateMonitorRequest {
    thread_id: u64,
    cmd: String,
    #[serde(default)]
    every_secs: Option<u64>,
    #[serde(flatten)]
    condition: MonitorCondition,
    label: String,
}

#[cfg(test)]
mod monitor_request_tests {
    use super::*;

    #[test]
    fn debug_monitor_request_deserializes_only_valid_conditions() {
        let valid: CreateMonitorRequest = serde_json::from_value(serde_json::json!({
            "thread_id": 1,
            "cmd": "printf ready",
            "wake_on": "regex",
            "pattern": "ready",
            "label": "ready"
        }))
        .unwrap();
        assert_eq!(valid.condition.pattern(), Some("ready"));

        for invalid in [
            serde_json::json!({
                "thread_id": 1,
                "cmd": "printf ready",
                "wake_on": "regex",
                "pattern": "[",
                "label": "ready"
            }),
            serde_json::json!({
                "thread_id": 1,
                "cmd": "printf ready",
                "wake_on": "changed",
                "pattern": "ignored",
                "label": "ready"
            }),
        ] {
            assert!(serde_json::from_value::<CreateMonitorRequest>(invalid).is_err());
        }
    }
}

#[derive(Debug, Deserialize)]
struct SetModelRequest {
    provider_id: String,
    model_id: String,
    variant: String,
}

#[derive(Debug, Deserialize)]
struct SetSubagentModelRequest {
    provider: String,
    model_id: String,
    enabled: bool,
    enabled_variants: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowViewRequest {
    thread_id: u64,
    #[serde(default)]
    template_id: Option<String>,
    #[serde(default)]
    spec: Option<serde_json::Value>,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ViewEventRequest {
    instance_id: String,
    action: String,
    #[serde(default)]
    data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct PairRequest {
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
struct RecordedPushesResponse {
    pushes: Vec<RecordedPush>,
}

#[derive(Debug, Serialize)]
struct CreateMonitorResponse {
    monitor: MonitorRecord,
}

#[derive(Debug, Serialize)]
struct BroadcastsResponse {
    events: Vec<HostToClient>,
}

#[derive(Debug, Serialize)]
struct ViewsResponse {
    views: Vec<ViewInstance>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    latest_msg_id: u64,
    debug: bool,
    started_at_unix: u64,
    model: Option<ModelSelection>,
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
    state.agent.reset_history().await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn seed_adaptive_thread(State(state): State<AppState>) -> Result<Json<Thread>, DebugError> {
    if !state.agent.is_scripted() {
        return Err(anyhow::anyhow!("adaptive fixture requires the scripted Agent").into());
    }
    let instrument = serde_json::json!({
        "type": "card",
        "children": [
            { "type": "heading", "text": "A Thread that changes with the work", "level": 2 },
            { "type": "text", "text": "Continue invokes the orchestrator and updates this Thread's instrument." },
            { "type": "field", "name": "confirmation", "kind": "text", "label": "Confirmation", "placeholder": "Type ready", "required": true },
            { "type": "submit", "action": "advance", "label": "Continue", "settles": false }
        ]
    });
    let (thread, _) = state
        .storage
        .create_thread(
            &format!("debug-adaptive-thread:{}", Uuid::new_v4()),
            "Adaptive host proof",
            "Advance this Thread through the real Host action contract",
            &instrument,
            ThreadAttention::NeedsOwner,
            None,
        )
        .await?;
    state.broadcast(HostToClient::ThreadUpsert {
        thread: thread.clone(),
    });
    Ok(Json(thread))
}

async fn show_view(
    State(state): State<AppState>,
    Json(request): Json<ShowViewRequest>,
) -> Result<Json<ViewInstance>, DebugError> {
    Ok(Json(
        state
            .views
            .show(
                &state.storage.history_id().await?,
                request.thread_id,
                request.template_id,
                request.spec,
                request.params,
                None,
            )
            .await?,
    ))
}

async fn views(State(state): State<AppState>) -> Json<ViewsResponse> {
    Json(ViewsResponse {
        views: state.views.snapshot().await,
    })
}

async fn view_event(
    State(state): State<AppState>,
    Json(request): Json<ViewEventRequest>,
) -> Result<Json<serde_json::Value>, DebugError> {
    let submission = state
        .handle_view_event(request.instance_id, request.action, request.data)
        .await?;
    Ok(Json(serde_json::json!({
        "client_id": submission.client_id,
        "message": submission.message,
    })))
}

async fn owner_message(
    State(state): State<AppState>,
    Json(request): Json<OwnerMessageRequest>,
) -> Result<Json<OwnerMessageResponse>, DebugError> {
    let client_id = request
        .client_id
        .unwrap_or_else(|| format!("debug-{}", Uuid::new_v4()));
    let history_id = state.storage.history_id().await?;
    let submission = state
        .submit_addressed_thread_message(
            &history_id,
            client_id,
            request.thread_id,
            request.body,
            request.attachments,
            request.mentions,
            request.mode,
            request.artifact_ids,
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

async fn trigger_digest(
    State(state): State<AppState>,
    Json(request): Json<TriggerDigestRequest>,
) -> Result<Json<hirsel_proto::ThreadActivity>, DebugError> {
    Ok(Json(
        state
            .tools
            .emit_scheduled_digest(
                &state.storage.history_id().await?,
                request.thread_id,
                request.job_id,
                request.text,
                request.status,
            )
            .await?,
    ))
}

#[derive(Deserialize)]
struct ForkWakeRequest {
    thread_id: u64,
    /// The non-owner message to triage, verbatim.
    text: String,
    /// Where it came from, for the pack's attribution line.
    #[serde(default = "default_fork_origin")]
    origin: String,
}

fn default_fork_origin() -> String {
    "debug".to_string()
}

/// Inject one synthetic non-owner message into the ADR-0015 dispatch.
///
/// This is the smoke lever for fork triage: it takes exactly the path a
/// Sub-agent completion or a monitor firing takes, so a real fork runs against
/// the live `[fork]` model and its exit is observable in the log and on the
/// event/queue surfaces.
async fn fork_wake(
    State(state): State<AppState>,
    Json(request): Json<ForkWakeRequest>,
) -> Result<Json<serde_json::Value>, DebugError> {
    let message = crate::fork_wake::WakeMessage::new(
        request.thread_id,
        crate::fork_wake::WakeSource::External {
            origin: request.origin,
        },
        request.text,
        format!("debug:{}", Uuid::new_v4()),
    );
    let dispatched = state.agent.dispatch_fork_wake(message).await?;
    Ok(Json(serde_json::json!({ "dispatched": dispatched })))
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
            request.thread_id,
            request.cmd,
            request.every_secs.unwrap_or(30),
            request.condition,
            request.label,
        )
        .await?;
    Ok(Json(CreateMonitorResponse { monitor }))
}

async fn set_model(
    State(state): State<AppState>,
    Json(request): Json<SetModelRequest>,
) -> Result<Json<ModelSelection>, DebugError> {
    Ok(Json(
        state
            .set_agent_model(&request.provider_id, &request.model_id, &request.variant)
            .await?,
    ))
}

async fn subagent_models(
    State(state): State<AppState>,
) -> Json<hirsel_proto::SubagentModelCatalog> {
    Json(state.subagent_model_snapshot())
}

async fn set_subagent_model(
    State(state): State<AppState>,
    Json(request): Json<SetSubagentModelRequest>,
) -> Result<Json<hirsel_proto::SubagentModelCatalog>, DebugError> {
    Ok(Json(
        state
            .set_subagent_model(
                &request.provider,
                &request.model_id,
                request.enabled,
                &request.enabled_variants,
            )
            .await?,
    ))
}

async fn chat(State(state): State<AppState>) -> Result<Json<ChatResponse>, DebugError> {
    Ok(Json(ChatResponse {
        messages: state.storage.all_chat().await?,
    }))
}

fn default_digest_job_id() -> String {
    "morning-digest".to_string()
}

fn default_digest_text() -> String {
    "The scheduled fleet digest completed without blockers.".to_string()
}

fn default_digest_status() -> String {
    "fleet digest ready".to_string()
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
        model: state.model_snapshot().map(|snapshot| snapshot.current),
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
