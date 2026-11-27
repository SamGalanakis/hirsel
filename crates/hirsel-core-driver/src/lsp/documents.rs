use bytes::Bytes;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Default)]
pub(super) enum Encoding {
    Utf8,
    #[default]
    Utf16,
}

impl Encoding {
    pub(super) fn negotiate(params: &Value) -> Self {
        params
            .pointer("/capabilities/general/positionEncodings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find_map(|value| match value.as_str()? {
                "utf-8" => Some(Self::Utf8),
                "utf-16" => Some(Self::Utf16),
                _ => None,
            })
            .unwrap_or_default()
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16 => "utf-16",
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
pub(super) struct Position {
    line: usize,
    character: usize,
}

#[derive(Clone, Copy, Deserialize)]
struct Range {
    start: Position,
    end: Position,
}

#[derive(Deserialize)]
pub(super) struct Change {
    range: Option<Range>,
    text: String,
}

#[derive(Clone)]
pub(super) struct Text {
    pub(super) bytes: Bytes,
    lines: Arc<[(usize, usize)]>,
}

impl Text {
    pub(super) fn new(bytes: Bytes) -> Self {
        let mut lines = Vec::new();
        let mut start = 0;
        let mut at = 0;
        while let Some(&byte) = bytes.get(at) {
            if matches!(byte, b'\r' | b'\n') {
                lines.push((start, at));
                at += usize::from(byte == b'\r' && bytes.get(at + 1) == Some(&b'\n'));
                start = at + 1;
            }
            at += 1;
        }
        lines.push((start, bytes.len()));
        Self {
            bytes,
            lines: lines.into(),
        }
    }

    fn offset(&self, position: Position, encoding: Encoding) -> Option<usize> {
        let (start, end) = *self.lines.get(position.line)?;
        let line = std::str::from_utf8(&self.bytes[start..end]).ok()?;
        match encoding {
            Encoding::Utf8 => {
                let offset = position.character.min(line.len());
                line.is_char_boundary(offset).then_some(start + offset)
            }
            Encoding::Utf16 => {
                let mut units = 0;
                for (offset, ch) in line.char_indices() {
                    if units == position.character {
                        return Some(start + offset);
                    }
                    units += ch.len_utf16();
                    if units > position.character {
                        return None;
                    }
                }
                Some(end)
            }
        }
    }

    pub(super) fn position(&self, offset: u32, encoding: Encoding) -> Option<Value> {
        let offset = offset as usize;
        if offset > self.bytes.len() {
            return None;
        }
        let line = self.lines.partition_point(|&(start, _)| start <= offset) - 1;
        let (start, end) = self.lines[line];
        let prefix = &self.bytes[start..offset.min(end)];
        let character = match encoding {
            Encoding::Utf8 => prefix.len(),
            Encoding::Utf16 => String::from_utf8_lossy(prefix).encode_utf16().count(),
        };
        Some(json!({"line": line, "character": character}))
    }
}

#[derive(Clone)]
pub(super) struct Document {
    pub(super) uri: String,
    pub(super) version: i64,
    pub(super) text: Text,
}

impl Document {
    pub(super) fn open(uri: String, version: i64, text: String) -> Self {
        Self {
            uri,
            version,
            text: Text::new(text.into()),
        }
    }

    pub(super) fn change(
        &mut self,
        version: i64,
        changes: Vec<Change>,
        encoding: Encoding,
    ) -> Option<()> {
        if version <= self.version {
            return None;
        }
        let mut text = self.text.clone();
        for change in changes {
            text = match change.range {
                None => Text::new(change.text.into()),
                Some(range) => {
                    let start = text.offset(range.start, encoding)?;
                    let end = text.offset(range.end, encoding)?;
                    if start > end {
                        return None;
                    }
                    let mut edited =
                        Vec::with_capacity(text.bytes.len() - (end - start) + change.text.len());
                    edited.extend_from_slice(&text.bytes[..start]);
                    edited.extend_from_slice(change.text.as_bytes());
                    edited.extend_from_slice(&text.bytes[end..]);
                    Text::new(edited.into())
                }
            }
        }
        self.version = version;
        self.text = text;
        Some(())
    }
}

// A local file URI is indexed by its decoded absolute path. The original spelling
// is retained for publication to the client that opened it.
pub(super) type SourceOverlay = BTreeMap<PathBuf, Document>;

pub(super) fn file_path(uri: &str) -> Option<PathBuf> {
    let uri = uri.strip_prefix("file://")?;
    let path = uri.strip_prefix("localhost").unwrap_or(uri);
    if !path.starts_with('/') || path.contains(['?', '#']) {
        return None;
    }
    let mut decoded = Vec::new();
    let mut rest = path.as_bytes();
    while let Some((&first, tail)) = rest.split_first() {
        rest = tail;
        if first == b'%' {
            let digits = rest.get(..2)?;
            if !digits.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            let hex = std::str::from_utf8(digits).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            if matches!(byte, 0 | b'/') {
                return None;
            }
            decoded.push(byte);
            rest = &rest[2..];
        } else {
            if first == 0 {
                return None;
            }
            decoded.push(first);
        }
    }
    let path = PathBuf::from(OsString::from_vec(decoded));
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::RootDir | Component::Normal(_) => result.push(part),
            Component::CurDir => (),
            _ => return None,
        }
    }
    Some(result)
}

pub(super) fn file_uri(path: &Path) -> String {
    use std::fmt::Write;

    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            uri.push(char::from(byte));
        } else {
            write!(uri, "%{byte:02X}").unwrap();
        }
    }
    uri
}
