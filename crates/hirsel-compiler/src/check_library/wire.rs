//! HCL1 worker request framing and canonical library facts.
use super::{CheckLibraryCaps, CheckLibraryRequest};
use serde_json::Value;
use std::fmt::Write as _;
use std::io::{Read, Write};

pub const WIRE_MAGIC: &[u8; 4] = b"HCL1";
pub const UNSUPPORTED_PREFIX: &str = "check_library: unsupported: ";
pub const LIMIT_INPUT_PREFIX: &str = "check_library: limit: input: ";
pub const LIMIT_OUTPUT_PREFIX: &str = "check_library: limit: output: ";

const CONTENT_LIMIT: usize = CheckLibraryCaps::REGISTRY.max_raw_content_bytes as usize;
const ARCHIVE_LIMIT: usize = CheckLibraryCaps::REGISTRY.max_total_content_bytes as usize;
const PACKAGE_LIMIT: usize = CheckLibraryCaps::REGISTRY.max_packages;
const HISTORY_METADATA_LIMIT: usize =
    CheckLibraryCaps::REGISTRY.max_history_metadata_bytes as usize;
pub const ID_PREFIX: &str = "check_library: compiler_id: ";

pub enum Failure {
    Rejected(String),
    Unsupported(String),
    InputLimit(String),
    OutputLimit(String),
    Unavailable(String),
}

pub struct WireHistory<R> {
    pub origin: String,
    pub identity: String,
    pub version: String,
    pub root: R,
}

fn read_exact_or_reject(reader: &mut impl Read, buf: &mut [u8], what: &str) -> Result<(), Failure> {
    reader
        .read_exact(buf)
        .map_err(|e| Failure::Rejected(format!("truncated request while reading {what}: {e}")))
}

fn read_u32(reader: &mut impl Read, what: &str) -> Result<u32, Failure> {
    let mut buf = [0u8; 4];
    read_exact_or_reject(reader, &mut buf, what)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(reader: &mut impl Read, what: &str) -> Result<u64, Failure> {
    let mut buf = [0u8; 8];
    read_exact_or_reject(reader, &mut buf, what)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_root<R: From<[u8; 32]>>(reader: &mut impl Read, what: &str) -> Result<R, Failure> {
    let mut buf = [0u8; 32];
    read_exact_or_reject(reader, &mut buf, what)?;
    Ok(R::from(buf))
}

fn read_bounded_bytes(
    reader: &mut impl Read,
    len: u64,
    per_entry_limit: usize,
    total: &mut usize,
    total_limit: usize,
    what: &str,
) -> Result<Vec<u8>, Failure> {
    if len > per_entry_limit as u64 {
        return Err(Failure::InputLimit(format!(
            "{what} length {len} exceeds {per_entry_limit} bytes"
        )));
    }
    let len = len as usize;
    let next_total = total.checked_add(len).ok_or_else(|| {
        Failure::InputLimit(format!("total input bytes exceed {total_limit} bytes"))
    })?;
    if next_total > total_limit {
        return Err(Failure::InputLimit(format!(
            "total {what} bytes {next_total} exceed {total_limit} bytes"
        )));
    }
    *total = next_total;
    let mut buf = Vec::new();
    buf.try_reserve_exact(len)
        .map_err(|_| Failure::Unavailable("out of memory while reserving input buffer".into()))?;
    buf.resize(len, 0);
    read_exact_or_reject(reader, &mut buf, what)?;
    Ok(buf)
}

fn read_history_string(
    reader: &mut impl Read,
    history_bytes: &mut usize,
    what: &str,
) -> Result<String, Failure> {
    let len = read_u32(reader, what)? as usize;
    let added = 4usize.checked_add(len).ok_or_else(|| {
        Failure::InputLimit(format!(
            "history metadata exceeds {HISTORY_METADATA_LIMIT} bytes"
        ))
    })?;
    let next = history_bytes.checked_add(added).ok_or_else(|| {
        Failure::InputLimit(format!(
            "history metadata exceeds {HISTORY_METADATA_LIMIT} bytes"
        ))
    })?;
    if next > HISTORY_METADATA_LIMIT {
        return Err(Failure::InputLimit(format!(
            "history metadata size {next} exceeds {HISTORY_METADATA_LIMIT} bytes"
        )));
    }
    *history_bytes = next;
    let mut buf = vec![0u8; len];
    read_exact_or_reject(reader, &mut buf, what)?;
    String::from_utf8(buf)
        .map_err(|_| Failure::Rejected(format!("invalid UTF-8 in history {what}")))
}

pub fn read_wire_request<R: From<[u8; 32]> + Copy>(
    mut reader: impl Read,
    mut consume: impl FnMut(R, Vec<u8>) -> Result<(), Failure>,
) -> Result<(R, Vec<WireHistory<R>>), Failure> {
    let mut magic = [0u8; 4];
    read_exact_or_reject(&mut reader, &mut magic, "request magic")?;
    if &magic != WIRE_MAGIC {
        return Err(Failure::Rejected(
            "invalid check_library request magic".into(),
        ));
    }
    let head_root = read_root(&mut reader, "head root")?;
    let head_len = read_u64(&mut reader, "head length")?;
    let mut total_content = 0usize;
    let head_bytes = read_bounded_bytes(
        &mut reader,
        head_len,
        CONTENT_LIMIT,
        &mut total_content,
        ARCHIVE_LIMIT,
        "head content",
    )?;
    consume(head_root, head_bytes)?;
    let closure_count = read_u32(&mut reader, "closure count")? as usize;
    if closure_count >= PACKAGE_LIMIT {
        return Err(Failure::InputLimit(format!(
            "package count {} exceeds limit of {PACKAGE_LIMIT}",
            closure_count.saturating_add(1)
        )));
    }
    for index in 0..closure_count {
        let root = read_root(&mut reader, "closure root")?;
        let len = read_u64(&mut reader, "closure content length")?;
        let bytes = read_bounded_bytes(
            &mut reader,
            len,
            CONTENT_LIMIT,
            &mut total_content,
            ARCHIVE_LIMIT,
            &format!("closure[{index}] content"),
        )?;
        consume(root, bytes)?;
    }
    let history_count = read_u32(&mut reader, "history count")? as usize;
    let mut history_bytes = 4usize;
    let min_row_bytes = history_count.saturating_mul(44);
    if history_bytes.saturating_add(min_row_bytes) > HISTORY_METADATA_LIMIT {
        return Err(Failure::InputLimit(format!(
            "history count {history_count} exceeds {HISTORY_METADATA_LIMIT} byte metadata limit"
        )));
    }
    let mut history = Vec::with_capacity(history_count);
    for _ in 0..history_count {
        let origin = read_history_string(&mut reader, &mut history_bytes, "origin")?;
        let identity = read_history_string(&mut reader, &mut history_bytes, "identity")?;
        let version = read_history_string(&mut reader, &mut history_bytes, "version")?;
        history_bytes = history_bytes.checked_add(32).ok_or_else(|| {
            Failure::InputLimit(format!(
                "history metadata exceeds {HISTORY_METADATA_LIMIT} bytes"
            ))
        })?;
        if history_bytes > HISTORY_METADATA_LIMIT {
            return Err(Failure::InputLimit(format!(
                "history metadata exceeds {HISTORY_METADATA_LIMIT} bytes"
            )));
        }
        let root = read_root(&mut reader, "history root")?;
        history.push(WireHistory {
            origin,
            identity,
            version,
            root,
        });
    }
    let mut trailing = [0u8; 1];
    match reader.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => {
            return Err(Failure::Rejected(
                "trailing bytes in check_library request".into(),
            ));
        }
        Err(error) => {
            return Err(Failure::Rejected(format!(
                "failed reading end of request: {error}"
            )));
        }
    }
    Ok((head_root, history))
}

pub(super) fn write_wire_request(
    writer: &mut impl Write,
    request: &CheckLibraryRequest<'_>,
) -> std::io::Result<()> {
    writer.write_all(WIRE_MAGIC)?;
    writer.write_all(&request.root.0)?;
    writer.write_all(&(request.content.len() as u64).to_le_bytes())?;
    writer.write_all(request.content)?;
    writer.write_all(&(request.closure.len() as u32).to_le_bytes())?;
    for entry in request.closure {
        writer.write_all(&entry.root.0)?;
        writer.write_all(&(entry.content.len() as u64).to_le_bytes())?;
        writer.write_all(entry.content)?;
    }
    writer.write_all(&(request.history.len() as u32).to_le_bytes())?;
    for row in request.history {
        writer.write_all(&(row.origin.len() as u32).to_le_bytes())?;
        writer.write_all(row.origin.as_bytes())?;
        writer.write_all(&(row.identity.len() as u32).to_le_bytes())?;
        writer.write_all(row.identity.as_bytes())?;
        writer.write_all(&(row.version.len() as u32).to_le_bytes())?;
        writer.write_all(row.version.as_bytes())?;
        writer.write_all(&row.root.0)?;
    }
    writer.flush()
}

/// Format a [`serde_json::Value`] as RFC 8785 canonical UTF-8 JSON with no
/// trailing newline.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => {
            if let Some(u) = number.as_u64() {
                let _ = write!(out, "{u}");
            } else if let Some(i) = number.as_i64() {
                let _ = write!(out, "{i}");
            } else {
                out.push_str(&number.to_string());
            }
        }
        Value::String(text) => write_canonical_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut entries: Vec<(&str, &Value)> =
                map.iter().map(|(k, v)| (k.as_str(), v)).collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            for (index, (key, val)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical_string(key, out);
                out.push(':');
                write_canonical(val, out);
            }
            out.push('}');
        }
    }
}

fn write_canonical_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{09}' => out.push_str("\\t"),
            '\u{0a}' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0d}' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
