//! One typed icon contract for owner edits and execution-scoped agent tools.
use super::{Storage, threads};
use base64::Engine;
use hirsel_proto::{Thread, ThreadIcon};
use image::{GenericImageView, ImageEncoder, ImageFormat, imageops::FilterType};
use rusqlite::params;
use serde_json::Value;

pub(crate) const MAX_ICON_BYTES: usize = 256 * 1024;
const MAX_ICON_SOURCE_DIMENSION: u32 = 4_096;
const MAX_ICON_SOURCE_PIXELS: u32 = 16_777_216;
const ICON_EDGE: u32 = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IconSource {
    Emoji(String),
    Blob(String),
    Artifact(u64),
}

fn validate_emoji(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty()
            && value.len() <= 64
            && value.chars().count() <= 16
            && !value
                .chars()
                .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')),
        "emoji icon must be nonblank and at most 16 Unicode code points / 64 UTF-8 bytes without controls or line separators"
    );
    Ok(())
}

pub(super) fn validate_icon(icon: Option<&ThreadIcon>) -> anyhow::Result<()> {
    match icon {
        Some(ThreadIcon::Emoji { value }) => validate_emoji(value),
        Some(ThreadIcon::Image { blob_id }) => {
            anyhow::ensure!(!blob_id.trim().is_empty(), "image icon requires a blob_id");
            Ok(())
        }
        None => Ok(()),
    }
}

/// Outer None means omitted; Some(None) explicitly restores the generated avatar.
pub(crate) fn parse_icon(args: &Value) -> anyhow::Result<Option<Option<ThreadIcon>>> {
    args.get("icon")
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
            let icon: ThreadIcon = serde_json::from_value(value.clone())
                .map_err(|error| anyhow::anyhow!("invalid typed Thread icon: {error}"))?;
            validate_icon(Some(&icon))?;
            Ok(Some(icon))
        })
        .transpose()
}

pub(crate) fn parse_agent_icon(args: &Value) -> anyhow::Result<Option<Option<IconSource>>> {
    args.get("icon")
        .map(|value| {
            if value.is_null() {
                return Ok(None);
            }
            let object = value
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("icon must be a typed object or null"))?;
            let kind = object.get("kind").and_then(Value::as_str);
            match kind {
                Some("emoji") => {
                    anyhow::ensure!(
                        object.len() == 2 && object.contains_key("value"),
                        "emoji icon requires only kind and value"
                    );
                    let value = object["value"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("emoji icon value must be a string"))?;
                    validate_emoji(value)?;
                    Ok(Some(IconSource::Emoji(value.to_owned())))
                }
                Some("image") => {
                    anyhow::ensure!(
                        object.len() == 2,
                        "image icon requires kind and exactly one of blob_id or artifact_id"
                    );
                    match (
                        object.get("blob_id").and_then(Value::as_str),
                        object.get("artifact_id").and_then(Value::as_u64),
                    ) {
                        (Some(blob_id), None) if !blob_id.trim().is_empty() => {
                            Ok(Some(IconSource::Blob(blob_id.to_owned())))
                        }
                        (None, Some(artifact_id)) if artifact_id > 0 => {
                            Ok(Some(IconSource::Artifact(artifact_id)))
                        }
                        _ => anyhow::bail!(
                            "image icon requires exactly one nonempty blob_id or positive artifact_id"
                        ),
                    }
                }
                _ => anyhow::bail!("icon kind must be emoji or image"),
            }
        })
        .transpose()
}

fn image_format(mime: &str) -> anyhow::Result<ImageFormat> {
    match mime {
        "image/png" => Ok(ImageFormat::Png),
        "image/jpeg" => Ok(ImageFormat::Jpeg),
        "image/webp" => Ok(ImageFormat::WebP),
        _ => anyhow::bail!("Thread icon must be PNG, JPEG, or WebP; SVG is not supported"),
    }
}

fn decode_icon(data: &[u8], mime: &str) -> anyhow::Result<image::DynamicImage> {
    anyhow::ensure!(
        !data.is_empty() && data.len() <= MAX_ICON_BYTES,
        "Thread icon bytes must be between 1 byte and 256 KiB"
    );
    let expected = image_format(mime)?;
    let detected = image::guess_format(data).map_err(|_| anyhow::anyhow!("invalid image bytes"))?;
    anyhow::ensure!(
        detected == expected,
        "Thread icon content does not match its MIME type"
    );
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(data), expected)
        .into_dimensions()
        .map_err(|error| anyhow::anyhow!("cannot read Thread icon dimensions: {error}"))?;
    anyhow::ensure!(
        width > 0
            && height > 0
            && width <= MAX_ICON_SOURCE_DIMENSION
            && height <= MAX_ICON_SOURCE_DIMENSION
            && width.saturating_mul(height) <= MAX_ICON_SOURCE_PIXELS,
        "Thread icon dimensions must fit within 4096 pixels per side and 16 megapixels"
    );
    let image = image::load_from_memory_with_format(data, expected)
        .map_err(|error| anyhow::anyhow!("cannot decode Thread icon: {error}"))?;
    Ok(image)
}

pub(crate) fn validate_normalized_icon(data: &[u8], mime: &str) -> anyhow::Result<()> {
    let image = decode_icon(data, mime)?;
    let (width, height) = image.dimensions();
    anyhow::ensure!(
        width == height && width <= ICON_EDGE,
        "Thread icon must be a square no larger than 256 pixels"
    );
    Ok(())
}

pub(crate) fn normalize_icon(data: &[u8], mime: &str) -> anyhow::Result<(Vec<u8>, &'static str)> {
    let image = decode_icon(data, mime)?;
    let (width, height) = image.dimensions();
    let edge = width.min(height);
    let square = image.crop_imm((width - edge) / 2, (height - edge) / 2, edge, edge);
    let resized = square.resize_exact(ICON_EDGE, ICON_EDGE, FilterType::Lanczos3);
    let rgba = resized.to_rgba8();
    let mut encoded = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut encoded).write_image(
        rgba.as_raw(),
        ICON_EDGE,
        ICON_EDGE,
        image::ExtendedColorType::Rgba8,
    )?;
    if encoded.len() <= MAX_ICON_BYTES {
        return Ok((encoded, "image/webp"));
    }

    // Lossless RGBA can be slightly larger than the limit for noisy source
    // images. A bounded JPEG fallback keeps agent-created icons effortless.
    encoded.clear();
    let rgb = resized.to_rgb8();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85).write_image(
        rgb.as_raw(),
        ICON_EDGE,
        ICON_EDGE,
        image::ExtendedColorType::Rgb8,
    )?;
    anyhow::ensure!(
        encoded.len() <= MAX_ICON_BYTES,
        "normalized Thread icon exceeds 256 KiB"
    );
    Ok((encoded, "image/jpeg"))
}

pub(crate) fn artifact_icon_bytes(content: &str) -> anyhow::Result<Vec<u8>> {
    let encoded = content
        .strip_prefix("data:")
        .and_then(|data| data.split_once(','))
        .map_or(content.trim(), |(_, bytes)| bytes.trim());
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("image artifact content must be base64-encoded bytes"))
}

impl Storage {
    pub(crate) async fn prepare_agent_thread_icon(
        &self,
        caller: &super::ThreadCaller,
        source: IconSource,
        client_id: &str,
    ) -> anyhow::Result<ThreadIcon> {
        match source {
            IconSource::Emoji(value) => Ok(ThreadIcon::Emoji { value }),
            IconSource::Blob(blob_id) => {
                let blob = self
                    .blob(&blob_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("unknown blob id: {blob_id}"))?;
                let data = self.read_blob(&blob_id).await?;
                let (data, mime) = normalize_icon(&data, &blob.blob.mime)?;
                let name = if mime == "image/webp" {
                    "thread-icon.webp"
                } else {
                    "thread-icon.jpg"
                };
                let stored = self.store_blob(client_id, name, mime, data).await?;
                Ok(ThreadIcon::Image {
                    blob_id: stored.blob.id,
                })
            }
            IconSource::Artifact(artifact_id) => {
                let artifact = self.scoped_artifact(caller, artifact_id).await?;
                let data = artifact_icon_bytes(&artifact.content)?;
                let (data, mime) = normalize_icon(&data, &artifact.summary.mime)?;
                let name = if mime == "image/webp" {
                    "thread-icon.webp"
                } else {
                    "thread-icon.jpg"
                };
                let stored = self.store_blob(client_id, name, mime, data).await?;
                Ok(ThreadIcon::Image {
                    blob_id: stored.blob.id,
                })
            }
        }
    }

    pub(crate) async fn validate_owner_thread_icon(&self, icon: &ThreadIcon) -> anyhow::Result<()> {
        validate_icon(Some(icon))?;
        if let ThreadIcon::Image { blob_id } = icon {
            let blob = self
                .blob(blob_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unknown blob id: {blob_id}"))?;
            let data = self.read_blob(blob_id).await?;
            validate_normalized_icon(&data, &blob.blob.mime)?;
        }
        Ok(())
    }

    /// Check the revision and write under the same lock as agent mutations.
    pub(crate) async fn update_thread_icon(
        &self,
        expected_history: &str,
        id: u64,
        icon: Option<Option<&ThreadIcon>>,
        expected_revision: u64,
    ) -> anyhow::Result<Thread> {
        if let Some(Some(icon)) = icon {
            self.validate_owner_thread_icon(icon).await?;
        }
        let c = self.conn.lock().await;
        super::thread_scope::validate_history(&c, expected_history)?;
        let current = threads::get(&c, id)?;
        anyhow::ensure!(
            current.revision == expected_revision,
            "thread changed; reload before updating its icon"
        );
        if let Some(icon) = icon {
            let (emoji, blob_id) = match icon {
                Some(ThreadIcon::Emoji { value }) => (Some(value.as_str()), None),
                Some(ThreadIcon::Image { blob_id }) => (None, Some(blob_id.as_str())),
                None => (None, None),
            };
            c.execute(
                "UPDATE threads SET icon=?2,icon_blob_id=?3,updated_at=?4,revision=revision+1 WHERE id=?1",
                params![id, emoji, blob_id, chrono::Utc::now().to_rfc3339()],
            )?;
        }
        threads::get(&c, id)
    }
}

#[cfg(test)]
#[path = "thread_icons_tests.rs"]
mod tests;
