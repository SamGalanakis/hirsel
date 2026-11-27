//! Named commands share one accepted pair of checkpoints per trust domain.
use crate::package_content::{Binding, Id, Library, Root};
use crate::package_manifest::{SourceLocation, name::Version};
use crate::package_resolve::{Libraries, Resolution};
use crate::package_store::{Entry, PackageStore};
use crate::package_workspace::{Edge, Workspace};
use crate::registry_fetch::Client;
use crate::registry_route::Origin;
use crate::registry_verify::notes::Checkpoint;
use crate::registry_verify::status::{Disposition, Status, StatusReplay};
use crate::registry_verify::{Error, Release};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

type Failure = Box<dyn std::error::Error>;
const WAIT: u64 = 72 * 3600;

#[derive(Debug)]
pub(crate) struct NoEligible(String);

impl std::fmt::Display for NoEligible {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}: no eligible registry release", self.0)
    }
}
impl std::error::Error for NoEligible {}

pub(crate) struct Query {
    pub(crate) name: String,
    pub(crate) major: Option<u32>,
    pub(crate) exact: Option<Version>,
}

impl Query {
    pub(crate) fn parse(text: &str) -> Result<Option<Self>, Failure> {
        let (name, suffix) = text
            .split_once('@')
            .map_or((text, None), |(n, v)| (n, Some(v)));
        if !crate::package_manifest::name::scoped(name) {
            return Ok(None);
        }
        let (major, exact) = match suffix {
            None => (None, None),
            Some(v) if !v.contains('.') => {
                let major = v.parse::<u32>()?;
                if major.to_string() != v {
                    return Err("invalid registry major".into());
                }
                (Some(major), None)
            }
            Some(v) => {
                let version = Version::parse(v).ok_or("expected an exact registry version")?;
                (Some(version.major), Some(version))
            }
        };
        Ok(Some(Self {
            name: name.to_owned(),
            major,
            exact,
        }))
    }
}

struct LogState {
    release: Checkpoint,
    statuses: StatusReplay,
}
struct Candidate {
    leaf: Release,
    covering: u64,
}

/// Incoming edges in the proposed graph, excluding manifest rows the command may rebind.
#[derive(Default)]
pub(crate) struct Constraints(BTreeMap<Id, Vec<Binding>>);

impl Constraints {
    pub(crate) fn from_graph(
        workspace: &Workspace,
        libs: &Libraries,
        mutable: impl Fn(&Id, &str) -> bool,
    ) -> Self {
        let mut edges = BTreeMap::<Id, Vec<Binding>>::new();
        for (member_id, member) in &workspace.members {
            for (alias, id) in &member.aliases {
                if !mutable(member_id, alias)
                    && let Edge::Published(root, _) = &member.edges[id]
                {
                    edges
                        .entry(id.clone())
                        .or_default()
                        .push(Binding::Exact(*root));
                }
            }
        }
        for lib in libs.values() {
            for (id, binding) in lib.content.bindings.values() {
                if binding.root().is_some() {
                    edges.entry(id.clone()).or_default().push(*binding);
                }
            }
        }
        Self(edges)
    }

    pub(crate) fn accepts(&self, id: &Id, root: Root, version: &Version) -> bool {
        self.0
            .get(id)
            .is_none_or(|edges| edges.iter().all(|b| b.accepts(root, version)))
    }
}

pub(crate) struct Registry {
    store: PackageStore,
    now: u64,
    logs: BTreeMap<Origin, LogState>,
    names: BTreeMap<String, BTreeMap<Version, Candidate>>,
    pub(crate) young_fix: Option<String>,
    denied: BTreeSet<Root>,
}

pub(crate) struct Selected {
    pub(crate) root: Root,
    pub(crate) library: Library,
    pub(crate) origin: Origin,
}

fn advisory_release(
    client: &Client<'_>,
    head: &Checkpoint,
    root: Root,
    bytes: &[u8],
) -> Result<Release, Error> {
    let document: Value = serde_json::from_slice(bytes).map_err(|_| Error::Advisory)?;
    for item in document["affected"].as_array().ok_or(Error::Advisory)? {
        if item["package"]["ecosystem"] != "Hirsel" {
            continue;
        }
        let name = item["package"]["name"].as_str().ok_or(Error::Advisory)?;
        // An OSV document can name several packages, including packages on other routes.
        let Ok(hints) = client.hints(name) else {
            continue;
        };
        for hint in hints {
            let leaf = client
                .release_from_hint(head, name, &hint)
                .map_err(|_| Error::ReleaseClaim)?;
            if leaf.root() == root {
                return Ok(leaf);
            }
        }
    }
    Err(Error::Advisory)
}

impl Registry {
    pub(crate) fn new(store: &PackageStore) -> Result<Self, Failure> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        #[cfg(test)]
        let now = std::env::var("HIRSEL_TEST_REGISTRY_SECONDS")
            .ok()
            .map(|n| n.parse())
            .transpose()?
            .unwrap_or(now);
        Ok(Self {
            store: store.clone(),
            now,
            logs: BTreeMap::new(),
            names: BTreeMap::new(),
            young_fix: None,
            denied: BTreeSet::new(),
        })
    }

    pub(crate) fn open(&mut self, name: &str, pin: Option<&Origin>) -> Result<Origin, Failure> {
        let client = self.store.registry(name, pin)?;
        let origin = client.route().origin().clone();
        if !self.logs.contains_key(&origin) {
            let release = client.release_checkpoint(self.now).map_err(boxed)?;
            if release.is_preview() && self.store.transport.is_offline() {
                return Err(
                    "--frozen cannot verify checkpoint freshness for preview (quorum 0)".into(),
                );
            }
            let statuses = client
                .status_replay(self.now, |root, bytes| {
                    advisory_release(&client, &release, root, bytes)
                })
                .map_err(boxed)?;
            self.logs
                .insert(origin.clone(), LogState { release, statuses });
        }
        Ok(origin)
    }

    fn candidates(&mut self, name: &str, origin: &Origin) -> Result<(), Failure> {
        if !self.names.contains_key(name) {
            let client = self.store.registry(name, Some(origin))?;
            let mut versions = BTreeMap::new();
            for hint in client.hints(name).map_err(boxed)? {
                let leaf = client
                    .release_from_hint(&self.logs[origin].release, name, &hint)
                    .map_err(boxed)?;
                if let Some(old) = versions.insert(
                    hint.version,
                    Candidate {
                        leaf,
                        covering: hint.covering_size,
                    },
                ) && old.leaf.index() != hint.index
                {
                    return Err(format!("conflicting release rows for {name}").into());
                }
            }
            self.names.insert(name.into(), versions);
        }
        Ok(())
    }

    pub(crate) fn select(
        &mut self,
        query: &Query,
        pin: Option<(&Origin, Root, &str)>,
        accepts: impl Fn(Root, &Version) -> bool,
    ) -> Result<Selected, Failure> {
        let name = &query.name;
        let origin = self.open(name, pin.map(|p| p.0))?;
        self.candidates(name, &origin)?;
        let versions = &self.names[name];
        let statuses = &self.logs[&origin].statuses;
        let client = self.store.registry(name, Some(&origin))?;
        let before = pin
            .map(|(_, root, alias)| {
                versions
                    .iter()
                    .find(|(v, c)| {
                        c.leaf.root() == root && query.major.is_none_or(|major| v.major == major)
                    })
                    .map(|(v, _)| (v, statuses.state(root), alias))
                    .ok_or_else(|| format!("{name} {root} lacks verified release inclusion"))
            })
            .transpose()?;
        let mut young = Vec::new();
        let mut selected = None;
        for (v, candidate) in versions.iter().rev() {
            if query.major.is_some_and(|major| major != v.major)
                || query
                    .exact
                    .as_ref()
                    .map_or(!v.pre.is_empty(), |exact| exact != v)
                || before
                    .is_some_and(|(old, _, _)| v < old || (old.major == 0 && old.minor != v.minor))
                || self.denied.contains(&candidate.leaf.root())
                || !accepts(candidate.leaf.root(), v)
                || statuses
                    .state(candidate.leaf.root())
                    .is_some_and(|s| s.disposition != Disposition::Active)
            {
                continue;
            }
            if query.exact.is_none() {
                let published = client
                    .publication_time(&candidate.leaf, Some(candidate.covering), self.now)
                    .map_err(boxed)?;
                let age = self.now.saturating_sub(published);
                if age < WAIT {
                    young.push((candidate, age));
                    continue;
                }
            }
            selected = Some(candidate);
            break;
        }
        if selected.is_none()
            && let Some((
                old,
                Some(Status {
                    disposition: Disposition::Retired { reason, message },
                    ..
                }),
                alias,
            )) = before
            && let Some((candidate, age)) = young.first()
        {
            let hint = if young.len() == 1 {
                let fix = candidate.leaf.version();
                let age = if *age >= 3600 {
                    format!("{} h", age / 3600)
                } else {
                    format!("{} s", age)
                };
                format!(
                    "{alias} {old} is retired ({reason}: {message}).\n{alias} {fix} was published {age} ago, inside the 72 h wait.\nTo take it now: hirsel update {alias}@{fix}"
                )
            } else {
                format!(
                    "{alias} {old} is retired ({reason}: {message}). No eligible release is outside the 72 h wait."
                )
            };
            self.young_fix.get_or_insert(hint);
            selected = Some(candidate);
        }
        let candidate = selected.ok_or_else(|| NoEligible(name.clone()))?;
        let root = candidate.leaf.root();
        let library = match self.store.read(root)? {
            Entry::Present(lib) => lib,
            _ => client
                .frame(candidate.leaf.id(), root, |_| Ok(()))
                .map_err(boxed)?,
        };
        if library.content.id != *candidate.leaf.id()
            || library.content.version != *candidate.leaf.version()
        {
            return Err(format!("{name} {root}: registry release differs from its content").into());
        }
        Ok(Selected {
            root,
            library,
            origin,
        })
    }

    pub(crate) fn deny(&mut self, root: Root) -> bool {
        self.denied.insert(root)
    }

    pub(crate) fn add_candidate(
        &mut self,
        query: &Query,
        options_alias: Option<&str>,
        shared: bool,
        workspace: &Workspace,
    ) -> Result<Selected, Failure> {
        let libs = crate::package_resolve::registry_inputs(
            workspace,
            &self.store,
            BTreeMap::new(),
            &mut BTreeSet::new(),
        )?;
        let alias = options_alias.unwrap_or_else(|| hirsel_core_resolve::short_name(&query.name));
        let constraints = Constraints::from_graph(workspace, &libs, |id, a| {
            a == alias
                && (workspace.member.as_ref() == Some(id)
                    || (shared
                        && matches!(
                            workspace.members[id].package.dependencies.get(a),
                            Some(crate::package_manifest::Dependency::Inherited)
                        )))
        });
        self.select(query, None, |r, v| {
            constraints.accepts(
                &Id {
                    name: query.name.clone(),
                    major: v.major,
                },
                r,
                v,
            )
        })
    }

    pub(crate) fn check_add(
        &mut self,
        trial: &Resolution,
        id: &Id,
        root: Option<Root>,
        locator: Option<&SourceLocation>,
        shared: Option<&crate::package_manifest::Workspace>,
        alias: &str,
    ) -> Result<(BTreeMap<String, Finding>, Vec<String>), Failure> {
        use crate::package_manifest::Dependency;
        let mut findings = self.graph(trial, None)?;
        require_active(&findings)?;
        let mut checked = crate::package_audit::check(trial, None)?;
        if let (Some(SourceLocation::Registry(origin)), Some(root)) = (locator, root)
            && shared.is_some()
            && !trial.workspace.members.values().any(|m| {
                matches!(
                    m.package.dependencies.get(alias),
                    Some(Dependency::Inherited)
                )
            })
        {
            let ceiling = match shared.as_ref().and_then(|w| w.dependencies.get(alias)) {
                Some(Dependency::Published(_, _, _, mask)) => *mask,
                _ => 0,
            };
            let (unused, authority) = self.unbound(id, root, origin, ceiling)?;
            findings.extend(unused);
            checked.authority.extend(authority);
        }
        let report = findings
            .into_iter()
            .map(|(id, f)| (id.to_string(), f))
            .collect();
        Ok((report, checked.authority))
    }

    pub(crate) fn unbound(
        &mut self,
        id: &Id,
        root: Root,
        origin: &Origin,
        ceiling: u16,
    ) -> Result<(BTreeMap<Id, Finding>, Vec<String>), Failure> {
        // The workspace lock omits unused shared rows. Resolve this declaration with
        // the same carrier walk and compiler checks as a member inheriting the row.
        let dir = tempfile::tempdir()?;
        let kinds = crate::reachability::authority::names(ceiling.into());
        let manifest = format!(
            "[package]\nname = \"local/consumer\"\nversion = \"1.0.0\"\ncharge = \"charge-v1\"\n[sources]\n\"main.hirsel\" = \"main\"\n[dependencies]\nsubject = {{ package = {:?}, major = {}, root = \"{root}\", registry = \"{origin}\", authority = {kinds:?} }}\n",
            id.name, id.major
        );
        std::fs::write(dir.path().join("hirsel.toml"), manifest)?;
        std::fs::write(
            dir.path().join("main.hirsel"),
            "pub fn main() -> Result[(), Exit] { Ok(()) }\n",
        )?;
        let workspace = Workspace::discover_for(dir.path(), true)?
            .ok_or("missing declaration check workspace")?;
        let res = Resolution::resolve(workspace, self.store.clone(), false)?;
        let findings = self.graph(&res, None)?;
        require_active(&findings)?;
        let checked = crate::package_audit::check(&res, None)?;
        Ok((findings, checked.authority))
    }

    pub(crate) fn finding(&self, origin: &Origin, root: Root) -> Result<Finding, Failure> {
        let log = &self.logs[origin];
        Finding::new(log.release.is_preview(), log.statuses.state(root))
    }

    /// Follow the effective roots, retaining the carrier of an unlogged exact child.
    pub(crate) fn graph(
        &mut self,
        res: &Resolution,
        selected: Option<&Id>,
    ) -> Result<BTreeMap<Id, Finding>, Failure> {
        let mut work: Vec<(Id, Option<Origin>, bool)> = res
            .workspace
            .members
            .keys()
            .filter(|id| selected.is_none_or(|member| member == *id))
            .map(|id| (id.clone(), None, false))
            .collect();
        let source = |id: &Id, declared: Option<&SourceLocation>| {
            let source = res
                .workspace
                .selections
                .get(id)
                .and_then(|(_, s)| s.as_ref())
                .or(declared);
            match source {
                Some(SourceLocation::Registry(origin)) => Some(origin.clone()),
                _ => None,
            }
        };
        let mut seen = BTreeSet::new();
        let mut result = BTreeMap::new();
        while let Some((id, carrier, logged)) = work.pop() {
            if !seen.insert((id.clone(), carrier.clone(), logged)) {
                continue;
            }
            if let Some(member) = res.workspace.members.get(&id) {
                for (child, edge) in &member.edges {
                    match edge {
                        Edge::Toolchain => {}
                        Edge::Member => work.push((child.clone(), None, false)),
                        Edge::Published(_, locator) => {
                            work.push((child.clone(), source(child, locator.as_ref()), true))
                        }
                    }
                }
                continue;
            }
            let Some(root) = res.published.get(&id).copied() else {
                continue;
            };
            let lib = &res.libraries[&root];
            if let Some(origin) = &carrier {
                if logged {
                    self.open(&id.name, Some(origin))?;
                    self.candidates(&id.name, origin)?;
                    if !self.names[&id.name]
                        .get(&lib.content.version)
                        .is_some_and(|c| c.leaf.root() == root)
                    {
                        return Err(format!("{id} {root} lacks verified release inclusion").into());
                    }
                }
                let finding = self.finding(origin, root)?;
                result
                    .entry(id.clone())
                    .or_insert_with(Vec::new)
                    .push(finding);
            }
            for (child, binding) in lib.content.bindings.values() {
                if binding.root().is_none() {
                    continue;
                }
                if let Binding::Floor {
                    tested,
                    minor,
                    patch,
                } = binding
                    && carrier.is_some()
                {
                    let origin = self.open(&child.name, None)?;
                    self.candidates(&child.name, &origin)?;
                    let floor = Version {
                        major: child.major,
                        minor: *minor,
                        patch: *patch,
                        pre: String::new(),
                    };
                    if self.names[&child.name]
                        .get(&floor)
                        .is_none_or(|c| c.leaf.root() != *tested)
                    {
                        return Err(format!(
                            "{child}: floor {floor} {tested} lacks verified release inclusion"
                        )
                        .into());
                    }
                    work.push((child.clone(), Some(origin), true));
                } else if let Some(explicit) = source(child, None) {
                    work.push((child.clone(), Some(explicit), true));
                } else {
                    work.push((child.clone(), carrier.clone(), false));
                }
            }
        }
        Ok(result
            .into_iter()
            .map(|(k, v)| (k, Finding::merge(v)))
            .collect())
    }
}

fn boxed(e: Box<dyn std::error::Error + Send + Sync>) -> Failure {
    e
}

#[derive(Default, Serialize)]
pub(crate) struct Finding {
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    pub(crate) trust: Option<&'static str>,
    pub(crate) advisories: BTreeMap<String, Value>,
}
impl Finding {
    fn new(preview: bool, state: Option<&Status>) -> Result<Self, Failure> {
        let (status, reason, message) = match state.map(|s| &s.disposition) {
            None | Some(Disposition::Active) => ("active", None, None),
            Some(Disposition::Retired { reason, message }) => {
                ("retired", Some(reason.clone()), Some(message.clone()))
            }
            Some(Disposition::TakenDown { reason, message }) => {
                ("taken-down", Some(reason.clone()), Some(message.clone()))
            }
        };
        let mut advisories = BTreeMap::new();
        for (id, bytes) in state.into_iter().flat_map(|s| &s.advisories) {
            advisories.insert(id.clone(), serde_json::from_slice(bytes)?);
        }
        Ok(Self {
            status,
            reason,
            message,
            trust: preview.then_some("preview (quorum 0), unwitnessed"),
            advisories,
        })
    }

    fn merge(items: Vec<Self>) -> Self {
        let mut merged = Self {
            status: "active",
            ..Default::default()
        };
        for item in items {
            merged.trust = merged.trust.or(item.trust);
            if item.status != "active" && merged.status != "taken-down" {
                merged.status = item.status;
                merged.reason = item.reason;
                merged.message = item.message;
            }
            merged.advisories.extend(item.advisories);
        }
        merged
    }

    pub(crate) fn report(&self, id: impl std::fmt::Display) {
        print!("{id}: {}", self.status.replace('-', " "));
        if let (Some(reason), Some(message)) = (&self.reason, &self.message) {
            print!(" ({reason}: {message})");
        }
        if let Some(trust) = self.trust {
            print!("; {trust}");
        }
        println!();
        for (osv, doc) in &self.advisories {
            println!(
                "{id}: {osv} (severity {}, aliases {}, modified {})",
                doc["severity"], doc["aliases"], doc["modified"]
            );
        }
    }
}

pub(crate) fn require_active(findings: &BTreeMap<Id, Finding>) -> Result<(), Failure> {
    for (id, finding) in findings {
        if finding.status != "active" {
            return Err(format!(
                "{id} is {} ({}: {})",
                finding.status,
                finding.reason.as_deref().unwrap_or_default(),
                finding.message.as_deref().unwrap_or_default()
            )
            .into());
        }
    }
    Ok(())
}
