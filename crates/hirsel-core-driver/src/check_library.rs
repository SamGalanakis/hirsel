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
    Binding, CHARGE, Content, Id, Library, PackageArchive, Root, toolchain_bindings,
};
use crate::package_dir::PackageDir;
use crate::package_manifest::name::{self as names, Version};
use crate::{CheckProduct, CheckRequest, Driver};
use bytes::Bytes;
use hirsel_core_ids::ContentHash;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufReader, Write};
use std::path::PathBuf;

pub(crate) use hirsel_compiler::check_library::WORKER_ARG;
use hirsel_compiler::check_library::{CheckLibraryCaps, canonical_json, wire::*};
const REPLY_LIMIT: usize = CheckLibraryCaps::REGISTRY.output_bytes as usize;

impl From<[u8; 32]> for Root {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
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
    if writeln!(
        io::stderr().lock(),
        "{ID_PREFIX}{}",
        crate::compiler_identity()
    )
    .is_err()
    {
        return 2;
    }
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
    hirsel_compiler::check_library::seal_worker()
        .map_err(|e| Failure::Unavailable(e.to_string()))?;
    let stdin = io::stdin();
    let mut entries = BTreeMap::new();
    let (head, history) = read_wire_request(BufReader::new(stdin.lock()), |claimed: Root, raw| {
        let root = Root::of(&raw);
        if root != claimed {
            return Err(Failure::Rejected(format!(
                "root mismatch: expected {claimed}, computed {root}"
            )));
        }
        if entries.contains_key(&root) {
            return Err(Failure::Rejected(format!("duplicate package root {root}")));
        }
        let content = Content::decode_as(&Bytes::from(raw), false)
            .map_err(|e| Failure::Rejected(format!("content {root}: {e}")))?;
        entries.insert(
            root,
            Library {
                content,
                encoded: Bytes::new(),
            },
        );
        Ok(())
    })?;
    let checked = check_library(head, entries, history)?;
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

fn check_library(
    actual_head: Root,
    entries: BTreeMap<Root, Library>,
    rows: Vec<WireHistory<Root>>,
) -> Result<Value, Failure> {
    // 3. Parse and validate caller-supplied verified release history tuples.
    let history = validate_history(rows, actual_head, &entries)?;

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
    rows: Vec<WireHistory<Root>>,
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
