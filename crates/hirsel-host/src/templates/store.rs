use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use super::{bind::bind_spec, spec::validate};

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
