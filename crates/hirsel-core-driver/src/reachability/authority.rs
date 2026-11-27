//! Package authority follows checked instance identities before target specialization.
use super::*;
use hirsel_core_ir::{Callee, FuncId, HostOp, Inst, Module, RtOp, release_recipe::GrantKind};

mod card;
pub(crate) use card::{Card, Cards, Verdict, carried, carried_names, names};
use hirsel_core_materialize::Resolver;
use std::collections::VecDeque;

#[derive(Default)]
pub(crate) struct Node {
    pub(crate) children: BTreeSet<Identity>,
    operations: BTreeMap<u8, String>,
    unresolved: Option<String>,
    provided: u16,
    contained: Option<String>,
}

impl Node {
    pub(crate) fn runtime(&mut self, op: RtOp) {
        let kinds: &[GrantKind] = match op {
            RtOp::Host(host) => {
                for kind in host.authority() {
                    if matches!(
                        host,
                        HostOp::ProvideClock
                            | HostOp::ProvideRandom
                            | HostOp::ProvideRandomTape
                            | HostOp::ProvideProcess
                            | HostOp::ProvideStdout
                            | HostOp::ProvideStderr
                            | HostOp::ProvideDir
                            | HostOp::ProvideNet
                            | HostOp::ProvideExec
                    ) {
                        self.provided |= 1 << *kind as u8;
                    }
                    self.operation(*kind, format!("{host:?}"));
                }
                return;
            }
            RtOp::Returnable(wait) => return self.runtime(wait.row()),
            RtOp::ArgsLen | RtOp::ArgsGet => &[GrantKind::Args],
            RtOp::Spawn { .. } | RtOp::SpawnUnit { .. } | RtOp::Join { .. } | RtOp::Cancel => {
                &[GrantKind::Tasks]
            }
            RtOp::SecretShare => &[GrantKind::Secret],
            RtOp::Closed(i) if hirsel_core_ir::CLOSED.get(usize::from(i)).is_none() => {
                self.unresolved = Some(format!(
                    "runtime function {i} is absent from the closed function table"
                ));
                return;
            }
            RtOp::CheckCancel
            | RtOp::ChannelNew { .. }
            | RtOp::ChannelSender
            | RtOp::Send { .. }
            | RtOp::TrySend { .. }
            | RtOp::Receive { .. }
            | RtOp::ReceiveUntil { .. }
            | RtOp::TryReceive { .. }
            | RtOp::LatestNew { .. }
            | RtOp::LatestHandle
            | RtOp::ConnNew { .. }
            | RtOp::ConnClient
            | RtOp::ConnSend { .. }
            | RtOp::ConnTrySend { .. }
            | RtOp::ConnReceive { .. }
            | RtOp::ConnTryReceive { .. }
            | RtOp::LatestPublish
            | RtOp::LatestRead
            | RtOp::SharedNew { .. }
            | RtOp::SharedShare
            | RtOp::SharedRelease { .. }
            | RtOp::DestId
            | RtOp::CompareBytes
            | RtOp::MapHashBytes
            | RtOp::MapHashWord
            | RtOp::MapHashStart
            | RtOp::MapHashAddWord
            | RtOp::MapHashAddBytes
            | RtOp::MapHashFinish
            | RtOp::MapPlaceWord
            | RtOp::MapPlaceBytes
            | RtOp::MapStrongWord
            | RtOp::MapStrongBytes
            | RtOp::MapStrongStart
            | RtOp::Sha256Blocks
            | RtOp::Sha512Blocks
            | RtOp::CauseNext
            | RtOp::CauseValue
            | RtOp::IsolateEnter
            | RtOp::IsolateLeave
            | RtOp::IsolateCatch
            | RtOp::StepEnter
            | RtOp::StepLeave
            | RtOp::StepCatch
            | RtOp::WorkSteps
            | RtOp::RootLimit
            | RtOp::LaunchWord
            | RtOp::LaunchTarget { .. }
            | RtOp::DescribeEmit
            | RtOp::Flow(_)
            | RtOp::Sha1Blocks
            | RtOp::ZeroBytes
            | RtOp::Xxh3Short
            | RtOp::Xxh3Stripes
            | RtOp::LaunchHandle
            | RtOp::Crc32c
            | RtOp::Crc64Nvme
            | RtOp::Crc32cCombine
            | RtOp::Crc64NvmeCombine
            | RtOp::Ed25519Verify
            | RtOp::P256Verify
            | RtOp::P256VerifyDer
            | RtOp::P384Verify
            | RtOp::P384VerifyDer
            | RtOp::RsaVerifyPkcs1Sha256
            | RtOp::RsaVerifyPssSha256
            | RtOp::Argon2id
            | RtOp::FrameRecord
            | RtOp::ReceiveRec { .. }
            | RtOp::Transfer { .. }
            | RtOp::Sha1Compress
            | RtOp::GlueMark
            | RtOp::GluePush
            | RtOp::GluePop
            | RtOp::GlueCut
            | RtOp::Closed(_) => &[],
        };
        for kind in kinds {
            self.operation(*kind, format!("{op:?}"));
        }
    }
    fn operation(&mut self, kind: GrantKind, operation: String) {
        self.operations
            .entry(kind as u8)
            .and_modify(|prior| {
                if operation < *prior {
                    prior.clone_from(&operation);
                }
            })
            .or_insert(operation);
    }
}

pub(crate) type Graph = BTreeMap<Identity, Node>;
pub(crate) type Facts = BTreeMap<PackageId, Result<BTreeMap<u8, String>, String>>;

fn reached(
    graph: &Graph,
    closure: &BTreeSet<PackageId>,
    roots: &BTreeSet<Identity>,
    mut detail: Option<&mut card::Reached>,
) -> Result<BTreeMap<u8, String>, String> {
    let mut witnesses = BTreeMap::new();
    let mut parent = BTreeMap::new();
    let mut queue = VecDeque::new();
    for root in roots {
        parent.insert(root, None);
        queue.push_back(root);
    }
    while let Some(id) = queue.pop_front() {
        let node = graph.get(id).ok_or_else(|| {
            format!(
                "what {} reaches cannot be resolved: missing checked instance",
                id.definition
            )
        })?;
        if let Some(cause) = &node.unresolved {
            return Err(format!(
                "what {} reaches cannot be resolved: {cause}",
                id.definition
            ));
        }
        if let Some(detail) = detail.as_deref_mut() {
            detail.kinds |= node
                .operations
                .keys()
                .fold(0, |mask, kind| mask | (1 << kind));
            detail.provided |= node.provided;
            detail.contained.extend(node.contained.iter().cloned());
        }
        for (&kind, operation) in &node.operations {
            witnesses.entry(kind).or_insert_with(|| {
                let mut path = vec![operation.clone()];
                let mut at = Some(id);
                while let Some(id) = at {
                    path.push(id.definition.to_string());
                    at = parent[id];
                }
                path.reverse();
                path.join(" -> ")
            });
        }
        for child in &node.children {
            if closure.contains(&child.definition.0.package) && !parent.contains_key(child) {
                parent.insert(child, Some(id));
                queue.push_back(child);
            }
        }
    }
    Ok(witnesses)
}

/// Check the current consumer rows against policy-independent facts. An older record
/// without a bound package's facts must fall through to the ordinary body walk.
pub(crate) fn enforce(facts: &Facts, inputs: &CorpusManifest) -> io::Result<bool> {
    let mut errors = Vec::new();
    for package in &inputs.packages {
        let Some(ns) = &package.namespace else {
            continue;
        };
        for (row, mask) in &ns.authority {
            let Some(found) = facts.get(&package.package) else {
                return Ok(false);
            };
            if let Err(error) = check_row(found, &package.package, row, *mask) {
                errors.push(error.to_string());
            }
        }
    }
    if errors.is_empty() {
        Ok(true)
    } else {
        Err(io::Error::other(errors.join("\nhirsel: ")))
    }
}

fn check_row(
    found: &Result<BTreeMap<u8, String>, String>,
    id: &PackageId,
    row: &str,
    mask: u16,
) -> io::Result<()> {
    let prefix = format!("{row}: {}@{}", id.name, id.version);
    let witnesses = found
        .as_ref()
        .map_err(|err| io::Error::other(format!("{prefix}: {err}")))?;
    let extra: Vec<_> = witnesses
        .iter()
        .filter(|(kind, _)| mask & (1 << **kind) == 0)
        .collect();
    if extra.is_empty() {
        return Ok(());
    }
    let names = &crate::native::recipe_grants::GRANT_NAMES;
    let denied = extra
        .iter()
        .map(|(kind, _)| names[**kind as usize])
        .collect::<Vec<_>>();
    let granted = names
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, name)| format!("{name:?}"))
        .collect::<Vec<_>>();
    let mut message = format!(
        "{prefix} reaches {}, which authority = [{}] does not grant",
        denied.join(", "),
        granted.join(", ")
    );
    for ((_, path), kind) in extra.into_iter().zip(denied) {
        message.push_str(&format!("\n  {kind}: {path}"));
    }
    Err(io::Error::other(message))
}

#[cfg(test)]
pub(crate) fn test_check(
    graph: &Graph,
    package: &PackageId,
    root: Identity,
    mask: u16,
) -> io::Result<()> {
    let facts = reached(graph, &[package.clone()].into(), &[root].into(), None);
    check_row(&facts, package, "row", mask)
}

struct Walk<'a> {
    product: &'a CheckProduct,
    resolver: Resolver,
    graph: Graph,
    pending: BTreeSet<Identity>,
}

impl Walk<'_> {
    fn expand(&mut self, id: &Identity) -> Result<(), String> {
        // PROG-6 rejects the selected build before this compiler marker can run.
        if id.definition.is_core("target.unsupported") {
            self.graph.insert(id.clone(), Node::default());
            return Ok(());
        }
        let args = self
            .resolver
            .arguments(id)
            .map_err(|_| "missing type argument".to_string())?;
        if hirsel_core_front::is_glue(&id.definition) {
            let (module, root) = hirsel_core_front::materialize_glue(id, &args)?;
            self.module(id, &module, root);
            return Ok(());
        }
        let body = self
            .product
            .demand_body(&id.definition)
            .map_err(|e| e.to_string())?;
        if !body.check.refusals.is_empty() || !body.incomplete.is_empty() {
            return Err("definition did not lower completely".into());
        }
        let root = body
            .check
            .function
            .ok_or("definition has no checked body")?;
        let module = body.transient_ir().map_err(|e| e.to_string())?;
        if let Some(template) = &body.check.template {
            let (module, root) =
                hirsel_core_front::materialize(&module, root, template, id, &args)?;
            self.module(id, &module, root);
        } else {
            self.module(id, &module, root);
        }
        Ok(())
    }

    fn module(&mut self, id: &Identity, module: &Module, root: FuncId) {
        // Retain the closure's local functions (including callable adapters) and
        // implicit finalizers. Do not specialize away branches for the build host.
        let (code, module) = hirsel_core_ir::normal::retain(module, root);
        let children = hirsel_core_materialize::children_for_slots(&module, &code.slots);
        self.resolver.add(&children);
        self.pending
            .extend(children.iter().map(|c| c.identity.clone()));
        let mut explicit = BTreeSet::new();
        for (index, fun) in module.functions.iter().enumerate() {
            let Some(body) = &fun.body else { continue };
            let mut node = Node::default();
            for inst in &body.insts {
                let callee = match inst {
                    Inst::FunctionRef { function, .. } | Inst::DescriptorRef { function, .. } => {
                        *function
                    }
                    Inst::Call { call } => match body.calls[call.index()].callee {
                        Callee::Direct(f) => f,
                        Callee::Indirect(_) => continue,
                        Callee::Runtime(op) => {
                            node.runtime(op);
                            continue;
                        }
                    },
                    _ => continue,
                };
                let target = module.identity(module.functions[callee.index()].instance);
                explicit.insert(target.clone());
                node.children.insert(target);
            }
            let own = if index == 0 {
                id.clone()
            } else {
                module.identity(fun.instance)
            };
            self.graph.insert(own, node);
        }
        let node = self.graph.get_mut(id).unwrap();
        node.children.extend(
            children
                .into_iter()
                .map(|c| c.identity)
                .filter(|c| !explicit.contains(c)),
        );
        for ty in &module.types {
            if let Some(f) = ty.finalizer {
                node.children
                    .insert(module.identity(module.functions[f.0 as usize].instance));
            }
        }
    }
}

pub(crate) fn collect(
    product: &CheckProduct,
    inputs: &CorpusManifest,
    mut cards: Option<&mut Cards>,
) -> io::Result<Facts> {
    if !inputs.packages.iter().any(|p| p.namespace.is_some()) {
        return Ok(Facts::new());
    }
    let mut walk = Walk {
        product,
        resolver: Resolver::default(),
        graph: Graph::new(),
        pending: BTreeSet::new(),
    };
    let mut exports = BTreeSet::new();
    let image = product.image.as_ref().map(|i| i.directory()).transpose()?;
    let mut test_sources = image
        .as_ref()
        .map(|i| i.test_sources.clone())
        .unwrap_or_default();
    test_sources.extend(
        product
            .files
            .iter()
            .filter(|f| f.test_source)
            .map(|f| f.file),
    );
    for (key, def) in product.index.definitions() {
        if !matches!(
            def.kind,
            hirsel_core_resolve::DefinitionKind::Function
                | hirsel_core_resolve::DefinitionKind::Constant
        ) {
            continue;
        }
        let path = product.index.definition_path(key);
        let id = Identity::plain(path);
        let package = &id.definition.0.package;
        if let Some(how) = product.index.module_containment(def.site.module) {
            let mut node = Node::default();
            if how == hirsel_core_resolve::Containment::Native {
                node.operation(GrantKind::Foreign, "Foreign".into());
            } else if cards.is_some() {
                let module = &id.definition.0.modules;
                node.contained = Some(format!(
                    "{}@{}/{}",
                    package.name,
                    package.version,
                    module.join("/")
                ));
            }
            walk.graph.insert(id.clone(), node);
        } else if package.name_is("core") || package.name_is("std") {
            continue;
        }
        if def.public
            && !def
                .site
                .source
                .is_some_and(|file| test_sources.contains(&file))
        {
            exports.insert(id.definition.clone());
        }
        if walk.graph.contains_key(&id) {
            continue;
        }
        if product.definitions.contains_key(&id.definition)
            || image
                .as_ref()
                .is_some_and(|i| i.definitions.contains_key(&id.definition))
        {
            let body = product.demand_body(&id.definition)?;
            if body.check.template.is_none()
                && (def.kind == hirsel_core_resolve::DefinitionKind::Function
                    || body.check.function.is_some()
                    || !body.incomplete.is_empty()
                    || !body.check.refusals.is_empty())
            {
                walk.pending.insert(id);
            }
        } else if def.kind == hirsel_core_resolve::DefinitionKind::Function {
            walk.graph.insert(
                id,
                Node {
                    unresolved: Some("definition has no checked body".into()),
                    ..Node::default()
                },
            );
        }
    }
    while let Some(id) = walk.pending.pop_first() {
        if !walk.graph.contains_key(&id)
            && let Err(e) = walk.expand(&id)
        {
            walk.graph.insert(
                id,
                Node {
                    unresolved: Some(e),
                    ..Node::default()
                },
            );
        }
    }
    let mut result = Facts::new();
    for bound in &inputs.packages {
        // A consumer can change which workspace rows bind the checked packages
        // without changing the request key. Store facts for every eligible package.
        if bound.namespace.is_none() {
            continue;
        }
        let mut closure = BTreeSet::from([crate::package("core"), crate::package("std")]);
        let mut pending = vec![&bound.package];
        while let Some(next) = pending.pop() {
            if closure.insert(next.clone())
                && let Some(p) = inputs.packages.iter().find(|p| &p.package == next)
            {
                pending.extend(&p.dependencies);
            }
        }
        let mut roots = BTreeSet::new();
        for (id, node) in &walk.graph {
            if id.definition.0.package == bound.package && exports.contains(&id.definition) {
                roots.insert(id.clone());
            }
            if !closure.contains(&id.definition.0.package) {
                roots.extend(
                    node.children
                        .iter()
                        .filter(|c| c.definition.0.package == bound.package)
                        .cloned(),
                );
            }
        }
        let facts = reached(&walk.graph, &closure, &roots, None);
        if let Some(cards) = cards.as_deref_mut() {
            let card = cards.entry(bound.package.clone()).or_default();
            card.set_reach(&walk.graph, &closure, &roots, &exports, &facts);
        }
        result.insert(bound.package.clone(), facts);
    }
    Ok(result)
}
