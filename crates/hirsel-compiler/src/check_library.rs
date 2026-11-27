//! Confined registry library admission (`docs/spec/registry.md` §2).
//!
//! Runs the `hirsel` worker under OS confinement with cleared environment and
//! descriptors, bounded binary length-delimited stdin request, bounded RFC 8785
//! JSON stdout reply, and bounded stderr diagnostics.

use crate::{Job, Limit, MEMORY_STATUS, Refusal, Service, host, unavailable};
use hirsel_core_ids::{ContentHash, PackageId};
use serde::de::{self, SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const WORKER_ARG: &str = "__check-library";
const WIRE_MAGIC: &[u8; 4] = b"HCL1";
const UNSUPPORTED_PREFIX: &str = "check_library: unsupported: ";
const LIMIT_INPUT_PREFIX: &str = "check_library: limit: input: ";
const LIMIT_OUTPUT_PREFIX: &str = "check_library: limit: output: ";

/// Resource and input caps for registry `check_library` admission (`registry.md` §2).
/// Separate from `Caps::HOST` (16 MiB module compile default).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckLibraryCaps {
    pub cpu_seconds: u64,
    pub memory_bytes: u64,
    pub wall_ms: u64,
    pub max_raw_content_bytes: u64,
    pub max_total_content_bytes: u64,
    pub max_packages: usize,
    pub max_history_metadata_bytes: u64,
    pub output_bytes: u64,
    pub diagnostics_bytes: usize,
}

impl CheckLibraryCaps {
    /// Normative registry admission caps from `docs/spec/registry.md` §2.
    pub const REGISTRY: Self = Self {
        cpu_seconds: 10,
        memory_bytes: 512 << 20,
        wall_ms: 20_000,
        max_raw_content_bytes: 64 << 20,
        max_total_content_bytes: 512 << 20,
        max_packages: 4_096,
        max_history_metadata_bytes: 64 << 20,
        output_bytes: 64 << 20,
        diagnostics_bytes: 64 << 10,
    };
}

/// One closure entry in a [`CheckLibraryRequest`]: its claimed Phase 0 library
/// root and expanded canonical `C` bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LibraryEntry<'a> {
    pub root: ContentHash,
    pub content: &'a [u8],
}

/// One registry-verified release history tuple for a child package (`registry.md` §2).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRow {
    pub origin: String,
    pub identity: String,
    pub version: String,
    pub root: ContentHash,
}

/// Input to [`Service::check_library`].
#[derive(Clone, Copy, Debug)]
pub struct CheckLibraryRequest<'a> {
    pub root: ContentHash,
    pub content: &'a [u8],
    pub closure: &'a [LibraryEntry<'a>],
    pub history: &'a [HistoryRow],
}

/// Checked library header returned in [`CheckedLibrary`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedHeader {
    pub identity: String,
    pub version: String,
    pub charge: String,
}

/// One dependency binding in canonical `C` order (`registry.md` §2):
/// `[alias, id, "toolchain"] | [alias, id, "exact", tested_root] | [alias, id, "floor", tested_root, bound_version]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckedBinding {
    Toolchain {
        alias: String,
        id: String,
    },
    Exact {
        alias: String,
        id: String,
        tested_root: String,
    },
    Floor {
        alias: String,
        id: String,
        tested_root: String,
        bound_version: String,
    },
}

impl Serialize for CheckedBinding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Toolchain { alias, id } => {
                let mut seq = serializer.serialize_seq(Some(3))?;
                seq.serialize_element(alias)?;
                seq.serialize_element(id)?;
                seq.serialize_element("toolchain")?;
                seq.end()
            }
            Self::Exact {
                alias,
                id,
                tested_root,
            } => {
                let mut seq = serializer.serialize_seq(Some(4))?;
                seq.serialize_element(alias)?;
                seq.serialize_element(id)?;
                seq.serialize_element("exact")?;
                seq.serialize_element(tested_root)?;
                seq.end()
            }
            Self::Floor {
                alias,
                id,
                tested_root,
                bound_version,
            } => {
                let mut seq = serializer.serialize_seq(Some(5))?;
                seq.serialize_element(alias)?;
                seq.serialize_element(id)?;
                seq.serialize_element("floor")?;
                seq.serialize_element(tested_root)?;
                seq.serialize_element(bound_version)?;
                seq.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for CheckedBinding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BindingVisitor;

        impl<'de> Visitor<'de> for BindingVisitor {
            type Value = CheckedBinding;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a binding tuple [alias, id, tag, ...]")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let alias: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let id: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(1, &self))?;
                let tag: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(2, &self))?;
                let binding = match tag.as_str() {
                    "toolchain" => CheckedBinding::Toolchain { alias, id },
                    "exact" => {
                        let tested_root: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(3, &self))?;
                        if parse_digest(&tested_root).is_none() {
                            return Err(de::Error::custom("invalid exact binding digest"));
                        }
                        CheckedBinding::Exact {
                            alias,
                            id,
                            tested_root,
                        }
                    }
                    "floor" => {
                        let tested_root: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(3, &self))?;
                        if parse_digest(&tested_root).is_none() {
                            return Err(de::Error::custom("invalid floor binding digest"));
                        }
                        let bound_version: String = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(4, &self))?;
                        CheckedBinding::Floor {
                            alias,
                            id,
                            tested_root,
                            bound_version,
                        }
                    }
                    other => {
                        return Err(de::Error::custom(format!("unknown binding tag {other:?}")));
                    }
                };
                if seq.next_element::<de::IgnoredAny>()?.is_some() {
                    return Err(de::Error::custom("trailing elements in binding tuple"));
                }
                Ok(binding)
            }
        }

        deserializer.deserialize_seq(BindingVisitor)
    }
}

/// Unfiltered `InterfaceV1` projection (`hirsel inspect --interface --json`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceV1 {
    pub version: u32,
    pub package: PackageId,
    pub modules: Vec<InterfaceModule>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceModule {
    pub path: Vec<String>,
    pub extends_core: bool,
    pub items: Vec<InterfaceItem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceItem {
    pub definition: String,
    pub name: String,
    pub kind: String,
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// §11 CoreIR authority card projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarriedAuthority {
    pub takes: Vec<String>,
    pub returns: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReachedAuthority {
    pub kinds: Vec<String>,
    pub provided: Vec<String>,
    pub contained: BTreeSet<String>,
    pub unresolved: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AuthorityVerdict {
    Status(String),
    Kinds(Vec<String>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityCard {
    pub carried: BTreeMap<String, CarriedAuthority>,
    pub reached: BTreeMap<String, ReachedAuthority>,
    pub verdict: AuthorityVerdict,
}

/// Checked library facts returned by [`Service::check_library`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedLibrary {
    pub root: String,
    pub header: CheckedHeader,
    pub bindings: Vec<CheckedBinding>,
    pub interface: InterfaceV1,
    pub authority: AuthorityCard,
}

impl CheckedLibrary {
    /// Serialize as RFC 8785 canonical UTF-8 JSON with no trailing newline.
    pub fn to_canonical_json(&self) -> Result<String, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        Ok(canonical_json(&value))
    }

    /// Serialize as RFC 8785 canonical UTF-8 JSON bytes with no trailing newline.
    pub fn canonical_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        Ok(self.to_canonical_json()?.into_bytes())
    }
}

/// Parse a 64-character lowercase hex digest (`registry.md` §1).
pub fn parse_digest(text: &str) -> Option<ContentHash> {
    if text.len() != 64 || text.bytes().any(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let bytes: [u8; 32] = hirsel_core_ids::hex::decode(text)?.try_into().ok()?;
    Some(ContentHash(bytes))
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

fn validate_input_limits(
    request: &CheckLibraryRequest<'_>,
    caps: CheckLibraryCaps,
) -> Result<(), Refusal> {
    let head_len = request.content.len() as u64;
    if head_len > caps.max_raw_content_bytes {
        return Err(Refusal::Limit(Limit::Input));
    }
    let packages = request
        .closure
        .len()
        .checked_add(1)
        .ok_or(Refusal::Limit(Limit::Input))?;
    if packages > caps.max_packages || request.closure.len() > u32::MAX as usize {
        return Err(Refusal::Limit(Limit::Input));
    }
    let mut total_content = head_len;
    for entry in request.closure {
        let len = entry.content.len() as u64;
        if len > caps.max_raw_content_bytes {
            return Err(Refusal::Limit(Limit::Input));
        }
        total_content = total_content
            .checked_add(len)
            .ok_or(Refusal::Limit(Limit::Input))?;
        if total_content > caps.max_total_content_bytes {
            return Err(Refusal::Limit(Limit::Input));
        }
    }
    if request.history.len() > u32::MAX as usize {
        return Err(Refusal::Limit(Limit::Input));
    }
    let mut history_bytes = 4u64;
    for row in request.history {
        if row.origin.len() > u32::MAX as usize
            || row.identity.len() > u32::MAX as usize
            || row.version.len() > u32::MAX as usize
        {
            return Err(Refusal::Limit(Limit::Input));
        }
        let row_size = 4u64
            .saturating_add(row.origin.len() as u64)
            .saturating_add(4)
            .saturating_add(row.identity.len() as u64)
            .saturating_add(4)
            .saturating_add(row.version.len() as u64)
            .saturating_add(32);
        history_bytes = history_bytes
            .checked_add(row_size)
            .ok_or(Refusal::Limit(Limit::Input))?;
        if history_bytes > caps.max_history_metadata_bytes {
            return Err(Refusal::Limit(Limit::Input));
        }
    }
    Ok(())
}

fn write_wire_request(
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

pub(crate) fn run(
    service: &Service,
    request: &CheckLibraryRequest<'_>,
    caps: CheckLibraryCaps,
) -> Result<CheckedLibrary, Refusal> {
    // 1. Validate all input caps before taking a seat, allocating, or spawning.
    validate_input_limits(request, caps)?;
    let _seat = service.seat()?;

    // 2. Create private per-job scratch directory (removed on drop; zero bytes
    //    persist on refusal).
    let job = Job(service.scratch.join(format!(
        "check-{}-{}",
        std::process::id(),
        service.jobs.fetch_add(1, Ordering::Relaxed)
    )));
    let runtime = job.0.join("run");
    let cache = job.0.join("cache");
    let data = job.0.join("data");
    let tmp = job.0.join("tmp");
    let setup = || -> std::io::Result<()> {
        for dir in [&job.0, &runtime, &cache, &data, &tmp] {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    };
    setup().map_err(unavailable)?;

    // 3. Prepare OS confinement before spawning; fail closed with Unavailable if
    //    confinement or resource caps cannot be enforced on this host.
    let spec = host::ConfinementSpec {
        worker: &service.worker,
        scratch: &job.0,
        cpu_seconds: caps.cpu_seconds,
        memory_bytes: caps.memory_bytes,
        file_bytes: caps.output_bytes.saturating_add(1),
    };
    let confinement = host::Confinement::prepare(&spec).map_err(unavailable)?;

    let mut command = confinement.command(WORKER_ARG);
    command
        .env_clear()
        .env("HIRSEL_MEMORY_LIMIT", caps.memory_bytes.to_string())
        .env("XDG_CACHE_HOME", &cache)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("HOME", &job.0)
        .env("TMPDIR", &tmp)
        .current_dir(&job.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

    let deadline = Instant::now() + Duration::from_millis(caps.wall_ms);
    let mut child = command.spawn().map_err(unavailable)?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let child_pid = child.id() as libc::pid_t;

    let output_limit = caps.output_bytes as usize;
    let diag_limit = caps.diagnostics_bytes;
    let output_overflow = Arc::new(AtomicBool::new(false));
    let overflow_flag = Arc::clone(&output_overflow);

    let (stdout_tx, stdout_rx) = mpsc::channel();
    let (stderr_tx, stderr_rx) = mpsc::channel();

    let (status, reply_bytes, stderr_bytes, overflowed) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            let mut buffered = std::io::BufWriter::with_capacity(64 << 10, &mut stdin);
            let _ = write_wire_request(&mut buffered, request);
            drop(buffered);
            drop(stdin);
        });
        let out_reader = scope.spawn(move || {
            let mut kept = Vec::new();
            let mut chunk = [0u8; 8192];
            let mut overflowed = false;
            while let Ok(read @ 1..) = stdout.read(&mut chunk) {
                let remaining = output_limit.saturating_sub(kept.len());
                if read > remaining {
                    overflowed = true;
                    overflow_flag.store(true, Ordering::Release);
                    drop(stdout);
                    break;
                }
                kept.extend_from_slice(&chunk[..read]);
            }
            let _ = stdout_tx.send((kept, overflowed));
        });
        let err_reader = scope.spawn(move || {
            let mut kept = Vec::new();
            let mut chunk = [0u8; 4096];
            // Always drain stderr to EOF so the worker never blocks on a full pipe.
            while let Ok(read @ 1..) = stderr.read(&mut chunk) {
                let room = diag_limit.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..read.min(room)]);
            }
            let _ = stderr_tx.send(kept);
        });

        let mut status = None;
        while Instant::now() < deadline {
            if output_overflow.load(Ordering::Acquire) {
                // SAFETY: child leads its own process group and is unreaped.
                unsafe { libc::kill(-child_pid, libc::SIGKILL) };
                status = child.wait().ok();
                break;
            }
            match child.try_wait() {
                Ok(Some(exited)) => {
                    status = Some(exited);
                    break;
                }
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => break,
            }
        }
        if status.is_none() {
            // SAFETY: child leads its own process group and is unreaped.
            unsafe { libc::kill(-child_pid, libc::SIGKILL) };
            let _ = child.wait();
        }
        let _ = writer.join();
        let _ = out_reader.join();
        let _ = err_reader.join();
        let (reply_bytes, overflowed) = stdout_rx.recv().unwrap_or_default();
        let stderr_bytes = stderr_rx.recv().unwrap_or_default();
        (status, reply_bytes, stderr_bytes, overflowed)
    });

    if overflowed || output_overflow.load(Ordering::Acquire) {
        return Err(Refusal::Limit(Limit::Output));
    }
    let Some(status) = status else {
        return Err(Refusal::Limit(Limit::Wall));
    };

    let said = String::from_utf8_lossy(&stderr_bytes).into_owned();
    match (status.code(), status.signal()) {
        (Some(0), _) => {}
        (Some(MEMORY_STATUS), _) => return Err(Refusal::Limit(Limit::Memory)),
        _ if confinement.memory_limit_hit() => return Err(Refusal::Limit(Limit::Memory)),
        (_, Some(libc::SIGXCPU)) => return Err(Refusal::Limit(Limit::Cpu)),
        (_, Some(libc::SIGXFSZ)) => return Err(Refusal::Limit(Limit::Output)),
        (Some(1), _) => return Err(Refusal::Rejected(said)),
        (Some(2), _) => {
            if let Some(reason) = said
                .lines()
                .find_map(|line| line.strip_prefix(UNSUPPORTED_PREFIX))
            {
                return Err(Refusal::Unsupported(reason.to_owned()));
            }
            if said
                .lines()
                .any(|line| line.starts_with(LIMIT_INPUT_PREFIX))
            {
                return Err(Refusal::Limit(Limit::Input));
            }
            if said
                .lines()
                .any(|line| line.starts_with(LIMIT_OUTPUT_PREFIX))
            {
                return Err(Refusal::Limit(Limit::Output));
            }
            return Err(Refusal::Unavailable(said));
        }
        _ => {
            return Err(Refusal::Unavailable(format!(
                "the worker ended with {status}"
            )));
        }
    }

    if reply_bytes.len() as u64 > caps.output_bytes {
        return Err(Refusal::Limit(Limit::Output));
    }
    let text = std::str::from_utf8(&reply_bytes)
        .map_err(|error| Refusal::Unavailable(format!("worker reply is not UTF-8: {error}")))?;
    let checked: CheckedLibrary = serde_json::from_str(text)
        .map_err(|error| Refusal::Unavailable(format!("malformed worker reply: {error}")))?;
    let expected_root = request.root.to_string();
    if checked.root != expected_root {
        return Err(Refusal::Unavailable(format!(
            "worker reply root {} does not match requested root {expected_root}",
            checked.root
        )));
    }
    if checked.header.charge != "charge-v1" {
        return Err(Refusal::Unavailable(format!(
            "worker reply carries unexpected charge policy {:?}",
            checked.header.charge
        )));
    }
    Ok(checked)
}
