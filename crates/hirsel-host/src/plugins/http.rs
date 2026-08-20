//! The plugin management API, and the mount points for per-plugin routers.
//!
//! Everything here lives under `/api/plugins` behind the same owner-token gate
//! as the rest of the host API. Each plugin gets exactly one nest,
//! `/api/plugins/<id>`, carrying its own management endpoints plus whatever
//! router the plugin returned — so `/enabled` and `/settings` are reserved
//! path names for plugin authors.

use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
};
use hirsel_plugin_api::SettingKind;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{PluginHost, PluginStatus};
use crate::{AppState, auth::owner_bearer_matches};

/// The value a client sees in place of a stored secret, and the value it may
/// send back to mean "leave this one alone".
const SECRET_MASK: &str = "<set>";

#[derive(Clone)]
struct ManageState {
    app: AppState,
    plugin_id: String,
}

#[derive(Clone)]
struct GateState {
    host: PluginHost,
    plugin_id: String,
}

/// Build `/api/plugins` and every per-plugin nest.
pub(crate) fn routes(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/api/plugins", get(list))
        .with_state(state.clone());

    for loaded in &state.plugins.inner.plugins {
        let manage = ManageState {
            app: state.clone(),
            plugin_id: loaded.id.clone(),
        };
        let mut nested = Router::new()
            .route("/enabled", post(set_enabled))
            .route("/settings", post(set_settings))
            .with_state(manage);
        if let Some(plugin_routes) = loaded.plugin.routes() {
            // The plugin's own routes are reachable only while it is running;
            // the management endpoints above stay reachable so a disabled or
            // errored plugin can be turned back on.
            nested = nested.merge(plugin_routes.with_state(loaded.ctx.clone()).layer(
                middleware::from_fn_with_state(
                    GateState {
                        host: state.plugins.clone(),
                        plugin_id: loaded.id.clone(),
                    },
                    require_running,
                ),
            ));
        }
        router = router.nest(&format!("/api/plugins/{}", loaded.id), nested);
    }

    router.layer(middleware::from_fn_with_state(state, require_owner))
}

async fn require_owner(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    if owner_bearer_matches(request.headers(), &state.token, state.debug_enabled) {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}

async fn require_running(
    State(gate): State<GateState>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    if gate.host.is_running(&gate.plugin_id).await {
        next.run(request).await
    } else {
        (
            StatusCode::NOT_FOUND,
            format!("plugin `{}` is not enabled", gate.plugin_id),
        )
            .into_response()
    }
}

async fn list(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let host = &state.plugins;
    let mut plugins = Vec::new();
    for loaded in &host.inner.plugins {
        let status = host.status(&loaded.id).await;
        let values = loaded.settings();
        let mut entry = json!({
            "id": loaded.id,
            "label": loaded.label,
            "version": loaded.version,
            "state": status.as_str(),
            "settings": loaded.descriptors,
            "values": masked_values(&loaded.descriptors, &values),
        });
        if let PluginStatus::Errored { detail } = status {
            entry["error"] = Value::String(detail);
        }
        plugins.push(entry);
    }
    Ok(Json(json!({ "plugins": plugins })))
}

#[derive(Debug, Deserialize)]
struct EnabledRequest {
    enabled: bool,
}

async fn set_enabled(
    State(manage): State<ManageState>,
    Json(request): Json<EnabledRequest>,
) -> Result<Json<Value>, ApiError> {
    let host = &manage.app.plugins;
    let changed = host.set_enabled(&manage.plugin_id, request.enabled).await?;
    if changed {
        // Toggling a plugin changes the agent's tool surface, so the catalog
        // has to be refreshed through the same seam a Sub-agent model change
        // uses. Fingerprint rotation on toggle is accepted (see the ADR).
        manage
            .app
            .agent
            .refresh_plugin_tools(&host.tool_names())
            .await?;
    }
    let status = host.status(&manage.plugin_id).await;
    Ok(Json(json!({
        "id": manage.plugin_id,
        "state": status.as_str(),
    })))
}

#[derive(Debug, Deserialize)]
struct SettingsRequest {
    #[serde(default)]
    values: Map<String, Value>,
}

async fn set_settings(
    State(manage): State<ManageState>,
    Json(request): Json<SettingsRequest>,
) -> Result<Json<Value>, ApiError> {
    let host = &manage.app.plugins;
    let loaded = host
        .find(&manage.plugin_id)
        .ok_or_else(|| ApiError::not_found(&manage.plugin_id))?
        .clone();
    let mut merge = Map::new();
    for (key, value) in request.values {
        let descriptor = loaded
            .descriptors
            .iter()
            .find(|descriptor| descriptor.key == key)
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "plugin `{}` has no setting `{key}`",
                    manage.plugin_id
                ))
            })?;
        match descriptor.kind {
            // The mask is what a client read back; sending it means "leave the
            // stored secret alone", so it never reaches storage.
            SettingKind::Secret if value.as_str() == Some(SECRET_MASK) => continue,
            SettingKind::Secret | SettingKind::String => {
                if !value.is_string() {
                    return Err(ApiError::bad_request(format!(
                        "setting `{key}` expects a string"
                    )));
                }
            }
            SettingKind::Boolean => {
                if !value.is_boolean() {
                    return Err(ApiError::bad_request(format!(
                        "setting `{key}` expects a boolean"
                    )));
                }
            }
        }
        merge.insert(key, value);
    }
    let values = host.update_settings(&manage.plugin_id, &merge).await?;
    Ok(Json(json!({
        "id": manage.plugin_id,
        "values": masked_values(&loaded.descriptors, &values),
    })))
}

/// Replace every secret with `"<set>"` (or `null` when unset). Applied to
/// every response body; secrets never leave the host in cleartext and never
/// reach a log line.
pub(super) fn masked_values(
    descriptors: &[hirsel_plugin_api::SettingDescriptor],
    values: &Map<String, Value>,
) -> Map<String, Value> {
    let mut masked = Map::new();
    for descriptor in descriptors {
        let stored = values.get(&descriptor.key);
        let rendered = match descriptor.kind {
            SettingKind::Secret => match stored {
                Some(Value::String(secret)) if !secret.is_empty() => {
                    Value::String(SECRET_MASK.to_string())
                }
                _ => Value::Null,
            },
            _ => stored.cloned().unwrap_or(Value::Null),
        };
        masked.insert(descriptor.key.clone(), rendered);
    }
    masked
}

/// Management-API error: a status plus a plain message body.
pub(super) struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(id: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("unknown plugin `{id}`"),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.status, self.message).into_response()
    }
}
