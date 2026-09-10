//! Explicit artifact fixtures for isolated scripted-host browser checks.
use super::{AppState, DebugError, Json, State};
use crate::storage::ArtifactDraft;
use hirsel_proto::{Artifact, HostToClient};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct PublishArtifactRequest {
    operation_id: String,
    thread_id: u64,
    artifact_id: Option<u64>,
    draft: Option<ArtifactDraft>,
}

pub(super) async fn publish_artifact(
    State(state): State<AppState>,
    Json(request): Json<PublishArtifactRequest>,
) -> Result<Json<Artifact>, DebugError> {
    if !state.agent.is_scripted() {
        return Err(anyhow::anyhow!("artifact fixture requires the scripted Agent").into());
    }
    let input = serde_json::json!({"thread_id":request.thread_id,"artifact_id":request.artifact_id,"draft":request.draft});
    let (artifact, message) = state
        .storage
        .publish_artifact_human(
            &format!("debug-artifact:{}", request.operation_id),
            &input,
            request.thread_id,
            request.artifact_id,
            request.draft,
        )
        .await?;
    state.broadcast(HostToClient::ArtifactUpsert {
        artifact: artifact.summary.clone(),
    });
    if let Some(message) = message {
        state.tools.publish_thread_message(message).await;
    }
    Ok(Json(artifact))
}
