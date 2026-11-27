//! Internal confined worker entry point for registry `check_library` admission
//! (`docs/spec/registry.md` §2).
//!
//! Reads one binary length-delimited request from stdin, enforces all input
//! caps before allocating, verifies every Phase 0 library root (`Root::of`),
//! decodes every `C` with the canonical v3 decoder (`package_content.rs`),
//! walks the tested dependency graph without substitutions, validates verified
//! release history for floor (and optional exact) children, runs the
//! target-independent library front-end + §11 CoreIR authority card check, and
//! emits RFC 8785 canonical JSON for `CheckedLibrary` on stdout.

use crate::checked::RejectedCheck;
use crate::package_content::{
    ARCHIVE_LIMIT, Binding, CHARGE, CONTENT_LIMIT, Content, Id, Library, PACKAGE_LIMIT,
    PackageArchive, Root, toolchain_bindings,
};
use crate::package_dir::PackageDir;
use crate::package_manifest::name::{self as names, Version};
use crate::{CheckProduct, CheckRequest, Driver};
use bytes::Bytes;
use hirsel_core_ids::ContentHash;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;

/// Internal argv token used by `hirsel-compiler` to invoke this worker.
pub(crate) const WORKER_ARG: &str = "__check-library";

const WIRE_MAGIC: &[u8; 4] = b"HCL1";
const HISTORY_METADATA_LIMIT: usize = 64 << 20;
const REPLY_LIMIT: usize = 64 << 20;
const UNSUPPORTED_PREFIX: &str = "check_library: unsupported: ";
const LIMIT_INPUT_PREFIX: &str = "check_library: limit: input: ";
const LIMIT_OUTPUT_PREFIX: &str = "check_library: limit: output: ";

enum Failure {
    Rejected(String),
    Unsupported(String),
    InputLimit(String),
    OutputLimit(String),
    Unavailable(String),
}

struct WireRequest {
    head_root: Root,
    head_bytes: Vec<u8>,
    closure: Vec<(Root, Vec<u8>)>,
    history: Vec<WireHistory>,
}

struct WireHistory {
    origin: String,
    identity: String,
    version: String,
    root: Root,
}

struct VerifiedHistoryRow {
    origin: String,
    identity: Id,
    version: Version,
    root: Root,
}

#[derive(Serialize)]
struct CheckedHeader<'a> {
    identity: String,
    version: String,
    charge: &'a str,
}

pub(crate) fn run_worker() -> i32 {
    match execute() {
        Ok(json_bytes) => {
            let mut stdout = io::stdout().lock();
            if let Err(error) = stdout.write_all(&json_bytes).and_then(|()| stdout.flush()) {
                let _ = writeln!(io::stderr().lock(), "check_library: write reply: {error}");
                return 2;
            }
            0
        }
        Err(Failure::Rejected(message)) => {
            let _ = writeln!(io::stderr().lock(), "{message}");
            1
        }
        Err(Failure::Unsupported(reason)) => {
            let _ = writeln!(io::stderr().lock(), "{UNSUPPORTED_PREFIX}{reason}");
            2
        }
        Err(Failure::InputLimit(reason)) => {
            let _ = writeln!(io::stderr().lock(), "{LIMIT_INPUT_PREFIX}{reason}");
            2
        }
        Err(Failure::OutputLimit(reason)) => {
            let _ = writeln!(io::stderr().lock(), "{LIMIT_OUTPUT_PREFIX}{reason}");
            2
        }
        Err(Failure::Unavailable(reason)) => {
            let _ = writeln!(io::stderr().lock(), "check_library: unavailable: {reason}");
            2
        }
    }
}

fn execute() -> Result<Vec<u8>, Failure> {
    let stdin = io::stdin();
    let request = read_wire_request(BufReader::new(stdin.lock()))?;
    let checked = check_wire_request(request)?;
    let canonical = canonical_json(&checked);
    let bytes = canonical.into_bytes();
    if bytes.len() > REPLY_LIMIT {
        return Err(Failure::OutputLimit(format!(
            "reply size {} exceeds {REPLY_LIMIT} bytes",
            bytes.len()
        )));
    }
    Ok(bytes)
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

fn read_root(reader: &mut impl Read, what: &str) -> Result<Root, Failure> {
    let mut buf = [0u8; 32];
    read_exact_or_reject(reader, &mut buf, what)?;
    Ok(Root(buf))
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

fn read_wire_request(mut reader: impl Read) -> Result<WireRequest, Failure> {
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
    let closure_count = read_u32(&mut reader, "closure count")? as usize;
    if closure_count >= PACKAGE_LIMIT {
        return Err(Failure::InputLimit(format!(
            "package count {} exceeds limit of {PACKAGE_LIMIT}",
            closure_count.saturating_add(1)
        )));
    }
    let mut closure = Vec::with_capacity(closure_count);
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
        closure.push((root, bytes));
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
    Ok(WireRequest {
        head_root,
        head_bytes,
        closure,
        history,
    })
}

fn check_wire_request(request: WireRequest) -> Result<Value, Failure> {
    // 1. Verify head root and decode canonical C v3 (scoped only).
    let actual_head = Root::of(&request.head_bytes);
    if actual_head != request.head_root {
        return Err(Failure::Rejected(format!(
            "head root mismatch: expected {}, computed {}",
            ContentHash(request.head_root.0),
            ContentHash(actual_head.0)
        )));
    }
    let head_encoded = Bytes::from(request.head_bytes);
    let head_content = Content::decode_as(&head_encoded, false)
        .map_err(|error| Failure::Rejected(format!("head content: {error}")))?;
    drop(head_encoded);
    let mut entries = BTreeMap::new();
    entries.insert(
        actual_head,
        Library {
            content: head_content,
            encoded: Bytes::new(),
        },
    );

    // 2. Verify every closure entry's root and decode canonical C v3.
    for (claimed_root, raw_bytes) in request.closure {
        let actual = Root::of(&raw_bytes);
        if actual != claimed_root {
            return Err(Failure::Rejected(format!(
                "closure root mismatch: expected {}, computed {}",
                ContentHash(claimed_root.0),
                ContentHash(actual.0)
            )));
        }
        if entries.contains_key(&actual) {
            return Err(Failure::Rejected(format!(
                "duplicate package root {} in closure",
                ContentHash(actual.0)
            )));
        }
        let encoded = Bytes::from(raw_bytes);
        let content = Content::decode_as(&encoded, false).map_err(|error| {
            Failure::Rejected(format!("closure {}: {error}", ContentHash(actual.0)))
        })?;
        drop(encoded);
        entries.insert(
            actual,
            Library {
                content,
                encoded: Bytes::new(),
            },
        );
    }

    // 3. Parse and validate caller-supplied verified release history tuples.
    let history = validate_history(request.history, actual_head, &entries)?;

    // 4. Walk the tested dependency graph without substitutions, verifying
    //    completeness, uniqueness, acyclicity, exact pins, floor history, and
    //    toolchain bindings.
    verify_graph(actual_head, &entries, &history)?;

    // 5. Build target-independent library CheckRequest and run front-end + §11
    //    CoreIR authority card check (no linking, no foreign init, no tests).
    let archive = PackageArchive {
        head: actual_head,
        entries,
    };
    let head_entry = &archive.entries[&archive.head].content;
    let head_id = head_entry.id.clone();
    let head_version = head_entry.version.clone();
    let head_bindings = head_entry.bindings.clone();
    let app = PackageDir::from_archive(&archive).map_err(classify_archive_error)?;
    drop(archive);
    let needs_std = app.needs_std();
    let store = worker_store_path()?;
    let mut check_request = CheckRequest::from_app(app, None, &store)
        .map_err(|error| Failure::Unavailable(format!("prepare check request: {error}")))?;
    check_request.entry = None;
    if needs_std && let Some(image) = discover_installed_std()? {
        check_request
            .use_image(image)
            .map_err(|error| Failure::Unavailable(format!("load installed std image: {error}")))?;
    }
    let mut driver = Driver::default();
    let product = match driver.check_for_authority(check_request) {
        Ok(product) => product,
        Err(error) => return Err(classify_driver_error(error)),
    };
    if !product.accepted() {
        return Err(Failure::Rejected(render_product_diagnostics(&product)));
    }
    if let Some(refusal) = &product.authority_refusal {
        return Err(Failure::Rejected(refusal.clone()));
    }

    // 6. Project CheckedLibrary: header, C-order bindings, unfiltered
    //    InterfaceV1, and §11 CoreIR authority Card.
    let interface = crate::inspect::unfiltered_interface(&product, head_id.compiler_id())
        .map_err(|error| Failure::Unavailable(format!("project interface: {error}")))?;
    let card = product
        .authority_cards
        .get(&head_id.compiler_id())
        .cloned()
        .unwrap_or_default();
    let authority = crate::package_audit::project_card(&card);

    let bindings: Vec<Value> = head_bindings
        .iter()
        .map(|(alias, (bound_id, binding))| match binding {
            Binding::Toolchain => serde_json::json!([alias, bound_id.to_string(), "toolchain"]),
            Binding::Exact(tested) => {
                serde_json::json!([
                    alias,
                    bound_id.to_string(),
                    "exact",
                    ContentHash(tested.0).to_string()
                ])
            }
            Binding::Floor {
                tested,
                minor,
                patch,
            } => serde_json::json!([
                alias,
                bound_id.to_string(),
                "floor",
                ContentHash(tested.0).to_string(),
                format!("{}.{minor}.{patch}", bound_id.major)
            ]),
        })
        .collect();

    let header = CheckedHeader {
        identity: head_id.to_string(),
        version: head_version.to_string(),
        charge: CHARGE,
    };

    Ok(serde_json::json!({
        "root": ContentHash(actual_head.0).to_string(),
        "header": header,
        "bindings": bindings,
        "interface": interface,
        "authority": authority,
    }))
}

fn validate_history(
    rows: Vec<WireHistory>,
    head_root: Root,
    entries: &BTreeMap<Root, Library>,
) -> Result<BTreeMap<Root, VerifiedHistoryRow>, Failure> {
    let mut by_root = BTreeMap::new();
    for row in rows {
        if !valid_origin(&row.origin) {
            return Err(Failure::Rejected(format!(
                "invalid registry origin {:?} in history",
                row.origin
            )));
        }
        let identity = parse_scoped_id(&row.identity).ok_or_else(|| {
            Failure::Rejected(format!(
                "invalid package identity {:?} in history",
                row.identity
            ))
        })?;
        let version = Version::parse(&row.version).ok_or_else(|| {
            Failure::Rejected(format!("invalid version {:?} in history", row.version))
        })?;
        if version.major != identity.major {
            return Err(Failure::Rejected(format!(
                "history version {} does not match identity {}",
                version, identity
            )));
        }
        if row.root == head_root {
            return Err(Failure::Rejected(format!(
                "history must not reference head root {}",
                ContentHash(row.root.0)
            )));
        }
        let Some(child) = entries.get(&row.root) else {
            return Err(Failure::Rejected(format!(
                "history references root {} not present in closure",
                ContentHash(row.root.0)
            )));
        };
        if child.content.id != identity {
            return Err(Failure::Rejected(format!(
                "history identity {} does not match decoded child {} for root {}",
                identity,
                child.content.id,
                ContentHash(row.root.0)
            )));
        }
        if child.content.version != version {
            return Err(Failure::Rejected(format!(
                "history version {} does not match decoded child version {} for {} ({})",
                version,
                child.content.version,
                child.content.id,
                ContentHash(row.root.0)
            )));
        }
        if by_root
            .insert(
                row.root,
                VerifiedHistoryRow {
                    origin: row.origin,
                    identity,
                    version,
                    root: row.root,
                },
            )
            .is_some()
        {
            return Err(Failure::Rejected(format!(
                "duplicate history entry for root {}",
                ContentHash(row.root.0)
            )));
        }
    }
    Ok(by_root)
}

fn verify_graph(
    head_root: Root,
    entries: &BTreeMap<Root, Library>,
    history: &BTreeMap<Root, VerifiedHistoryRow>,
) -> Result<(), Failure> {
    struct Walk<'a> {
        entries: &'a BTreeMap<Root, Library>,
        history: &'a BTreeMap<Root, VerifiedHistoryRow>,
        visiting_roots: BTreeSet<Root>,
        visiting_ids: BTreeSet<Id>,
        visited: BTreeSet<Root>,
        identity_roots: BTreeMap<Id, Root>,
        toolchain: BTreeSet<Id>,
    }

    impl Walk<'_> {
        fn visit(&mut self, root: Root) -> Result<(), Failure> {
            let content = &self.entries[&root].content;
            if self.visiting_roots.contains(&root) || self.visiting_ids.contains(&content.id) {
                return Err(Failure::Rejected(format!(
                    "dependency cycle involving {}",
                    content.id
                )));
            }
            if let Some(&existing) = self.identity_roots.get(&content.id) {
                if existing != root {
                    return Err(Failure::Rejected(format!(
                        "{} is bound to both {} and {}",
                        content.id,
                        ContentHash(existing.0),
                        ContentHash(root.0)
                    )));
                }
                if self.visited.contains(&root) {
                    return Ok(());
                }
            }
            self.identity_roots.insert(content.id.clone(), root);
            self.visiting_roots.insert(root);
            self.visiting_ids.insert(content.id.clone());

            for (alias, (bound_id, binding)) in &content.bindings {
                match *binding {
                    Binding::Toolchain => {
                        if toolchain_bindings(bound_id).is_none() {
                            return Err(Failure::Rejected(format!(
                                "{}: binding {alias} names unknown toolchain package {bound_id}",
                                content.id
                            )));
                        }
                        self.toolchain.insert(bound_id.clone());
                    }
                    Binding::Exact(child_root) => {
                        let child = self.entries.get(&child_root).ok_or_else(|| {
                            Failure::Rejected(format!(
                                "{}: binding {alias} = {bound_id} is missing closure entry {}",
                                content.id,
                                ContentHash(child_root.0)
                            ))
                        })?;
                        if child.content.id != *bound_id {
                            return Err(Failure::Rejected(format!(
                                "{}: binding {alias} expects {bound_id} at {}, but closure holds {}",
                                content.id,
                                ContentHash(child_root.0),
                                child.content.id
                            )));
                        }
                        if let Some(row) = self.history.get(&child_root)
                            && (row.root != child_root
                                || row.identity != *bound_id
                                || row.version != child.content.version
                                || row.origin.is_empty())
                        {
                            return Err(Failure::Rejected(format!(
                                "{}: exact binding {alias} = {bound_id} disagrees with verified history",
                                content.id
                            )));
                        }
                        self.visit(child_root)?;
                    }
                    Binding::Floor {
                        tested: child_root,
                        minor,
                        patch,
                    } => {
                        let child = self.entries.get(&child_root).ok_or_else(|| {
                            Failure::Rejected(format!(
                                "{}: floor binding {alias} = {bound_id} is missing closure entry {}",
                                content.id,
                                ContentHash(child_root.0)
                            ))
                        })?;
                        if child.content.id != *bound_id {
                            return Err(Failure::Rejected(format!(
                                "{}: floor binding {alias} expects {bound_id} at {}, but closure holds {}",
                                content.id,
                                ContentHash(child_root.0),
                                child.content.id
                            )));
                        }
                        let bound_version = Version {
                            major: bound_id.major,
                            minor,
                            patch,
                            pre: String::new(),
                        };
                        if child.content.version != bound_version {
                            return Err(Failure::Rejected(format!(
                                "{}: floor binding {alias} = {bound_id} requires tested version {bound_version}, but child header declares {}",
                                content.id, child.content.version
                            )));
                        }
                        let Some(row) = self.history.get(&child_root) else {
                            return Err(Failure::Rejected(format!(
                                "{}: floor binding {alias} = {bound_id} ({}) is missing verified release history",
                                content.id,
                                ContentHash(child_root.0)
                            )));
                        };
                        if row.root != child_root
                            || row.identity != *bound_id
                            || row.version != bound_version
                            || row.origin.is_empty()
                        {
                            return Err(Failure::Rejected(format!(
                                "{}: floor binding {alias} = {bound_id} ({bound_version}) does not match verified history ({} {} at {})",
                                content.id, row.identity, row.version, row.origin
                            )));
                        }
                        self.visit(child_root)?;
                    }
                }
            }

            self.visiting_roots.remove(&root);
            self.visiting_ids.remove(&content.id);
            self.visited.insert(root);
            Ok(())
        }
    }

    let mut walk = Walk {
        entries,
        history,
        visiting_roots: BTreeSet::new(),
        visiting_ids: BTreeSet::new(),
        visited: BTreeSet::new(),
        identity_roots: BTreeMap::new(),
        toolchain: BTreeSet::new(),
    };
    walk.visit(head_root)?;

    for id in &walk.toolchain {
        if walk.identity_roots.contains_key(id) {
            return Err(Failure::Rejected(format!(
                "{id} is bound both to the toolchain and to a published library"
            )));
        }
    }
    if let Some(extra) = entries.keys().find(|root| !walk.visited.contains(root)) {
        return Err(Failure::Rejected(format!(
            "closure contains unreferenced entry {}",
            ContentHash(extra.0)
        )));
    }
    Ok(())
}

fn parse_scoped_id(text: &str) -> Option<Id> {
    let (name, major_text) = text.rsplit_once('@')?;
    if !names::scoped(name) || major_text.is_empty() {
        return None;
    }
    if (major_text.len() > 1 && major_text.starts_with('0'))
        || !major_text.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let major = major_text.parse::<u32>().ok()?;
    Some(Id {
        name: name.to_owned(),
        major,
    })
}

/// `registry.md` §1: HTTPS origin with lowercase DNS host, no userinfo, path,
/// query or fragment, and default port (443) omitted.
fn valid_origin(origin: &str) -> bool {
    let Some(authority) = origin.strip_prefix("https://") else {
        return false;
    };
    if authority.is_empty()
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
        || authority.contains('@')
    {
        return false;
    }
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if !valid_dns_host(host) {
        return false;
    }
    if let Some(port) = port {
        if port.is_empty() || port.starts_with('0') || !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        let Ok(number) = port.parse::<u16>() else {
            return false;
        };
        if number == 0 || number == 443 {
            return false;
        }
    }
    true
}

fn valid_dns_host(host: &str) -> bool {
    let labels: Vec<_> = host.split('.').collect();
    host.len() <= 253
        && (2..=127).contains(&labels.len())
        && labels.iter().all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && !labels
            .last()
            .is_some_and(|label| label.bytes().all(|b| b.is_ascii_digit()))
}

fn worker_store_path() -> Result<PathBuf, Failure> {
    let cwd = std::env::current_dir()
        .map_err(|error| Failure::Unavailable(format!("read worker cwd: {error}")))?;
    Ok(cwd.join("store"))
}

fn discover_installed_std() -> Result<Option<std::sync::Arc<crate::CoreImage>>, Failure> {
    let Ok(executable) = crate::installation::compiler_executable() else {
        return Ok(None);
    };
    let installed = match crate::installation::Installed::discover(&executable) {
        Ok(Some(installed)) => installed,
        Ok(None) => return Ok(None),
        // A bare worker under confinement may not have read access to its parent
        // directory when no toolchain prefix exists; fall back to bundled std.
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(None),
        Err(error) => {
            return Err(Failure::Unavailable(format!(
                "discover installed toolchain: {error}"
            )));
        }
    };
    installed
        .product("std")
        .map_err(|error| Failure::Unavailable(format!("load installed std product: {error}")))
}

fn classify_archive_error(error: io::Error) -> Failure {
    let message = error.to_string();
    if message.contains("requires an installed toolchain")
        || message.contains("the toolchain supplies no")
    {
        Failure::Unsupported(message)
    } else {
        Failure::Rejected(message)
    }
}

fn classify_driver_error(error: io::Error) -> Failure {
    if let Some(rejected) = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<RejectedCheck>())
    {
        return Failure::Rejected(render_product_diagnostics(&rejected.product));
    }
    Failure::Rejected(error.to_string())
}

fn render_product_diagnostics(product: &CheckProduct) -> String {
    let mut lines = Vec::new();
    for refusal in &product.diagnostics {
        match product.render_refusal(refusal) {
            Ok(text) => lines.push(text),
            Err(error) => lines.push(format!("{}: {error}", refusal.rule)),
        }
    }
    for resource in &product.resources {
        let site = product
            .render_site(&resource.primary)
            .unwrap_or_else(|_| "<unknown>".into());
        lines.push(format!("{site}: {}", resource.message));
    }
    for gap in &product.incomplete {
        let site = product
            .render_site(&gap.primary)
            .unwrap_or_else(|_| "<unknown>".into());
        lines.push(format!("{site}: {}", gap.message));
    }
    if lines.is_empty() {
        "library check failed".into()
    } else {
        lines.join("\n")
    }
}

/// RFC 8785 (JCS) canonical UTF-8 JSON with no trailing newline.
fn canonical_json(value: &Value) -> String {
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
