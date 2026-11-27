//! Shared C embed ABI v1 surface and EMB-5 manifest binding projection.
//!
//! Single source of truth for `hirsel_embed.h`, exported v1 symbol names,
//! binding-projection canonicalization, and manifest entry metadata used by
//! both `hirsel-core-rt` (embed slot) and `hirsel-core-driver` (header/stub emission).
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Current C embed ABI version (`HIRSEL_EMBED_ABI`).
pub const ABI_VERSION: u32 = 1;

/// Exact C symbol names exported by the embed runtime for ABI v1 (EMB-4).
pub const EXPORTS_V1: &[&str] = &[
    "hirsel_embed_v1_abi",
    "hirsel_embed_v1_manifest",
    "hirsel_embed_v1_start",
    "hirsel_embed_v1_open",
    "hirsel_embed_v1_call",
    "hirsel_embed_v1_abandon",
    "hirsel_embed_v1_subscribe",
    "hirsel_embed_v1_unsubscribe",
    "hirsel_embed_v1_room_join",
    "hirsel_embed_v1_room_set",
    "hirsel_embed_v1_room_send",
    "hirsel_embed_v1_room_leave",
    "hirsel_embed_v1_close",
    "hirsel_embed_v1_stop",
];

/// Canonical `hirsel_embed.h` header for ABI v1 (EMB-4).
pub const HEADER_V1: &str = r#"#ifndef HIRSEL_EMBED_H
#define HIRSEL_EMBED_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define HIRSEL_EMBED_ABI 1

typedef struct { uint64_t generation; } hirsel_runtime;                    /* 0: none */
typedef struct { uint64_t generation; uint64_t session; } hirsel_session;  /* 0: none */
typedef struct { const uint8_t *ptr; size_t len; } hirsel_bytes;           /* ptr may be NULL only if len is 0 */

typedef enum {
  HIRSEL_OK = 0,
  HIRSEL_ABI_MISMATCH = 1,    /* the arguments break ABI v1: abi, fingerprint, size, tail or a NULL required field */
  HIRSEL_REFUSED = 2,         /* start refused; config.ended has run once with the reason */
  HIRSEL_STOPPED = 3,         /* the handle's runtime or session is not live */
  HIRSEL_UNKNOWN_ENTRY = 4,   /* want out of range, or the wrong kind for this function */
  HIRSEL_BUSY = 5,            /* stop called on the delivery thread */
  HIRSEL_ALREADY_RUNNING = 6, /* the runtime is starting, running, ended or stopping */
  HIRSEL_ID_IN_USE = 7,       /* the call, sub or room id is live in this session */
  HIRSEL_DEFERRED = 8,        /* close called on the delivery thread; session_ended follows */
  HIRSEL_NOT_LIVE = 9,        /* the id names no live call, sub or room */
  HIRSEL_FULL = 10,           /* host-client storage cannot take this request; nothing was sent */
  HIRSEL_HOST_MEMORY = 11,    /* the C allocator refused the copy; nothing was sent */
  HIRSEL_OVER_BOUND = 12      /* a room state or message exceeds the room's registered bound */
} hirsel_status;

typedef enum {
  HIRSEL_END_STOPPED = 0,     /* the host called stop */
  HIRSEL_END_REFUSED = 1,     /* start refused; reason is the refusal */
  HIRSEL_END_PLACE_FAILED = 2,/* the place ended with Fail; reason is its failure text */
  HIRSEL_END_RUN_STOP = 3     /* a FAIL-15 run stop; reason is the stop's name */
} hirsel_end_code;

typedef struct {
  uint32_t size;              /* sizeof(hirsel_embed_callbacks) the caller was built with */
  void *context;              /* passed back unread; retained until session_ended returns */
  void (*outcome)(void *context, uint64_t call, hirsel_bytes outcome);
  void (*change)(void *context, uint64_t sub, hirsel_bytes change);
  void (*sub_ended)(void *context, uint64_t sub, hirsel_bytes end);
  void (*room_event)(void *context, uint64_t room, hirsel_bytes event);
  void (*session_ended)(void *context, hirsel_bytes end);
} hirsel_embed_callbacks;

typedef struct {
  uint32_t size;              /* sizeof(hirsel_embed_config) the caller was built with */
  uint32_t abi;               /* HIRSEL_EMBED_ABI the caller was generated for */
  hirsel_bytes manifest_fp;   /* SHA-256 of the binding projection (EMB-5) */
  hirsel_bytes digest;        /* 32 bytes or empty (EMB-3) */
  uint64_t n_place;
  uint64_t client_bytes;      /* host-client storage bound */
  uint32_t start_timeout_ms;  /* positive ms bound on start reaching Ready; 0 is HIRSEL_ABI_MISMATCH */
  hirsel_bytes grants;        /* UTF-8 grant text */
  hirsel_bytes dir_bases;     /* UTF-8 base lines (EMB-3) */
  hirsel_bytes store_dir;     /* UTF-8 absolute path */
  hirsel_bytes exec_helper;   /* absolute path of hirsel-exec-helper, or empty (EMB-3) */
  void *context;              /* passed back unread; retained until ended returns */
  void (*ended)(void *context, uint32_t code, hirsel_bytes reason);
} hirsel_embed_config;

uint32_t hirsel_embed_v1_abi(void);
hirsel_bytes hirsel_embed_v1_manifest(void);
hirsel_status hirsel_embed_v1_start(const hirsel_embed_config *, hirsel_runtime *out);
hirsel_status hirsel_embed_v1_open(hirsel_runtime, const hirsel_embed_callbacks *,
                                   hirsel_bytes credential, hirsel_session *out);
hirsel_status hirsel_embed_v1_call(hirsel_session, uint64_t call, uint32_t want,
                                   hirsel_bytes args, uint32_t deadline_ms);
hirsel_status hirsel_embed_v1_abandon(hirsel_session, uint64_t call);
hirsel_status hirsel_embed_v1_subscribe(hirsel_session, uint64_t sub, uint32_t want,
                                        hirsel_bytes args);
hirsel_status hirsel_embed_v1_unsubscribe(hirsel_session, uint64_t sub);
hirsel_status hirsel_embed_v1_room_join(hirsel_session, uint64_t room, uint32_t want,
                                        hirsel_bytes args);
hirsel_status hirsel_embed_v1_room_set(hirsel_session, uint64_t room, uint32_t present,
                                       hirsel_bytes state);
hirsel_status hirsel_embed_v1_room_send(hirsel_session, uint64_t room, uint64_t to,
                                        hirsel_bytes message);   /* to = 0: whole room */
hirsel_status hirsel_embed_v1_room_leave(hirsel_session, uint64_t room);
hirsel_status hirsel_embed_v1_close(hirsel_session);
hirsel_status hirsel_embed_v1_stop(hirsel_runtime);

#ifdef __cplusplus
}
#endif

#endif /* HIRSEL_EMBED_H */
"#;

/// Kind of an exported place entry in the schema manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Compute,
    Transact,
    Query,
    Granted,
    Room,
}

impl EntryKind {
    pub fn is_callable(self) -> bool {
        matches!(self, Self::Compute | Self::Transact | Self::Granted)
    }

    pub fn is_subscribable(self) -> bool {
        matches!(self, Self::Query)
    }

    pub fn is_room(self) -> bool {
        matches!(self, Self::Room)
    }
}

/// Room bounds declared in the manifest for a `Room` entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoomLimits {
    pub peers: u32,
    pub state_max: u64,
    pub message_max: u64,
}

/// Parsed metadata for one entry in the manifest's `entries` array.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestEntry {
    pub index: u32,
    pub kind: EntryKind,
    pub identity: Vec<u8>,
    pub args_fp: u64,
    pub result_fp: u64,
    pub slot_count: u32,
    pub room: Option<RoomLimits>,
}

/// Parsed manifest information needed by the embed runtime client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedManifest {
    pub fingerprint: [u8; 32],
    pub entries: Vec<ManifestEntry>,
}

/// Compute the canonical EMB-5 binding projection of a schema manifest JSON document:
/// strips top-level `launch` and each entry's `revision`, keeps `version: 1`, `tables`,
/// and `entries`, and serializes compact JSON with lexicographically sorted keys and a
/// trailing `\n`.
pub fn binding_projection(manifest_json: &[u8]) -> Result<Vec<u8>, String> {
    let text =
        std::str::from_utf8(manifest_json).map_err(|_| "manifest is not UTF-8".to_owned())?;
    let mut root = parse_json(text)?;
    project_value(&mut root)?;
    let mut out = Vec::with_capacity(manifest_json.len());
    write_canonical(&root, &mut out);
    out.push(b'\n');
    Ok(out)
}

/// SHA-256 of [`binding_projection`] (EMB-5 `manifest_fp`).
pub fn manifest_fingerprint(manifest_json: &[u8]) -> Result<[u8; 32], String> {
    let projected = binding_projection(manifest_json)?;
    Ok(Sha256::digest(&projected).into())
}

/// Parse the manifest's `entries` array and compute its EMB-5 fingerprint.
pub fn parse_manifest(manifest_json: &[u8]) -> Result<ParsedManifest, String> {
    let text =
        std::str::from_utf8(manifest_json).map_err(|_| "manifest is not UTF-8".to_owned())?;
    let mut root = parse_json(text)?;
    project_value(&mut root)?;
    let mut projected = Vec::with_capacity(manifest_json.len());
    write_canonical(&root, &mut projected);
    projected.push(b'\n');
    let fingerprint: [u8; 32] = Sha256::digest(&projected).into();

    let JsonValue::Object(map) = &root else {
        return Err("manifest root is not an object".into());
    };
    let Some(JsonValue::Array(raw_entries)) = map.get("entries") else {
        return Err("manifest missing entries array".into());
    };
    let mut entries = Vec::with_capacity(raw_entries.len());
    for (idx, item) in raw_entries.iter().enumerate() {
        let index = u32::try_from(idx).map_err(|_| "too many manifest entries".to_owned())?;
        let JsonValue::Object(obj) = item else {
            return Err(format!("manifest entry {idx} is not an object"));
        };
        let kind = match obj.get("kind") {
            Some(JsonValue::String(s)) => match s.as_str() {
                "compute" => EntryKind::Compute,
                "transact" => EntryKind::Transact,
                "query" => EntryKind::Query,
                "granted" => EntryKind::Granted,
                "room" => EntryKind::Room,
                other => return Err(format!("manifest entry {idx} has unknown kind {other:?}")),
            },
            _ => return Err(format!("manifest entry {idx} missing kind")),
        };
        let identity = parse_bytes_field(obj.get("identity"), idx)?;
        let args_fp = parse_u64_field(obj.get("args_fp"), "args_fp", idx)?;
        let result_fp = parse_u64_field(obj.get("result_fp"), "result_fp", idx)?;
        let slot_count = match obj.get("slots") {
            Some(JsonValue::Array(slots)) => {
                u32::try_from(slots.len()).map_err(|_| format!("entry {idx} slots overflow"))?
            }
            None => 0,
            _ => return Err(format!("manifest entry {idx} slots is not an array")),
        };
        let room = if kind == EntryKind::Room {
            let Some(JsonValue::Object(r)) = obj.get("room") else {
                return Err(format!("room entry {idx} missing room object"));
            };
            let peers = parse_u64_field(r.get("peers"), "room.peers", idx)?;
            let peers =
                u32::try_from(peers).map_err(|_| format!("room entry {idx} peers out of range"))?;
            let state_max = parse_u64_field(r.get("state_max"), "room.state_max", idx)?;
            let message_max = parse_u64_field(r.get("message_max"), "room.message_max", idx)?;
            Some(RoomLimits {
                peers,
                state_max,
                message_max,
            })
        } else {
            None
        };
        entries.push(ManifestEntry {
            index,
            kind,
            identity,
            args_fp,
            result_fp,
            slot_count,
            room,
        });
    }
    Ok(ParsedManifest {
        fingerprint,
        entries,
    })
}

fn project_value(root: &mut JsonValue) -> Result<(), String> {
    let JsonValue::Object(obj) = root else {
        return Err("manifest root must be a JSON object".into());
    };
    match obj.get("version") {
        Some(JsonValue::Number(n)) if n == "1" => {}
        _ => return Err("manifest version must be 1".into()),
    }
    obj.remove("launch");
    let Some(JsonValue::Array(entries)) = obj.get_mut("entries") else {
        return Err("manifest missing entries array".into());
    };
    for (i, entry) in entries.iter_mut().enumerate() {
        let JsonValue::Object(e) = entry else {
            return Err(format!("manifest entry {i} must be an object"));
        };
        e.remove("revision");
    }
    if !obj.contains_key("tables") {
        return Err("manifest missing tables".into());
    }
    Ok(())
}

fn parse_bytes_field(val: Option<&JsonValue>, idx: usize) -> Result<Vec<u8>, String> {
    match val {
        Some(JsonValue::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let JsonValue::Number(n) = item else {
                    return Err(format!(
                        "manifest entry {idx} identity byte is not a number"
                    ));
                };
                let b: u8 = n
                    .parse()
                    .map_err(|_| format!("manifest entry {idx} identity byte out of u8 range"))?;
                out.push(b);
            }
            Ok(out)
        }
        Some(JsonValue::String(s)) => crate::hex::decode(s)
            .ok_or_else(|| format!("manifest entry {idx} identity is invalid hex")),
        _ => Err(format!("manifest entry {idx} missing identity")),
    }
}

fn parse_u64_field(val: Option<&JsonValue>, name: &str, idx: usize) -> Result<u64, String> {
    match val {
        Some(JsonValue::String(s)) => s
            .parse::<u64>()
            .map_err(|_| format!("manifest entry {idx} invalid {name}")),
        Some(JsonValue::Number(n)) => n
            .parse::<u64>()
            .map_err(|_| format!("manifest entry {idx} invalid {name}")),
        _ => Err(format!("manifest entry {idx} missing {name}")),
    }
}

// ---------------------------------------------------------------------------
// Minimal strict RFC 8259 JSON parser and canonical serializer (zero deps)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum JsonValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    depth: usize,
}

const MAX_DEPTH: usize = 128;

fn parse_json(text: &str) -> Result<JsonValue, String> {
    let mut p = Parser {
        src: text.as_bytes(),
        pos: 0,
        depth: 0,
    };
    let value = p.parse_value()?;
    p.skip_ws();
    if p.pos != p.src.len() {
        return Err(format!("trailing JSON bytes at offset {}", p.pos));
    }
    Ok(value)
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while let Some(&b) = self.src.get(self.pos) {
            if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn parse_value(&mut self) -> Result<JsonValue, String> {
        self.skip_ws();
        let b = self
            .peek()
            .ok_or_else(|| "unexpected EOF in JSON".to_owned())?;
        match b {
            b'n' => {
                self.expect_bytes(b"null")?;
                Ok(JsonValue::Null)
            }
            b't' => {
                self.expect_bytes(b"true")?;
                Ok(JsonValue::Bool(true))
            }
            b'f' => {
                self.expect_bytes(b"false")?;
                Ok(JsonValue::Bool(false))
            }
            b'"' => Ok(JsonValue::String(self.parse_string()?)),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            b'-' | b'0'..=b'9' => Ok(JsonValue::Number(self.parse_number()?)),
            _ => Err(format!("unexpected byte 0x{b:02x} at offset {}", self.pos)),
        }
    }

    fn expect_bytes(&mut self, expected: &[u8]) -> Result<(), String> {
        if self.src.get(self.pos..self.pos + expected.len()) == Some(expected) {
            self.pos += expected.len();
            Ok(())
        } else {
            Err(format!("expected token at offset {}", self.pos))
        }
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("JSON nesting too deep".into());
        }
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(JsonValue::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(JsonValue::Array(items));
                }
                _ => return Err(format!("expected ',' or ']' at offset {}", self.pos)),
            }
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("JSON nesting too deep".into());
        }
        self.pos += 1; // '{'
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(JsonValue::Object(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!("expected object key string at offset {}", self.pos));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(format!("expected ':' at offset {}", self.pos));
            }
            self.pos += 1;
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(JsonValue::Object(map));
                }
                _ => return Err(format!("expected ',' or '}}' at offset {}", self.pos)),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.pos += 1; // '"'
        let mut out = String::new();
        while let Some(&b) = self.src.get(self.pos) {
            if b == b'"' {
                self.pos += 1;
                return Ok(out);
            }
            if b < 0x20 {
                return Err(format!("unescaped control char at offset {}", self.pos));
            }
            if b == b'\\' {
                self.pos += 1;
                let esc = *self
                    .src
                    .get(self.pos)
                    .ok_or_else(|| "truncated escape".to_owned())?;
                self.pos += 1;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\x08'),
                    b'f' => out.push('\x0c'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let code = self.parse_hex4()?;
                        if (0xD800..=0xDBFF).contains(&code) {
                            if self.src.get(self.pos..self.pos + 2) != Some(b"\\u") {
                                return Err("unpaired high surrogate in JSON".into());
                            }
                            self.pos += 2;
                            let low = self.parse_hex4()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return Err("invalid low surrogate in JSON".into());
                            }
                            let cp = 0x10000
                                + (((u32::from(code) - 0xD800) << 10) | (u32::from(low) - 0xDC00));
                            let ch = char::from_u32(cp)
                                .ok_or_else(|| "invalid unicode scalar".to_owned())?;
                            out.push(ch);
                        } else if (0xDC00..=0xDFFF).contains(&code) {
                            return Err("unpaired low surrogate in JSON".into());
                        } else {
                            let ch = char::from_u32(u32::from(code))
                                .ok_or_else(|| "invalid unicode scalar".to_owned())?;
                            out.push(ch);
                        }
                    }
                    _ => return Err(format!("invalid escape '\\{}'", esc as char)),
                }
            } else {
                let start = self.pos;
                while let Some(&c) = self.src.get(self.pos) {
                    if c == b'"' || c == b'\\' || c < 0x20 {
                        break;
                    }
                    self.pos += 1;
                }
                let chunk = std::str::from_utf8(&self.src[start..self.pos])
                    .map_err(|_| "invalid UTF-8 in JSON string".to_owned())?;
                out.push_str(chunk);
            }
        }
        Err("unterminated JSON string".into())
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        let slice = self
            .src
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| "truncated \\u escape".to_owned())?;
        self.pos += 4;
        let s = std::str::from_utf8(slice).map_err(|_| "invalid \\u escape".to_owned())?;
        u16::from_str_radix(s, 16).map_err(|_| "invalid \\u hex digits".to_owned())
    }

    fn parse_number(&mut self) -> Result<String, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.pos += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(format!("invalid JSON number at offset {}", self.pos)),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(format!("invalid JSON fraction at offset {}", self.pos));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(format!("invalid JSON exponent at offset {}", self.pos));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        Ok(std::str::from_utf8(&self.src[start..self.pos])
            .unwrap()
            .to_owned())
    }
}

fn write_canonical(value: &JsonValue, out: &mut Vec<u8>) {
    match value {
        JsonValue::Null => out.extend_from_slice(b"null"),
        JsonValue::Bool(true) => out.extend_from_slice(b"true"),
        JsonValue::Bool(false) => out.extend_from_slice(b"false"),
        JsonValue::Number(n) => out.extend_from_slice(n.as_bytes()),
        JsonValue::String(s) => write_json_string(s, out),
        JsonValue::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        JsonValue::Object(map) => {
            out.push(b'{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_json_string(k, out);
                out.push(b':');
                write_canonical(v, out);
            }
            out.push(b'}');
        }
    }
}

fn write_json_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\x08' => out.extend_from_slice(b"\\b"),
            '\x0c' => out.extend_from_slice(b"\\f"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                let hex = format!("\\u{code:04x}");
                out.extend_from_slice(hex.as_bytes());
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_projection_strips_launch_and_revision_and_sorts_keys() {
        let a = br#"{"version":1,"launch":{"digest":[1,2],"target":"x86_64-unknown-linux-gnu","n_place":"16777216","revision":[3]},"tables":[{"name":"items","id":1,"owner":0,"kind":0}],"entries":[{"name":"inc","module":["main"],"kind":"transact","identity":[10,11],"revision":[9,8],"args_fp":"100","result_fp":"200","args":{"root":[0],"definitions":[],"key":false},"result":{"root":[0],"definitions":[],"key":false},"slots":[]}]}"#;
        let b = br#"{"entries":[{"slots":[],"result":{"key":false,"definitions":[],"root":[0]},"args":{"definitions":[],"key":false,"root":[0]},"result_fp":"200","args_fp":"100","revision":[77,78],"identity":[10,11],"kind":"transact","module":["main"],"name":"inc"}],"tables":[{"kind":0,"owner":0,"id":1,"name":"items"}],"launch":{"digest":[99],"target":"aarch64-apple-ios","n_place":"999","revision":[5]},"version":1}"#;
        assert_eq!(
            binding_projection(a).unwrap(),
            binding_projection(b).unwrap()
        );
        assert_eq!(
            manifest_fingerprint(a).unwrap(),
            manifest_fingerprint(b).unwrap()
        );
        let projected = String::from_utf8(binding_projection(a).unwrap()).unwrap();
        assert!(projected.ends_with('\n'));
        assert!(!projected.contains("\"launch\""));
        assert!(!projected.contains("\"revision\""));
    }

    #[test]
    fn parse_manifest_reads_entries_and_room_limits() {
        let json = br#"{"version":1,"launch":{},"tables":[],"entries":[{"module":["m"],"name":"f","kind":"transact","identity":[1,2],"revision":[],"args_fp":"11","result_fp":"22","args":{},"result":{},"slots":[]},{"module":["m"],"name":"r","kind":"room","identity":[3,4],"revision":[],"args_fp":"33","result_fp":"44","args":{},"result":{},"slots":[{},{}],"room":{"peers":8,"state_max":256,"message_max":512,"state":{},"message":{}}}]}"#;
        let parsed = parse_manifest(json).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].kind, EntryKind::Transact);
        assert_eq!(parsed.entries[0].identity, vec![1, 2]);
        assert_eq!(parsed.entries[0].args_fp, 11);
        assert_eq!(parsed.entries[0].result_fp, 22);
        assert_eq!(parsed.entries[1].kind, EntryKind::Room);
        assert_eq!(parsed.entries[1].slot_count, 2);
        assert_eq!(
            parsed.entries[1].room,
            Some(RoomLimits {
                peers: 8,
                state_max: 256,
                message_max: 512
            })
        );
    }

    #[test]
    fn header_lists_every_v1_export() {
        for sym in EXPORTS_V1 {
            assert!(HEADER_V1.contains(sym), "header missing {sym}");
        }
        assert_eq!(EXPORTS_V1.len(), 14);
    }
}
