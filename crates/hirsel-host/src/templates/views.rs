use std::{collections::BTreeMap, sync::Arc};

use hirsel_proto::{HostToClient, ViewInstance};
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::sync::{RwLock, broadcast};

use super::{bind::bind_spec, spec::validate, store::TemplateStore};
use crate::BroadcastLog;

#[derive(Debug, Clone)]
enum ViewSource {
    Template { template_id: String },
    Inline { spec: Value },
}

#[derive(Debug, Clone, Deserialize)]
struct PatchOperation {
    op: String,
    path: String,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    value: Option<Value>,
}

#[derive(Debug, Clone)]
struct ActiveView {
    view: ViewInstance,
    source: ViewSource,
    params: Value,
    patches: Vec<PatchOperation>,
}

// Active views are intentionally process-local in v1. They are included in every
// reconnect snapshot, while a host restart begins with a clean canvas.
#[derive(Clone)]
pub struct ViewManager {
    templates: TemplateStore,
    active: Arc<RwLock<BTreeMap<String, ActiveView>>>,
    broadcaster: broadcast::Sender<HostToClient>,
    broadcast_log: BroadcastLog,
}

impl ViewManager {
    pub fn new(
        templates: TemplateStore,
        broadcaster: broadcast::Sender<HostToClient>,
        broadcast_log: BroadcastLog,
    ) -> Self {
        Self {
            templates,
            active: Arc::new(RwLock::new(BTreeMap::new())),
            broadcaster,
            broadcast_log,
        }
    }

    pub fn templates(&self) -> &TemplateStore {
        &self.templates
    }

    pub async fn show(
        &self,
        template_id: Option<String>,
        spec: Option<Value>,
        params: Option<Value>,
        instance_id: Option<String>,
        placement: String,
    ) -> anyhow::Result<ViewInstance> {
        validate_placement(&placement)?;
        let params = params.unwrap_or_else(|| Value::Object(Map::new()));
        if !params.is_object() {
            anyhow::bail!("view params must be an object");
        }
        let (source, resolved) = match (template_id, spec) {
            (Some(template_id), None) => {
                let resolved = self.templates.resolve(&template_id, params.clone()).await?;
                (ViewSource::Template { template_id }, resolved)
            }
            (None, Some(spec)) => {
                let resolved = bind_spec(&spec, &params)?;
                validate(&resolved)?;
                (ViewSource::Inline { spec }, resolved)
            }
            _ => anyhow::bail!("provide exactly one of `template_id` or `spec`"),
        };
        let instance_id = instance_id.unwrap_or_else(|| format!("view-{}", uuid::Uuid::new_v4()));
        if instance_id.trim().is_empty() {
            anyhow::bail!("instance_id must be a non-empty string");
        }
        let view = ViewInstance {
            instance_id: instance_id.clone(),
            placement,
            spec: resolved,
        };
        self.active.write().await.insert(
            instance_id,
            ActiveView {
                view: view.clone(),
                source,
                params,
                patches: Vec::new(),
            },
        );
        self.publish_upsert(&view);
        Ok(view)
    }

    pub async fn update(
        &self,
        instance_id: &str,
        params: Option<Value>,
        patch: Option<Value>,
    ) -> anyhow::Result<ViewInstance> {
        if params.is_none() && patch.is_none() {
            anyhow::bail!("provide `params`, `patch`, or both");
        }
        let patch = patch
            .map(serde_json::from_value::<Vec<PatchOperation>>)
            .transpose()
            .map_err(|error| anyhow::anyhow!("invalid JSON Patch: {error}"))?
            .unwrap_or_default();
        let mut record = self
            .active
            .read()
            .await
            .get(instance_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown view instance `{instance_id}`"))?;
        if let Some(params) = params {
            merge_params(&mut record.params, params)?;
        }
        record.patches.extend(patch);
        let mut resolved = match &record.source {
            ViewSource::Template { template_id } => {
                self.templates
                    .resolve(template_id, record.params.clone())
                    .await?
            }
            ViewSource::Inline { spec } => bind_spec(spec, &record.params)?,
        };
        apply_patch(&mut resolved, &record.patches)?;
        validate(&resolved)?;
        record.view.spec = resolved;
        let view = record.view.clone();
        self.active
            .write()
            .await
            .insert(instance_id.to_string(), record);
        self.publish_upsert(&view);
        Ok(view)
    }

    pub async fn clear(&self, instance_id: &str) -> anyhow::Result<()> {
        if self.active.write().await.remove(instance_id).is_none() {
            anyhow::bail!("unknown view instance `{instance_id}`");
        }
        let event = HostToClient::ViewRemoved {
            instance_id: instance_id.to_string(),
        };
        self.broadcast_log.record(event.clone());
        let _ = self.broadcaster.send(event);
        Ok(())
    }

    pub async fn snapshot(&self) -> Vec<ViewInstance> {
        self.active
            .read()
            .await
            .values()
            .map(|record| record.view.clone())
            .collect()
    }

    pub async fn get(&self, instance_id: &str) -> Option<ViewInstance> {
        self.active
            .read()
            .await
            .get(instance_id)
            .map(|record| record.view.clone())
    }

    pub async fn clear_all(&self) {
        self.active.write().await.clear();
    }

    fn publish_upsert(&self, view: &ViewInstance) {
        let event = HostToClient::ViewUpsert {
            instance_id: view.instance_id.clone(),
            placement: view.placement.clone(),
            spec: view.spec.clone(),
        };
        self.broadcast_log.record(event.clone());
        let _ = self.broadcaster.send(event);
    }
}

fn validate_placement(placement: &str) -> anyhow::Result<()> {
    if matches!(placement, "canvas" | "chat") {
        return Ok(());
    }
    if let Some(ping_id) = placement.strip_prefix("ping:")
        && ping_id.parse::<u64>().is_ok_and(|id| id > 0)
    {
        return Ok(());
    }
    anyhow::bail!("placement must be `canvas`, `chat`, or `ping:<ping_id>`")
}

fn merge_params(existing: &mut Value, update: Value) -> anyhow::Result<()> {
    let existing = existing
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("stored view params are not an object"))?;
    let update = update
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("view params update must be an object"))?;
    for (key, value) in update {
        existing.insert(key.clone(), value.clone());
    }
    Ok(())
}

fn apply_patch(document: &mut Value, operations: &[PatchOperation]) -> anyhow::Result<()> {
    for (index, operation) in operations.iter().enumerate() {
        let result = match operation.op.as_str() {
            "add" => patch_add(
                document,
                &operation.path,
                operation
                    .value
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("add requires `value`"))?,
            ),
            "remove" => patch_remove(document, &operation.path).map(|_| ()),
            "replace" => {
                let value = operation
                    .value
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("replace requires `value`"))?;
                patch_replace(document, &operation.path, value)
            }
            "move" => {
                let from = operation
                    .from
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("move requires `from`"))?;
                let value = patch_remove(document, from)?;
                patch_add(document, &operation.path, value)
            }
            "copy" => {
                let from = operation
                    .from
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("copy requires `from`"))?;
                let value = patch_get(document, from)?.clone();
                patch_add(document, &operation.path, value)
            }
            "test" => {
                let expected = operation
                    .value
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("test requires `value`"))?;
                if patch_get(document, &operation.path)? != expected {
                    anyhow::bail!("test failed at `{}`", operation.path);
                }
                Ok(())
            }
            other => anyhow::bail!("unsupported patch operation `{other}`"),
        };
        result.map_err(|error| anyhow::anyhow!("JSON Patch operation {index}: {error}"))?;
    }
    Ok(())
}

fn patch_get<'a>(document: &'a Value, path: &str) -> anyhow::Result<&'a Value> {
    validate_pointer(path)?;
    if path.is_empty() {
        return Ok(document);
    }
    document
        .pointer(path)
        .ok_or_else(|| anyhow::anyhow!("path does not exist: `{path}`"))
}

fn patch_add(document: &mut Value, path: &str, value: Value) -> anyhow::Result<()> {
    let segments = pointer_segments(path)?;
    if segments.is_empty() {
        *document = value;
        return Ok(());
    }
    let (parent_path, key) = segments.split_at(segments.len() - 1);
    let parent = pointer_parent_mut(document, parent_path, path)?;
    let key = &key[0];
    match parent {
        Value::Object(object) => {
            object.insert(key.clone(), value);
            Ok(())
        }
        Value::Array(array) if key == "-" => {
            array.push(value);
            Ok(())
        }
        Value::Array(array) => {
            let index = parse_array_index(key, path)?;
            if index > array.len() {
                anyhow::bail!("array index out of bounds at `{path}`");
            }
            array.insert(index, value);
            Ok(())
        }
        _ => anyhow::bail!("parent is not a container at `{path}`"),
    }
}

fn patch_remove(document: &mut Value, path: &str) -> anyhow::Result<Value> {
    let segments = pointer_segments(path)?;
    if segments.is_empty() {
        return Ok(std::mem::take(document));
    }
    let (parent_path, key) = segments.split_at(segments.len() - 1);
    let parent = pointer_parent_mut(document, parent_path, path)?;
    let key = &key[0];
    match parent {
        Value::Object(object) => object
            .remove(key)
            .ok_or_else(|| anyhow::anyhow!("path does not exist: `{path}`")),
        Value::Array(array) => {
            let index = parse_array_index(key, path)?;
            if index >= array.len() {
                anyhow::bail!("array index out of bounds at `{path}`");
            }
            Ok(array.remove(index))
        }
        _ => anyhow::bail!("parent is not a container at `{path}`"),
    }
}

fn patch_replace(document: &mut Value, path: &str, value: Value) -> anyhow::Result<()> {
    if path.is_empty() {
        *document = value;
        return Ok(());
    }
    let target = patch_get_mut(document, path)?;
    *target = value;
    Ok(())
}

fn patch_get_mut<'a>(document: &'a mut Value, path: &str) -> anyhow::Result<&'a mut Value> {
    validate_pointer(path)?;
    document
        .pointer_mut(path)
        .ok_or_else(|| anyhow::anyhow!("path does not exist: `{path}`"))
}

fn pointer_parent_mut<'a>(
    mut document: &'a mut Value,
    segments: &[String],
    full_path: &str,
) -> anyhow::Result<&'a mut Value> {
    for segment in segments {
        document = match document {
            Value::Object(object) => object
                .get_mut(segment)
                .ok_or_else(|| anyhow::anyhow!("path does not exist: `{full_path}`"))?,
            Value::Array(array) => {
                let index = parse_array_index(segment, full_path)?;
                array
                    .get_mut(index)
                    .ok_or_else(|| anyhow::anyhow!("array index out of bounds at `{full_path}`"))?
            }
            _ => anyhow::bail!("parent is not a container at `{full_path}`"),
        };
    }
    Ok(document)
}

fn validate_pointer(path: &str) -> anyhow::Result<()> {
    pointer_segments(path).map(|_| ())
}

fn pointer_segments(path: &str) -> anyhow::Result<Vec<String>> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let raw = path
        .strip_prefix('/')
        .ok_or_else(|| anyhow::anyhow!("JSON Pointer must be empty or start with `/`: `{path}`"))?;
    raw.split('/').map(decode_pointer_segment).collect()
}

fn decode_pointer_segment(segment: &str) -> anyhow::Result<String> {
    let mut decoded = String::new();
    let mut chars = segment.chars();
    while let Some(character) = chars.next() {
        if character != '~' {
            decoded.push(character);
            continue;
        }
        match chars.next() {
            Some('0') => decoded.push('~'),
            Some('1') => decoded.push('/'),
            _ => anyhow::bail!("invalid `~` escape in JSON Pointer"),
        }
    }
    Ok(decoded)
}

fn parse_array_index(value: &str, path: &str) -> anyhow::Result<usize> {
    if value.starts_with('0') && value.len() > 1 {
        anyhow::bail!("array index has a leading zero at `{path}`");
    }
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid array index `{value}` at `{path}`"))
}
