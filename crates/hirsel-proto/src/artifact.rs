//! Explicitly published results. References resolve to the current content.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The one render discriminator. Every variant names exactly one surface the
/// app can draw, and any data a variant carries belongs to that variant alone:
/// nothing outside `Image`/`File` has a MIME type, and nothing outside `File`
/// has a filename, so no two fields can disagree about how to open a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Self-contained Solid JSX module exporting a default component.
    Solid,
    /// Self-contained HTML document.
    Html,
    /// CommonMark/GFM source rendered as a document.
    Markdown,
    /// OpenUI Lang v0.5 source rendered natively by the app's own component
    /// library. Interactive, on-brand by construction, and never executed:
    /// unparseable lines are dropped, not run.
    #[serde(rename = "openui")]
    OpenUi,
    /// Image content: SVG source, or base64 bytes for raster types.
    Image { mime: String },
    /// Opaque UTF-8 payload shown as text and downloaded under its own name.
    File {
        mime: String,
        filename: Option<String>,
    },
}

impl ArtifactKind {
    /// Every stored tag, in declaration order. The store's CHECK is built from
    /// this list, so a new variant cannot be persisted without widening it.
    pub const TAGS: [&'static str; 6] = ["solid", "html", "markdown", "openui", "image", "file"];

    pub fn tag(&self) -> &'static str {
        match self {
            Self::Solid => "solid",
            Self::Html => "html",
            Self::Markdown => "markdown",
            Self::OpenUi => "openui",
            Self::Image { .. } => "image",
            Self::File { .. } => "file",
        }
    }

    /// The MIME type a download should carry. Derived from the kind, never
    /// stored beside it.
    pub fn download_mime(&self) -> &str {
        match self {
            Self::Solid => "text/jsx",
            Self::Html => "text/html",
            Self::Markdown => "text/markdown",
            Self::OpenUi => "text/x-openui",
            Self::Image { mime } | Self::File { mime, .. } => mime,
        }
    }

    /// The single tool-boundary mapping from the ergonomic publish inputs
    /// (a tag plus the optional MIME and filename an agent may know) onto the
    /// discriminator. Markdown and image files are recognized here, once,
    /// instead of by every reader of a stored MIME type.
    pub fn from_publish_inputs(
        tag: &str,
        mime: Option<&str>,
        filename: Option<&str>,
    ) -> Result<Self, String> {
        let essence = mime.map(|mime| {
            mime.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        });
        let essence = essence.as_deref().filter(|value| !value.is_empty());
        let markdown_name = filename.is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.ends_with(".md") || name.ends_with(".markdown")
        });
        match tag {
            "solid" => Ok(Self::Solid),
            "html" => Ok(Self::Html),
            "markdown" => Ok(Self::Markdown),
            "openui" => Ok(Self::OpenUi),
            "image" => Ok(Self::Image {
                mime: essence
                    .map(str::to_string)
                    .or_else(|| image_mime_for(filename?).map(str::to_string))
                    .ok_or_else(|| {
                        "image artifacts require a mime type such as image/svg+xml".to_string()
                    })?,
            }),
            "file" => {
                if essence == Some("text/x-openui") {
                    return Ok(Self::OpenUi);
                }
                if essence == Some("text/markdown") || (essence.is_none() && markdown_name) {
                    return Ok(Self::Markdown);
                }
                if let Some(mime) = essence.filter(|mime| mime.starts_with("image/")) {
                    return Ok(Self::Image {
                        mime: mime.to_string(),
                    });
                }
                Ok(Self::File {
                    mime: essence.unwrap_or("text/plain").to_string(),
                    filename: filename.map(str::to_string),
                })
            }
            other => Err(format!(
                "unknown artifact kind `{other}`; expected one of {}",
                Self::TAGS.join(", ")
            )),
        }
    }
}

fn image_mime_for(filename: &str) -> Option<&'static str> {
    let name = filename.to_ascii_lowercase();
    [
        (".svg", "image/svg+xml"),
        (".png", "image/png"),
        (".jpg", "image/jpeg"),
        (".jpeg", "image/jpeg"),
        (".webp", "image/webp"),
        (".gif", "image/gif"),
    ]
    .into_iter()
    .find_map(|(suffix, mime)| name.ends_with(suffix).then_some(mime))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSummary {
    pub id: u64,
    pub title: String,
    #[serde(flatten)]
    pub kind: ArtifactKind,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub thread_ids: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(flatten)]
    pub summary: ArtifactSummary,
    pub content: String,
}
