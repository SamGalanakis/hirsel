use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use hirsel_proto::{HostToClient, ViewInstance};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{RwLock, broadcast};

use crate::BroadcastLog;

pub fn bundled_templates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates")
}

#[derive(Debug, Clone, Deserialize)]
struct TemplateFile {
    id: String,
    title: String,
    #[serde(default)]
    params_schema: BTreeMap<String, String>,
    spec: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateSummary {
    pub id: String,
    pub title: String,
}

#[derive(Clone)]
pub struct TemplateStore {
    dir: Arc<PathBuf>,
    cache: Arc<RwLock<BTreeMap<String, TemplateFile>>>,
}

impl TemplateStore {
    pub async fn load(dir: PathBuf) -> anyhow::Result<Self> {
        let store = Self {
            dir: Arc::new(dir),
            cache: Arc::new(RwLock::new(BTreeMap::new())),
        };
        store.refresh().await?;
        Ok(store)
    }

    pub fn directory(&self) -> &Path {
        &self.dir
    }

    pub async fn list(&self) -> anyhow::Result<Vec<TemplateSummary>> {
        self.refresh().await?;
        Ok(self
            .cache
            .read()
            .await
            .values()
            .map(|template| TemplateSummary {
                id: template.id.clone(),
                title: template.title.clone(),
            })
            .collect())
    }

    pub async fn resolve(&self, template_id: &str, params: Value) -> anyhow::Result<Value> {
        validate_template_id(template_id)?;
        let path = self.dir.join(format!("{template_id}.json"));
        let template = read_template(&path).await?;
        if template.id != template_id {
            anyhow::bail!(
                "template id mismatch in {}: expected `{template_id}`, found `{}`",
                path.display(),
                template.id
            );
        }
        validate_params(&template.params_schema, &params)?;
        let resolved = bind_spec(&template.spec, &params)?;
        validate(&resolved)?;
        self.cache
            .write()
            .await
            .insert(template.id.clone(), template);
        Ok(resolved)
    }

    async fn refresh(&self) -> anyhow::Result<()> {
        let mut entries = tokio::fs::read_dir(&*self.dir).await.map_err(|error| {
            anyhow::anyhow!("read templates directory {}: {error}", self.dir.display())
        })?;
        let mut templates = BTreeMap::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let template = read_template(&path).await?;
            validate_template_id(&template.id)?;
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| {
                    anyhow::anyhow!("template filename is not valid UTF-8: {}", path.display())
                })?;
            if stem != template.id {
                anyhow::bail!(
                    "template id mismatch in {}: filename is `{stem}`, id is `{}`",
                    path.display(),
                    template.id
                );
            }
            if templates.insert(template.id.clone(), template).is_some() {
                anyhow::bail!("duplicate template id `{stem}`");
            }
        }
        *self.cache.write().await = templates;
        Ok(())
    }
}

async fn read_template(path: &Path) -> anyhow::Result<TemplateFile> {
    let contents = tokio::fs::read_to_string(path)
        .await
        .map_err(|error| anyhow::anyhow!("read template {}: {error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| anyhow::anyhow!("parse template {}: {error}", path.display()))
}

fn validate_template_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        anyhow::bail!("invalid template id `{id}`; use ASCII letters, digits, `-`, or `_`");
    }
    Ok(())
}

fn validate_params(schema: &BTreeMap<String, String>, params: &Value) -> anyhow::Result<()> {
    let params = params
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("template params must be an object"))?;
    for (name, expected) in schema {
        let value = params
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("missing template param `{name}`"))?;
        let valid = match expected.as_str() {
            "string" => value.is_string(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            other => anyhow::bail!("unsupported params_schema type `{other}` for `{name}`"),
        };
        if !valid {
            anyhow::bail!(
                "template param `{name}` must be {expected}, got {}",
                value_kind(value)
            );
        }
    }
    Ok(())
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub fn bind_spec(spec: &Value, params: &Value) -> anyhow::Result<Value> {
    if !params.is_object() {
        anyhow::bail!("view params must be an object");
    }
    bind_value(spec, params, params, "spec")
}

fn bind_value(value: &Value, root: &Value, current: &Value, at: &str) -> anyhow::Result<Value> {
    match value {
        Value::String(text) => bind_string(text, root, current, at),
        Value::Array(values) => {
            let mut resolved = Vec::new();
            for (index, item) in values.iter().enumerate() {
                let item_path = format!("{at}[{index}]");
                if let Some((path, template)) = each_directive(item)? {
                    let items = lookup_binding(root, current, path).ok_or_else(|| {
                        anyhow::anyhow!(
                            "missing each binding `{{{{#each {path}}}}}` at {item_path}"
                        )
                    })?;
                    let items = items.as_array().ok_or_else(|| {
                        anyhow::anyhow!("each binding `{path}` at {item_path} must be an array")
                    })?;
                    for (each_index, each_item) in items.iter().enumerate() {
                        resolved.push(bind_value(
                            template,
                            root,
                            each_item,
                            &format!("{item_path}{{{each_index}}}"),
                        )?);
                    }
                } else {
                    resolved.push(bind_value(item, root, current, &item_path)?);
                }
            }
            Ok(Value::Array(resolved))
        }
        Value::Object(object) => {
            let mut resolved = Map::new();
            for (key, child) in object {
                resolved.insert(
                    key.clone(),
                    bind_value(child, root, current, &format!("{at}.{key}"))?,
                );
            }
            Ok(Value::Object(resolved))
        }
        _ => Ok(value.clone()),
    }
}

fn each_directive(value: &Value) -> anyhow::Result<Option<(&str, &Value)>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if object.len() != 1 {
        return Ok(None);
    }
    let (key, template) = object.iter().next().expect("one entry");
    let Some(inner) = key
        .strip_prefix("{{#each ")
        .and_then(|value| value.strip_suffix("}}"))
    else {
        return Ok(None);
    };
    let path = inner.trim();
    if path.is_empty() {
        anyhow::bail!("each binding path must not be empty");
    }
    Ok(Some((path, template)))
}

fn bind_string(text: &str, root: &Value, current: &Value, at: &str) -> anyhow::Result<Value> {
    if let Some(path) = text
        .strip_prefix("{{")
        .and_then(|value| value.strip_suffix("}}"))
        .filter(|value| !value.contains("{{") && !value.starts_with("#each "))
    {
        let path = path.trim();
        return lookup_binding(root, current, path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing binding `{{{{{path}}}}}` at {at}"));
    }

    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let binding = &rest[start + 2..];
        let end = binding
            .find("}}")
            .ok_or_else(|| anyhow::anyhow!("unterminated binding at {at}"))?;
        let path = binding[..end].trim();
        if path.starts_with("#each ") {
            anyhow::bail!("each bindings are only valid as array entries at {at}");
        }
        let value = lookup_binding(root, current, path)
            .ok_or_else(|| anyhow::anyhow!("missing binding `{{{{{path}}}}}` at {at}"))?;
        match value {
            Value::Null => {}
            Value::String(value) => output.push_str(value),
            other => output.push_str(&serde_json::to_string(other)?),
        }
        rest = &binding[end + 2..];
    }
    output.push_str(rest);
    Ok(Value::String(output))
}

fn lookup_binding<'a>(root: &'a Value, current: &'a Value, path: &str) -> Option<&'a Value> {
    if path == "this" {
        return Some(current);
    }
    if let Some(path) = path.strip_prefix("this.") {
        return descend(current, path);
    }
    if path == "@root" {
        return Some(root);
    }
    if let Some(path) = path.strip_prefix("@root.") {
        return descend(root, path);
    }
    descend(current, path).or_else(|| descend(root, path))
}

fn descend<'a>(mut value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(value);
    }
    for segment in path.split('.') {
        value = match value {
            Value::Object(object) => object.get(segment)?,
            Value::Array(array) => array.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(value)
}

pub fn validate(spec: &Value) -> anyhow::Result<()> {
    validate_node(spec, "spec")
}

fn validate_node(node: &Value, at: &str) -> anyhow::Result<()> {
    let object = node
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{at} must be a component object"))?;
    let component = required_string(object, "type", at)?;
    match component {
        "card" => {
            allowed(object, &["type", "title", "subtitle", "children"], at)?;
            optional_string(object, "title", at)?;
            optional_string(object, "subtitle", at)?;
            children(object, "children", true, at)?;
        }
        "stack" => {
            allowed(object, &["type", "gap", "children"], at)?;
            optional_enum(object, "gap", &["xs", "sm", "md", "lg"], at)?;
            children(object, "children", true, at)?;
        }
        "row" => {
            allowed(object, &["type", "gap", "align", "wrap", "children"], at)?;
            optional_enum(object, "gap", &["xs", "sm", "md", "lg"], at)?;
            optional_enum(object, "align", &["start", "center", "end", "stretch"], at)?;
            optional_bool(object, "wrap", at)?;
            children(object, "children", true, at)?;
        }
        "heading" => {
            allowed(object, &["type", "text", "level"], at)?;
            required_string(object, "text", at)?;
            optional_integer_range(object, "level", 1, 4, at)?;
        }
        "text" => {
            allowed(object, &["type", "text", "tone"], at)?;
            required_display_scalar(object, "text", at)?;
            optional_tone(object, "tone", at)?;
        }
        "keyValue" => {
            allowed(object, &["type", "items"], at)?;
            object_array(object, "items", at, |item, item_at| {
                allowed(item, &["label", "value", "tone"], item_at)?;
                required_string(item, "label", item_at)?;
                required_display_scalar(item, "value", item_at)?;
                optional_tone(item, "tone", item_at)
            })?;
        }
        "table" => validate_table(object, at)?,
        "list" => {
            allowed(object, &["type", "items", "ordered"], at)?;
            optional_bool(object, "ordered", at)?;
            object_array(object, "items", at, |item, item_at| {
                allowed(item, &["text", "tone"], item_at)?;
                required_display_scalar(item, "text", item_at)?;
                optional_tone(item, "tone", item_at)
            })?;
        }
        "checklist" => {
            allowed(object, &["type", "items"], at)?;
            object_array(object, "items", at, |item, item_at| {
                allowed(item, &["label", "checked", "detail"], item_at)?;
                required_string(item, "label", item_at)?;
                required_bool(item, "checked", item_at)?;
                optional_string(item, "detail", item_at)
            })?;
        }
        "badge" => {
            allowed(object, &["type", "label", "tone"], at)?;
            required_string(object, "label", at)?;
            optional_tone(object, "tone", at)?;
        }
        "status" => {
            allowed(object, &["type", "label", "state"], at)?;
            required_string(object, "label", at)?;
            required_enum(
                object,
                "state",
                &["neutral", "running", "success", "warning", "danger"],
                at,
            )?;
        }
        "progress" => {
            allowed(object, &["type", "value", "label"], at)?;
            required_number_range(object, "value", 0.0, 1.0, at)?;
            optional_string(object, "label", at)?;
        }
        "callout" => {
            allowed(object, &["type", "tone", "title", "body"], at)?;
            optional_enum(
                object,
                "tone",
                &["default", "success", "warning", "danger"],
                at,
            )?;
            optional_string(object, "title", at)?;
            required_string(object, "body", at)?;
        }
        "divider" => allowed(object, &["type"], at)?,
        "action" => {
            allowed(object, &["type", "label", "action", "data", "variant"], at)?;
            required_string(object, "label", at)?;
            required_string(object, "action", at)?;
            optional_enum(object, "variant", &["primary", "secondary", "danger"], at)?;
        }
        "optionSet" => validate_option_set(object, at)?,
        "field" => validate_field(object, at)?,
        "form" => validate_form(object, at)?,
        other => anyhow::bail!("unknown component type `{other}` at {at}"),
    }
    Ok(())
}

fn validate_table(object: &Map<String, Value>, at: &str) -> anyhow::Result<()> {
    allowed(object, &["type", "columns", "rows", "caption"], at)?;
    optional_string(object, "caption", at)?;
    let mut keys = BTreeSet::new();
    object_array(object, "columns", at, |column, column_at| {
        allowed(column, &["key", "label", "align"], column_at)?;
        let key = required_string(column, "key", column_at)?;
        if !keys.insert(key.to_string()) {
            anyhow::bail!("duplicate table column key `{key}` at {column_at}");
        }
        required_string(column, "label", column_at)?;
        optional_enum(column, "align", &["start", "center", "end"], column_at)
    })?;
    let rows = object
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("{at}.rows must be an array"))?;
    for (index, row) in rows.iter().enumerate() {
        let row_at = format!("{at}.rows[{index}]");
        let row = row
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{row_at} must be an object"))?;
        for key in &keys {
            let value = row
                .get(key)
                .ok_or_else(|| anyhow::anyhow!("{row_at} is missing column `{key}`"))?;
            if !is_display_scalar(value) {
                anyhow::bail!("{row_at}.{key} must be a string, number, or boolean");
            }
        }
        if let Some(extra) = row.keys().find(|key| !keys.contains(*key)) {
            anyhow::bail!("unknown table column `{extra}` at {row_at}");
        }
    }
    Ok(())
}

fn validate_option_set(object: &Map<String, Value>, at: &str) -> anyhow::Result<()> {
    allowed(
        object,
        &["type", "action", "label", "choices", "selected"],
        at,
    )?;
    required_string(object, "action", at)?;
    optional_string(object, "label", at)?;
    optional_display_scalar(object, "selected", at)?;
    object_array(object, "choices", at, |choice, choice_at| {
        allowed(choice, &["label", "value", "description"], choice_at)?;
        required_string(choice, "label", choice_at)?;
        required_display_scalar(choice, "value", choice_at)?;
        optional_string(choice, "description", choice_at)
    })
}

fn validate_field(object: &Map<String, Value>, at: &str) -> anyhow::Result<()> {
    allowed(
        object,
        &[
            "type",
            "name",
            "label",
            "kind",
            "value",
            "placeholder",
            "required",
            "options",
        ],
        at,
    )?;
    required_string(object, "name", at)?;
    required_string(object, "label", at)?;
    let kind = required_enum(
        object,
        "kind",
        &["text", "textarea", "number", "toggle", "select"],
        at,
    )?;
    optional_string(object, "placeholder", at)?;
    optional_bool(object, "required", at)?;
    if let Some(value) = object.get("value")
        && !is_display_scalar(value)
        && !value.is_null()
    {
        anyhow::bail!("{at}.value must be null, a string, number, or boolean");
    }
    match (kind, object.get("options")) {
        ("select", Some(options)) => validate_field_options(options, at),
        ("select", None) => anyhow::bail!("{at}.options is required for a select field"),
        (_, Some(_)) => anyhow::bail!("{at}.options is only valid for a select field"),
        (_, None) => Ok(()),
    }
}

fn validate_field_options(options: &Value, at: &str) -> anyhow::Result<()> {
    let options = options
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{at}.options must be an array"))?;
    for (index, option) in options.iter().enumerate() {
        let option_at = format!("{at}.options[{index}]");
        let option = option
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{option_at} must be an object"))?;
        allowed(option, &["label", "value"], &option_at)?;
        required_string(option, "label", &option_at)?;
        required_display_scalar(option, "value", &option_at)?;
    }
    Ok(())
}

fn validate_form(object: &Map<String, Value>, at: &str) -> anyhow::Result<()> {
    allowed(object, &["type", "action", "fields", "submitLabel"], at)?;
    required_string(object, "action", at)?;
    optional_string(object, "submitLabel", at)?;
    let fields = object
        .get("fields")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("{at}.fields must be an array"))?;
    for (index, field) in fields.iter().enumerate() {
        let field_at = format!("{at}.fields[{index}]");
        validate_node(field, &field_at)?;
        if field.get("type").and_then(Value::as_str) != Some("field") {
            anyhow::bail!("{field_at} must be a field component");
        }
    }
    Ok(())
}

fn allowed(object: &Map<String, Value>, keys: &[&str], at: &str) -> anyhow::Result<()> {
    if let Some(key) = object.keys().find(|key| !keys.contains(&key.as_str())) {
        anyhow::bail!("unknown property `{key}` at {at}");
    }
    Ok(())
}

fn children(
    object: &Map<String, Value>,
    key: &str,
    required: bool,
    at: &str,
) -> anyhow::Result<()> {
    let Some(value) = object.get(key) else {
        if required {
            anyhow::bail!("missing required array `{key}` at {at}");
        }
        return Ok(());
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be an array"))?;
    for (index, child) in values.iter().enumerate() {
        validate_node(child, &format!("{at}.{key}[{index}]"))?;
    }
    Ok(())
}

fn object_array<F>(
    object: &Map<String, Value>,
    key: &str,
    at: &str,
    mut validate_item: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Map<String, Value>, &str) -> anyhow::Result<()>,
{
    let values = object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be an array"))?;
    for (index, item) in values.iter().enumerate() {
        let item_at = format!("{at}.{key}[{index}]");
        let item = item
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("{item_at} must be an object"))?;
        validate_item(item, &item_at)?;
    }
    Ok(())
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    at: &str,
) -> anyhow::Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be a non-empty string"))
}

fn optional_string(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    if let Some(value) = object.get(key)
        && !value.is_string()
    {
        anyhow::bail!("{at}.{key} must be a string");
    }
    Ok(())
}

fn required_bool(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    if object.get(key).and_then(Value::as_bool).is_none() {
        anyhow::bail!("{at}.{key} must be a boolean");
    }
    Ok(())
}

fn optional_bool(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    if let Some(value) = object.get(key)
        && !value.is_boolean()
    {
        anyhow::bail!("{at}.{key} must be a boolean");
    }
    Ok(())
}

fn required_display_scalar(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    if !object.get(key).is_some_and(is_display_scalar) {
        anyhow::bail!("{at}.{key} must be a string, number, or boolean");
    }
    Ok(())
}

fn optional_display_scalar(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    if let Some(value) = object.get(key)
        && !is_display_scalar(value)
    {
        anyhow::bail!("{at}.{key} must be a string, number, or boolean");
    }
    Ok(())
}

fn is_display_scalar(value: &Value) -> bool {
    value.is_string() || value.is_number() || value.is_boolean()
}

fn optional_tone(object: &Map<String, Value>, key: &str, at: &str) -> anyhow::Result<()> {
    optional_enum(
        object,
        key,
        &["default", "muted", "success", "warning", "danger"],
        at,
    )
}

fn required_enum<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    choices: &[&str],
    at: &str,
) -> anyhow::Result<&'a str> {
    let value = required_string(object, key, at)?;
    if !choices.contains(&value) {
        anyhow::bail!("{at}.{key} must be one of: {}", choices.join(", "));
    }
    Ok(value)
}

fn optional_enum(
    object: &Map<String, Value>,
    key: &str,
    choices: &[&str],
    at: &str,
) -> anyhow::Result<()> {
    if object.contains_key(key) {
        required_enum(object, key, choices, at)?;
    }
    Ok(())
}

fn required_number_range(
    object: &Map<String, Value>,
    key: &str,
    min: f64,
    max: f64,
    at: &str,
) -> anyhow::Result<()> {
    let value = object
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be a number"))?;
    if !(min..=max).contains(&value) {
        anyhow::bail!("{at}.{key} must be between {min} and {max}");
    }
    Ok(())
}

fn optional_integer_range(
    object: &Map<String, Value>,
    key: &str,
    min: u64,
    max: u64,
    at: &str,
) -> anyhow::Result<()> {
    if let Some(raw) = object.get(key) {
        let value = raw
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("{at}.{key} must be an integer"))?;
        if !(min..=max).contains(&value) {
            anyhow::bail!("{at}.{key} must be between {min} and {max}");
        }
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hirsel_proto::HostToClient;
    use serde_json::json;
    use tokio::sync::broadcast;

    use super::{TemplateStore, ViewManager, bind_spec, bundled_templates_dir, validate};
    use crate::BroadcastLog;

    #[test]
    fn binds_typed_values_interpolation_and_each_blocks() {
        let spec = json!({
            "type": "stack",
            "gap": "sm",
            "children": [
                { "type": "text", "text": "{{count}} checks" },
                { "{{#each checks}}": {
                    "type": "text",
                    "text": "{{label}}",
                    "tone": "{{tone}}"
                }}
            ]
        });
        let resolved = bind_spec(
            &spec,
            &json!({
                "count": 2,
                "checks": [
                    { "label": "Build", "tone": "success" },
                    { "label": "Review", "tone": "muted" }
                ]
            }),
        )
        .unwrap();
        assert_eq!(resolved["children"][0]["text"], "2 checks");
        assert_eq!(resolved["children"][1]["text"], "Build");
        assert_eq!(resolved["children"][2]["tone"], "muted");
        validate(&resolved).unwrap();
    }

    #[test]
    fn validation_rejects_unknown_components_and_properties() {
        let error = validate(&json!({ "type": "marquee", "text": "no" }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown component type `marquee`"));

        let error = validate(&json!({ "type": "text", "text": "ok", "flash": true }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown property `flash`"));
    }

    #[tokio::test]
    async fn template_resolve_reloads_edits_without_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.json");
        tokio::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "id": "status",
                "title": "Status",
                "params_schema": { "message": "string" },
                "spec": { "type": "text", "text": "{{message}}" }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let store = TemplateStore::load(dir.path().to_path_buf()).await.unwrap();
        let first = store
            .resolve("status", json!({ "message": "calm" }))
            .await
            .unwrap();
        assert_eq!(first["text"], "calm");

        tokio::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "id": "status",
                "title": "Status",
                "params_schema": { "message": "string" },
                "spec": { "type": "text", "text": "Now: {{message}}" }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let second = store
            .resolve("status", json!({ "message": "ready" }))
            .await
            .unwrap();
        assert_eq!(second["text"], "Now: ready");
    }

    #[tokio::test]
    async fn seed_templates_resolve_and_validate() {
        let store = TemplateStore::load(bundled_templates_dir()).await.unwrap();
        let cases = BTreeMap::from([
            (
                "decision",
                json!({
                    "title": "Choose a release window",
                    "context": "Two safe windows remain.",
                    "question": "Which one should I use?",
                    "choices": [
                        { "label": "Tonight", "value": "tonight", "description": "Lower traffic." },
                        { "label": "Tomorrow", "value": "tomorrow", "description": "More observers." }
                    ]
                }),
            ),
            (
                "pr-summary",
                json!({
                    "title": "Keep reconnect snapshots stable",
                    "branch": "feat/reconnect",
                    "files": 8,
                    "tests_ok": true,
                    "tests_label": "Tests passing",
                    "tests_state": "success",
                    "checks": [
                        { "label": "Build", "checked": true, "detail": "Workspace" },
                        { "label": "Review", "checked": false, "detail": "Awaiting owner" }
                    ]
                }),
            ),
            (
                "status-digest",
                json!({
                    "title": "Workstream status",
                    "updated": "just now",
                    "workstreams": [
                        { "name": "Host", "state": "success", "detail": "Steady." },
                        { "name": "Client", "state": "running", "detail": "In progress." }
                    ]
                }),
            ),
            (
                "table-report",
                json!({
                    "title": "Checks",
                    "summary": "All required checks reported.",
                    "columns": [
                        { "key": "name", "label": "Check" },
                        { "key": "result", "label": "Result", "align": "end" }
                    ],
                    "rows": [
                        { "name": "Build", "result": "Pass" },
                        { "name": "Test", "result": "Pass" }
                    ],
                    "caption": "Latest run"
                }),
            ),
            (
                "task-progress",
                json!({
                    "title": "Release preparation",
                    "value": 0.6,
                    "progress_label": "Three of five steps",
                    "current_step": "Running the full suite.",
                    "state_label": "In progress",
                    "state": "running"
                }),
            ),
        ]);
        for (template_id, params) in cases {
            store.resolve(template_id, params).await.unwrap();
        }
        assert_eq!(store.list().await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn view_show_update_patch_clear_lifecycle_broadcasts() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(
            dir.path().join("progress.json"),
            serde_json::to_vec(&json!({
                "id": "progress",
                "title": "Progress",
                "params_schema": { "value": "number", "label": "string" },
                "spec": {
                    "type": "progress",
                    "value": "{{value}}",
                    "label": "{{label}}"
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        let templates = TemplateStore::load(dir.path().to_path_buf()).await.unwrap();
        let (broadcaster, mut broadcasts) = broadcast::channel(8);
        let log = BroadcastLog::default();
        let views = ViewManager::new(templates, broadcaster, log.clone());

        let shown = views
            .show(
                Some("progress".to_string()),
                None,
                Some(json!({ "value": 0.2, "label": "Starting" })),
                Some("view-test".to_string()),
                "canvas".to_string(),
            )
            .await
            .unwrap();
        assert_eq!(shown.spec["value"], 0.2);
        assert!(matches!(
            broadcasts.recv().await.unwrap(),
            HostToClient::ViewUpsert { .. }
        ));

        let updated = views
            .update(
                "view-test",
                Some(json!({ "value": 0.8 })),
                Some(json!([
                    { "op": "replace", "path": "/label", "value": "Nearly done" }
                ])),
            )
            .await
            .unwrap();
        assert_eq!(updated.spec["value"], 0.8);
        assert_eq!(updated.spec["label"], "Nearly done");
        assert_eq!(views.snapshot().await, vec![updated]);
        assert!(matches!(
            broadcasts.recv().await.unwrap(),
            HostToClient::ViewUpsert { .. }
        ));

        views.clear("view-test").await.unwrap();
        assert!(views.snapshot().await.is_empty());
        assert!(matches!(
            broadcasts.recv().await.unwrap(),
            HostToClient::ViewRemoved { .. }
        ));
        assert!(log.recent().iter().any(|event| matches!(
            event,
            HostToClient::ViewRemoved { instance_id } if instance_id == "view-test"
        )));
    }
}
