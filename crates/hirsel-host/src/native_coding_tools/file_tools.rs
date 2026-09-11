use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use lash_core::{AttachmentSource, MediaType, ToolCallOutput, ToolOutcome, ToolValue};
use serde_json::{Value, json};
use tokio::sync::Mutex;

const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
const MAX_READ_BYTES: usize = 50 * 1024;
const MAX_READ_LINES: usize = 2_000;

pub(super) async fn execute_file_tool(
    name: &str,
    args: &Value,
    cwd: Arc<PathBuf>,
    mutations: Arc<Mutex<()>>,
) -> ToolOutcome {
    let result = match name {
        super::READ => {
            let args = args.clone();
            tokio::task::spawn_blocking(move || read_file(&cwd, &args)).await
        }
        super::EDIT => {
            let args = args.clone();
            let _mutation = mutations.lock().await;
            tokio::task::spawn_blocking(move || edit_file(&cwd, &args)).await
        }
        super::WRITE => {
            let args = args.clone();
            let _mutation = mutations.lock().await;
            tokio::task::spawn_blocking(move || write_file(&cwd, &args)).await
        }
        _ => return ToolOutcome::err_fmt(format!("unknown file tool `{name}`")),
    };

    match result {
        Ok(Ok(value)) => ToolOutcome::from_output(ToolCallOutput::success_tool_value(value)),
        Ok(Err(error)) => ToolOutcome::err_fmt(error),
        Err(error) => ToolOutcome::err_fmt(format!("file tool task failed: {error}")),
    }
}

fn read_file(cwd: &Path, args: &Value) -> Result<ToolValue, String> {
    let path = resolve_path(cwd, required_string(args, "path")?);
    let offset = optional_usize(args, "offset", 1, MAX_FILE_BYTES)?;
    let limit = optional_usize(args, "limit", MAX_READ_LINES, MAX_READ_LINES)?;

    let bytes = bounded_read(&path)?;
    if let Some(media_type) = image_media_type(&path) {
        if args.get("offset").is_some() || args.get("limit").is_some() {
            return Err("`offset` and `limit` are not supported for image reads".to_string());
        }
        let media_type = MediaType::parse(media_type)
            .map_err(|error| format!("unsupported image media type: {error}"))?;
        return Ok(ToolValue::Array(vec![
            ToolValue::String(format!(
                "Image `{}` ({} bytes)",
                path.display(),
                bytes.len()
            )),
            ToolValue::Attachment(AttachmentSource::inline(media_type, bytes)),
        ]));
    }

    let text = String::from_utf8(bytes)
        .map_err(|_| format!("file is not valid UTF-8: `{}`", path.display()))?;
    Ok(ToolValue::untrusted_json(text_window(
        &path, &text, offset, limit,
    )))
}

fn edit_file(cwd: &Path, args: &Value) -> Result<ToolValue, String> {
    let path = resolve_path(cwd, required_string(args, "path")?);
    let old_text = required_string(args, "old_text")?;
    if old_text.is_empty() {
        return Err("field `old_text` must not be empty".to_string());
    }
    let new_text = required_string(args, "new_text")?;
    let bytes = bounded_read(&path)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("file is not valid UTF-8: `{}`", path.display()))?;
    let matches = overlapping_match_starts(&text, old_text);
    let Some(&start) = matches.first() else {
        return Err("old text was not found; file was not changed".to_string());
    };
    if matches.len() != 1 {
        return Err(
            "old text is ambiguous (more than one exact match); file was not changed".to_string(),
        );
    }

    let new_len = text
        .len()
        .checked_sub(old_text.len())
        .and_then(|len| len.checked_add(new_text.len()))
        .ok_or_else(|| "edited file size overflow".to_string())?;
    if new_len > MAX_FILE_BYTES {
        return Err(format!("edited file exceeds {MAX_FILE_BYTES} byte limit"));
    }
    let mut edited = String::with_capacity(new_len);
    edited.push_str(&text[..start]);
    edited.push_str(new_text);
    edited.push_str(&text[start + old_text.len()..]);
    atomic_write(&path, edited.as_bytes())?;
    Ok(ToolValue::untrusted_json(json!({
        "path": path,
        "bytes_written": edited.len(),
        "replacements": 1
    })))
}

fn write_file(cwd: &Path, args: &Value) -> Result<ToolValue, String> {
    let path = resolve_path(cwd, required_string(args, "path")?);
    let content = required_string(args, "content")?;
    if content.len() > MAX_FILE_BYTES {
        return Err(format!("content exceeds {MAX_FILE_BYTES} byte limit"));
    }
    atomic_write(&path, content.as_bytes())?;
    Ok(ToolValue::untrusted_json(json!({
        "path": path,
        "bytes_written": content.len()
    })))
}

fn bounded_read(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path)
        .map_err(|error| format!("failed to open `{}`: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read `{}`: {error}", path.display()))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!(
            "file exceeds {MAX_FILE_BYTES} byte limit: `{}`",
            path.display()
        ));
    }
    Ok(bytes)
}

fn text_window(path: &Path, text: &str, offset: usize, limit: usize) -> Value {
    let lines = text.split_inclusive('\n').collect::<Vec<_>>();
    let start = offset.saturating_sub(1).min(lines.len());
    let requested_end = start.saturating_add(limit).min(lines.len());
    let mut content = String::new();
    let mut consumed = 0_usize;
    let mut line_truncated = false;

    for line in &lines[start..requested_end] {
        let remaining = MAX_READ_BYTES.saturating_sub(content.len());
        if line.len() <= remaining {
            content.push_str(line);
            consumed += 1;
            continue;
        }
        if content.is_empty() && remaining > 0 {
            let mut end = remaining.min(line.len());
            while end > 0 && !line.is_char_boundary(end) {
                end -= 1;
            }
            content.push_str(&line[..end]);
            consumed = 1;
            line_truncated = true;
        }
        break;
    }

    let end_index = start.saturating_add(consumed);
    let truncated = line_truncated || end_index < lines.len();
    json!({
        "path": path,
        "content": content,
        "start_line": if lines.is_empty() { 0 } else { start + 1 },
        "end_line": end_index,
        "total_lines": lines.len(),
        "truncated": truncated,
        "line_truncated": line_truncated,
        "next_offset": truncated.then_some(end_index + 1)
    })
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create parent `{}`: {error}", parent.display()))?;
    let existing_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        format!(
            "failed to create temporary file in `{}`: {error}",
            parent.display()
        )
    })?;
    temporary
        .write_all(content)
        .and_then(|()| temporary.flush())
        .map_err(|error| {
            format!(
                "failed to write temporary file for `{}`: {error}",
                path.display()
            )
        })?;
    if let Some(permissions) = existing_permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| {
                format!(
                    "failed to preserve permissions for `{}`: {error}",
                    path.display()
                )
            })?;
    }
    temporary.persist(path).map_err(|error| {
        format!(
            "failed to atomically replace `{}`: {}",
            path.display(),
            error.error
        )
    })?;
    Ok(())
}

fn resolve_path(cwd: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string field `{key}`"))
}

fn optional_usize(
    args: &Value,
    key: &str,
    default: usize,
    maximum: usize,
) -> Result<usize, String> {
    let Some(value) = args.get(key) else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value >= 1 && *value <= maximum)
        .ok_or_else(|| format!("field `{key}` must be an integer from 1 through {maximum}"))?;
    Ok(value)
}

fn image_media_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn overlapping_match_starts(text: &str, needle: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(2);
    let advance = needle.chars().next().map(char::len_utf8).unwrap_or(1);
    let mut search_start = 0;
    while search_start <= text.len() {
        let Some(relative) = text[search_start..].find(needle) else {
            break;
        };
        let start = search_start + relative;
        starts.push(start);
        if starts.len() > 1 {
            break;
        }
        search_start = start + advance;
    }
    starts
}
