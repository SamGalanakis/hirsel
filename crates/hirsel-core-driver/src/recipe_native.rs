//! Publish the checked program and assemble its recipe with a local toolchain.

use super::std_release::StdSide;
use super::*;
use crate::code_admission::{
    AdmissionError, AdmissionLimits, NativeSemantics, VerifiedRelease, admit_local,
};
use crate::code_store::{CodeStore, ObjectKind};
use hirsel_core_ir::{
    FuncId, ItemId,
    address::{Binding, Group, builtin_semantics_number},
    decode::DecodeBudget,
    normal::{self, Annex, Site, Slot},
    release_recipe::{self as wire, BodyRef, GrantBinding, GrantKind, Launch, ObjectRef, Recipe},
    source::{ItemSource, SourceFile, SourceMap, anchored_items},
};
use std::collections::HashMap;

#[derive(Debug, Serialize)]
pub struct AssemblyRecord {
    pub identity: ContentHash,
    pub recipe: ContentHash,
    pub toolchain: ContentHash,
    pub digest: ContentHash,
}

impl AssemblyRecord {
    pub(super) fn new(recipe: ContentHash, toolchain: ContentHash, bytes: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let digest = ContentHash(Sha256::digest(bytes).into());
        let frame = [recipe.0, toolchain.0, digest.0].concat();
        let identity = ContentHash::of_parts("hirsel.assembly-record.v1", [frame.as_slice()]);
        Self {
            identity,
            recipe,
            toolchain,
            digest,
        }
    }
}

pub(super) struct Unit {
    code: BodyRef,
    pub(super) module: Module,
    annex: Annex,
    builtins: Vec<u32>,
}

impl Unit {
    /// The owner of the unit's private callable variants: its body.
    pub(super) fn owner(&self) -> io::Result<ContentHash> {
        key(
            "hirsel.recipe-release-body.v1",
            &(self.code.body.hash, self.code.annex.hash, &self.builtins),
        )
    }
}

/// A reached instance's unit, as a recipe names its body and annex.
pub(super) fn prepare_unit(
    original: &Module,
    function: FuncId,
    sources: &hirsel_core_ir::source::SourcePositions<'_>,
) -> Unit {
    let (body, annex, module) = normal::prepare_assembly(original, function, sources);
    unit(original, body, annex, module)
}

fn unit(original: &Module, body: normal::EncodedBody, annex: Annex, module: Module) -> Unit {
    let builtins = body
        .slots
        .iter()
        .map(|slot| slot.identity(original))
        .filter_map(|identity| builtin_semantics_number(&identity.definition))
        .collect();
    let code = BodyRef {
        body: ObjectRef {
            hash: ObjectKind::Body.hash(&body.bytes),
            len: body.bytes.len() as u64,
        },
        annex: {
            let bytes = annex.encode();
            ObjectRef {
                hash: ObjectKind::Annex.hash(&bytes),
                len: bytes.len() as u64,
            }
        },
    };
    Unit {
        code,
        module,
        annex,
        builtins,
    }
}

pub struct Publication {
    pub(crate) root: ContentHash,
    pub(crate) recipe: Recipe,
    units: BTreeMap<Identity, Unit>,
    addresses: BTreeMap<Identity, ContentHash>,
}

const GRANT_NAMES: [&str; 12] = [
    "Dir", "Net", "Clock", "Random", "Stdout", "Stderr", "Stdin", "Env", "Process", "Tasks",
    "Args", "Secret",
];

pub(super) fn kind(name: &str) -> Option<u8> {
    GRANT_NAMES
        .iter()
        .position(|kind| *kind == name)
        .map(|i| i as u8)
}

impl CheckProduct {
    pub fn publish_release(
        &self,
        entry: &DefPath,
        reached: &Reachable<'_>,
        options: &NativeOptions,
        store: &CodeStore,
        n_place: u64,
    ) -> Result<Publication, NativeStop> {
        let instances = reached.require()?;
        let decl = self.entry_decl(entry)?;
        let entry = Identity::plain(entry.clone());
        let items = instances
            .iter()
            .map(|i| reached.materialized.get(i).ir().map(anchored_items))
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let sources = self.native_sources(&items)?;
        let sources = sources.positions();
        let mut units = BTreeMap::new();
        for identity in instances {
            let instance = reached.materialized.get(identity);
            let original = instance.ir()?;
            let (body, mut annex, module) =
                normal::prepare_assembly(original, instance.function, &sources);
            if identity == &entry {
                annex.1 = Some(decl.parameters.iter().map(|p| p.name.clone()).collect());
            }
            let builtins = body
                .slots
                .iter()
                .map(|slot| slot.identity(original))
                .filter_map(|identity| builtin_semantics_number(&identity.definition))
                .collect();
            let code = BodyRef {
                body: store.put(ObjectKind::Body, &body.bytes)?,
                annex: store.put(ObjectKind::Annex, &annex.encode())?,
            };
            units.insert(
                identity.clone(),
                Unit {
                    code,
                    module,
                    annex,
                    builtins,
                },
            );
        }
        let mut groups = Vec::new();
        let mut pending = Vec::new();
        let addresses = reached
            .materialized
            .groups(instances, None, [entry.clone()], |_, bytes| {
                pending.push(bytes);
            })
            .ok_or_else(|| NativeStop::Entry("incomplete or inconsistent recipe closure".into()))?;
        for bytes in pending {
            groups.push(store.put(ObjectKind::Group, &bytes)?);
        }
        groups.sort_unstable_by_key(|group| group.hash);
        units.retain(|identity, _| addresses.contains_key(identity));
        let mut bodies = units.values().map(|unit| unit.code).collect::<Vec<_>>();
        bodies.sort_unstable_by_key(|body| body.body.hash);
        let launch = Launch {
            backend: match options.backend {
                Backend::Cranelift => wire::Backend::Cranelift,
                Backend::Llvm => wire::Backend::Llvm,
            },
            target: options.target.clone(),
            root_limit: options.root_limit,
            n_place,
            exit_status: decl.exit,
            grants: bindings(decl, options)?,
        };
        let recipe = Recipe {
            address: addresses[&entry],
            entry,
            groups,
            bodies,
            launch,
        };
        let root = store.put(ObjectKind::Recipe, &recipe.encode())?.hash;
        Ok(Publication {
            root,
            recipe,
            units,
            addresses,
        })
    }
}

fn grant_name(kind: GrantKind) -> &'static str {
    GRANT_NAMES[kind as usize]
}

fn net_rule(rule: &hirsel_core_net::GrantRule) -> String {
    use hirsel_core_net::{GrantRule, TlsTrust};
    match rule {
        GrantRule::Address(rule) => {
            let direction = if rule.dir == 0 { "Connect" } else { "Listen" };
            let ip = if rule.family == 0 {
                std::net::IpAddr::V4(std::net::Ipv4Addr::from(rule.lo as u32))
            } else {
                std::net::IpAddr::V6(std::net::Ipv6Addr::from(
                    (u128::from(rule.hi) << 64) | u128::from(rule.lo),
                ))
            };
            format!(
                "{direction} {ip}/{} {}-{}",
                rule.bits, rule.first, rule.last
            )
        }
        GrantRule::Tls(rule) => {
            let (first, last) = rule.ports();
            let trust = match rule.trust() {
                TlsTrust::System => "System".to_owned(),
                TlsTrust::Roots(hash) => format!("Roots=sha256:{}", selection(ContentHash(hash))),
            };
            format!("Tls {} {first}-{last} {trust}", rule.pattern_text())
        }
        GrantRule::Resolve(rule) => format!("Resolve {}", rule.pattern_text()),
    }
}

fn bindings(decl: &EntryDecl, options: &NativeOptions) -> Result<Vec<GrantBinding>, NativeStop> {
    let mut next = [0usize; GRANT_NAMES.len()];
    let mut bound = Vec::new();
    for (ordinal, param) in decl.parameters.iter().enumerate() {
        let tag = param
            .ty
            .strip_prefix("core@1/core/")
            .and_then(kind)
            .ok_or_else(|| {
                NativeStop::Entry(format!("unsupported provider parameter {}", param.ty))
            })?;
        let kind = GrantKind::from_tag(tag).expect("runtime grant kind");
        let index = next[usize::from(tag)];
        next[usize::from(tag)] += 1;
        let values = match kind {
            GrantKind::Dir => options
                .directories
                .get(index)
                .iter()
                .map(|s| (*s).clone())
                .collect(),
            GrantKind::Net if index == 0 => options.net_rules.iter().map(net_rule).collect(),
            GrantKind::Env if index == 0 => options
                .env_patterns
                .iter()
                .map(hirsel_core_net::EnvPatterns::text)
                .collect(),
            GrantKind::Secret => options
                .secret_grants
                .iter()
                .find(|grant| grant.parameter == param.name)
                .map_or_else(Vec::new, |grant| {
                    vec![grant.source.clone(), grant.patterns.clone()]
                }),
            GrantKind::Args => vec![grant_name(kind).to_owned()],
            _ if index == 0 && options.grant_mask & (1 << tag) != 0 => {
                vec![grant_name(kind).to_owned()]
            }
            _ => Vec::new(),
        };
        bound.push(GrantBinding {
            ordinal: ordinal as u32,
            kind,
            values,
        });
    }
    if options.directories.len() > next[GrantKind::Dir as usize]
        || (!options.net_rules.is_empty() && next[GrantKind::Net as usize] == 0)
        || GRANT_NAMES
            .iter()
            .enumerate()
            .any(|(tag, _)| options.grant_mask & (1 << tag) != 0 && next[tag] == 0)
        || options.secret_grants.iter().any(|grant| {
            !decl
                .parameters
                .iter()
                .any(|p| p.name == grant.parameter && p.ty == "core@1/core/Secret")
        })
    {
        return Err(NativeStop::Entry(
            "grant has no matching entry parameter".into(),
        ));
    }
    Ok(bound)
}

pub(crate) fn display_grants(recipe: &Recipe) -> Vec<String> {
    let mut grants = Vec::new();
    for grant in &recipe.launch.grants {
        match grant.kind {
            GrantKind::Dir | GrantKind::Net | GrantKind::Env => grants.extend(
                grant
                    .values
                    .iter()
                    .map(|v| format!("{}={v}", grant_name(grant.kind))),
            ),
            GrantKind::Secret if !grant.values.is_empty() => grants.push("Secret".into()),
            GrantKind::Args => {}
            _ => grants.extend(grant.values.iter().cloned()),
        }
    }
    grants.sort();
    grants
}

fn launch_options(
    recipe: &Recipe,
    names: &[String],
    options: &NativeOptions,
) -> Result<(NativeOptions, EntryDecl), NativeStop> {
    let mut options = options.clone();
    options.target.clone_from(&recipe.launch.target);
    options.backend = match recipe.launch.backend {
        wire::Backend::Cranelift => Backend::Cranelift,
        wire::Backend::Llvm => Backend::Llvm,
    };
    options.root_limit = recipe.launch.root_limit;
    options.directories.clear();
    options.grant_mask = 0;
    options.net_rules.clear();
    options.secret_grants.clear();
    options.env_patterns = None;
    if names.len() != recipe.launch.grants.len() {
        return Err(NativeStop::Entry(
            "grant bindings do not match main parameters".into(),
        ));
    }
    let mut params = Vec::new();
    let mut next = [0usize; GRANT_NAMES.len()];
    for (i, (binding, name)) in recipe.launch.grants.iter().zip(names).enumerate() {
        let kind = binding.kind;
        let index = next[kind as usize];
        next[kind as usize] += 1;
        let values = &binding.values;
        let invalid = || NativeStop::Entry(format!("invalid recipe binding for parameter {i}"));
        if i != binding.ordinal as usize {
            return Err(invalid());
        }
        match kind {
            GrantKind::Dir => match values.as_slice() {
                [] => {}
                [path] if options.directories.len() == index => {
                    options.directories.push(path.clone())
                }
                _ => return Err(invalid()),
            },
            GrantKind::Net if index == 0 && !values.is_empty() => {
                options.net_rules = values
                    .iter()
                    .map(|v| hirsel_core_net::GrantRule::parse(v))
                    .collect::<Result<_, _>>()
                    .map_err(NativeStop::Entry)?;
                options.grant_mask |= 1 << kind as u8;
            }
            GrantKind::Env => match values.as_slice() {
                [] => {}
                [patterns] if index == 0 => {
                    options.env_patterns =
                        Some(hirsel_core_net::EnvPatterns::parse(patterns).ok_or_else(invalid)?);
                    options.grant_mask |= 1 << kind as u8;
                }
                _ => return Err(invalid()),
            },
            GrantKind::Secret => match values.as_slice() {
                [] => {}
                [source, patterns] => options.secret_grants.push(
                    hirsel_core_net::SecretGrant::new(name, source, patterns)
                        .ok_or_else(invalid)?,
                ),
                _ => return Err(invalid()),
            },
            GrantKind::Args if values.as_slice() == ["Args"] => {}
            _ if values.is_empty() => {}
            _ if index == 0 && values.as_slice() == [grant_name(kind)] => {
                options.grant_mask |= 1 << kind as u8
            }
            _ => return Err(invalid()),
        }
        params.push(EntryParameter {
            ty: format!("core@1/core/{}", grant_name(kind)),
            name: name.clone(),
        });
    }
    Ok((
        options,
        EntryDecl {
            exit: recipe.launch.exit_status,
            parameters: params,
        },
    ))
}

#[cfg(test)]
fn read_object(
    store: &CodeStore,
    kind: ObjectKind,
    expected: wire::ObjectRef,
) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut file = store.open(kind, expected.hash)?;
    if file.metadata()?.len() != expected.len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "code object length mismatch",
        ));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(expected.len.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != expected.len || kind.hash(&bytes) != expected.hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "code object hash mismatch",
        ));
    }
    Ok(bytes)
}

impl Publication {
    pub fn root(&self) -> ContentHash {
        self.root
    }

    pub fn recipe(&self) -> &Recipe {
        &self.recipe
    }

    pub fn open(
        store: &CodeStore,
        root: ContentHash,
        limits: &AdmissionLimits,
    ) -> Result<Self, AdmissionError> {
        admit_local(store, root, limits, &NativeSemantics).map(Self::from_verified)
    }

    /// Consume admission's decoded data. Host callers retain the collection
    /// lease until the assembly record and image are durably rooted.
    pub fn from_verified(release: VerifiedRelease) -> Self {
        let (root, recipe, groups, decoded) = release.into_parts();
        let mut bodies: BTreeMap<_, _> = recipe
            .bodies
            .iter()
            .copied()
            .zip(decoded)
            .map(|(code, (body, annex))| (code.body.hash, (code, body, annex)))
            .collect();
        let mut units = BTreeMap::new();
        let mut addresses = BTreeMap::new();
        let mut budget = DecodeBudget::unlimited();
        for (reference, group) in recipe.groups.iter().zip(groups) {
            for (i, member) in group.members.into_iter().enumerate() {
                let (code, body, annex) = bodies.remove(&member.body).expect("admitted body");
                let identity = body
                    .identity(FuncId(0), &mut budget)
                    .expect("admitted identity");
                let address = Group::member_address(reference.hash, i as u32);
                addresses.insert(identity.clone(), address);
                let builtins = body
                    .slots
                    .iter()
                    .zip(member.slots)
                    .filter_map(|(slot, binding)| {
                        let Binding::Builtin(number) = binding else {
                            return None;
                        };
                        let target = match slot {
                            Slot::Builtin(definition) => Identity::plain(definition.clone()),
                            Slot::External(function) => body
                                .identity(*function, &mut budget)
                                .expect("admitted slot identity"),
                        };
                        let address =
                            hirsel_core_ir::builtin_address(&target).expect("admitted builtin");
                        addresses.insert(target, address);
                        Some(number)
                    })
                    .collect();
                units.insert(
                    identity,
                    Unit {
                        code,
                        module: body.module,
                        annex,
                        builtins,
                    },
                );
            }
        }
        Self {
            root,
            recipe,
            units,
            addresses,
        }
    }
}

fn site_item(site: &Site) -> DefPath {
    DefPath(hirsel_core_ids::ItemPath {
        package: PackageId {
            name: "source".into(),
            version: "1".into(),
        },
        modules: vec![site.file.clone()],
        name: format!("{}_{}", site.line, site.column),
    })
}

/// The anchor of `site` in a unit whose source items are `items`, adding its
/// item on first sight. Keyed by the borrowed site, so each distinct site
/// builds its DefPath once.
fn anchor<'a>(
    seen: &mut HashMap<(&'a str, u32, u32), ItemId>,
    items: &mut Vec<DefPath>,
    site: Option<&'a Site>,
) -> Option<hirsel_core_ir::Anchor> {
    let site = site?;
    let index = *seen
        .entry((site.file.as_str(), site.line, site.column))
        .or_insert_with(|| {
            let index = ItemId(items.len() as u32);
            items.push(site_item(site));
            index
        });
    Some(hirsel_core_ir::Anchor {
        item: index,
        start: 0,
        end: 0,
    })
}

/// Bind each unit's annex sites to source items, and the source map of every
/// site the units and `extra` (a separately lowered std's sites) name.
pub(super) fn bind_annexes(
    units: &mut BTreeMap<Identity, Unit>,
    extra: &BTreeMap<String, Vec<(u32, u32)>>,
) -> Result<SourceMap, NativeStop> {
    let mut points = BTreeMap::<String, BTreeSet<(u32, u32)>>::new();
    for (file, at) in extra {
        points
            .entry(file.clone())
            .or_default()
            .extend(at.iter().copied());
    }
    for unit in units.values() {
        for hints in unit.annex.0.iter().flatten() {
            for site in hints
                .instructions
                .iter()
                .chain(&hints.terminators)
                .flatten()
            {
                let at = (site.line, site.column);
                match points.get_mut(&site.file) {
                    Some(file) => {
                        file.insert(at);
                    }
                    None => {
                        points.insert(site.file.clone(), BTreeSet::from([at]));
                    }
                }
            }
        }
    }
    let mut sources = SourceMap::default();
    for (path, points) in points {
        let file = u32::try_from(sources.files.len())
            .map_err(|_| NativeStop::Entry("source file table exceeds u32".into()))?;
        let points: Vec<_> = points.into_iter().collect();
        for (offset, &(line, column)) in points.iter().enumerate() {
            let item = site_item(&Site {
                file: path.clone(),
                line,
                column,
            });
            sources.items.insert(
                item,
                ItemSource {
                    file,
                    file_offset: u32::try_from(offset).map_err(|_| {
                        NativeStop::Entry("source position table exceeds u32".into())
                    })?,
                },
            );
        }
        sources.files.push(SourceFile {
            path: path.clone(),
            text: String::new(),
        });
        sources.points.insert(path, points);
    }
    for unit in units.values_mut() {
        let module = &mut unit.module;
        let mut items = HashMap::new();
        for (function, hints) in module.functions.iter_mut().zip(&unit.annex.0) {
            let (Some(body), Some(hints)) = (function.body.as_mut(), hints) else {
                continue;
            };
            for (at, site) in body.inst_anchors.iter_mut().zip(&hints.instructions) {
                *at = anchor(&mut items, &mut module.items, site.as_ref());
            }
            for (block, site) in body.blocks.iter_mut().zip(&hints.terminators) {
                block.anchor = anchor(&mut items, &mut module.items, site.as_ref());
            }
            for (call, &cold) in body.calls.iter_mut().zip(&hints.cold_calls) {
                call.cold = cold;
            }
            for (edge, &cold) in body.edges.iter_mut().zip(&hints.cold_edges) {
                edge.cold = cold;
            }
            body.callable_parameters = hints
                .callable_parameters
                .iter()
                .copied()
                .map(hirsel_core_ir::ValueId)
                .collect();
        }
    }
    Ok(sources)
}

fn object(
    store: &Store,
    options: &NativeOptions,
    key: ContentHash,
    instance: &Identity,
    module: &Module,
) -> Result<ObjectProduct, NativeStop> {
    let (held, object, compiled) = match crate::native_object::load(store, key)? {
        Some((bare, object, file)) => ((bare, file), object, false),
        None => {
            let object = emit_unit(module, || module.link_symbols(), options, key, instance)
                .map_err(|message| NativeStop::Emit {
                    definition: instance.definition.clone(),
                    message,
                })?;
            (
                crate::native_object::publish(store, key, &object)?,
                object,
                true,
            )
        }
    };
    Ok(ObjectProduct::held(
        instance.clone(),
        store,
        held,
        object,
        None,
        compiled,
    ))
}

impl Publication {
    pub fn assemble(
        self,
        options: &NativeOptions,
        counters: crate::reach::ReachCounters,
    ) -> Result<BuildProduct, NativeStop> {
        self.assemble_with(options, counters, None)
    }

    /// [`Self::assemble`], with the program's plain std units taken from
    /// `std` (a separately lowered release std) when given: the program holds
    /// none of their bodies and calls each by its exported symbol.
    pub(crate) fn assemble_with(
        mut self,
        options: &NativeOptions,
        counters: crate::reach::ReachCounters,
        std: Option<StdSide>,
    ) -> Result<BuildProduct, NativeStop> {
        let entry_identity = self.recipe.entry.clone();
        if let Some(std) = &std {
            self.units
                .retain(|identity, _| identity == &entry_identity || !std.roots.contains(identity));
        }
        let no_points = BTreeMap::new();
        let sources = bind_annexes(
            &mut self.units,
            std.as_ref().map_or(&no_points, |std| &std.points),
        )?;
        let entry = &self.recipe.entry;
        let root = &self.units[entry];
        let names = root.annex.1.as_deref().ok_or_else(|| {
            NativeStop::Entry("recipe entry has no startup parameter names".into())
        })?;
        let (options, decl) = launch_options(&self.recipe, names, options)?;
        let (abi, cleanup) = bound_entry_abi(&decl, &entry.definition, &root.module, FuncId(0))
            .map_err(NativeStop::Entry)?;
        let target = hirsel_core_layout::Target::from_triple(&options.target)
            .filter(|_| options.backend.supports_target(&options.target))
            .ok_or_else(|| {
                NativeStop::Entry(format!("unsupported recipe target {}", options.target))
            })?;
        let (tools, link) = match &options.observed {
            Some(observed) => observed.as_ref().clone(),
            None => {
                crate::tool_inputs::toolchain(&options.link, &options.target, &options.runtime)?
            }
        };
        // Runtime archive bytes are a final-link input. Emission reads the ABI
        // and target description already built into the compiler and backends.
        let emission = environment(&options, ContentHash::default())?;
        let emitter = key(
            "hirsel.recipe-emitter.v1",
            &(
                &emission,
                target.cpu(),
                target.llvm_features(),
                options.optimize
                    && matches!(options.backend, Backend::Llvm)
                    && std::env::var_os("HIRSEL_LLVM_PRINTED_ORACLE").is_some(),
            ),
        )?;
        use std::os::unix::ffi::OsStrExt;
        let fingerprint = key(
            "hirsel.assembly-environment.v1",
            &(
                std.as_ref().map(|std| std.key),
                // The bodies the program imports shape its code.
                std.as_ref().filter(|std| !std.lent.is_empty()).map(|std| {
                    ContentHash::of_parts("hirsel.core-std-lent.v1", [std.lent.as_slice()])
                }),
                emitter,
                tools.image,
                tools.runtime,
                tools.environment,
                link.identity,
                DIRECT_LINK_SUPPORT_LIBS,
                options.recording,
                &options.replay,
                options
                    .logical_charge_report
                    .as_ref()
                    .map(|path| path.as_os_str().as_bytes()),
            ),
        )?;
        let store = Store::new(&options.store)?;
        let mut release_sites = Vec::new();
        let (mut objects, closed_image, launch_digest_unused) =
            if matches!(options.backend, Backend::Llvm) && options.optimize {
                let reached = self.units.keys().cloned().collect::<Vec<_>>();
                let mut modules = self
                    .units
                    .values()
                    .map(|unit| Cow::Borrowed(&unit.module))
                    .collect();
                let body_keys = crate::callable_variants::prepare_inputs(
                    &mut modules,
                    &reached,
                    crate::callable_variants::Roots::Entry(&entry.definition),
                    options.instance_limit,
                    |identity| {
                        let owner = self.units[identity].owner()?;
                        Ok((
                            FuncId(0),
                            owner,
                            !identity.closures.is_empty() || !identity.dictionaries.is_empty(),
                        ))
                    },
                )?;
                let joined = release::join_modules(entry, &reached, &modules, |_| FuncId(0))?;
                let emitted = release::emit_release_image(
                    entry,
                    &reached,
                    &body_keys,
                    joined,
                    &options,
                    tools.runtime,
                    &store,
                    link.uses_hirsel_launcher(),
                    std.as_ref()
                        .map(|std| (&std.summaries, std.key, std.lent.as_slice())),
                )?;
                release_sites = emitted.release_sites;
                let mut objects = emitted.objects;
                if let Some(std) = std {
                    objects.extend(std.objects);
                }
                (objects, emitted.closed_image, emitted.launch_digest_unused)
            } else {
                let objects = self
                    .units
                    .iter()
                    .map(|(identity, unit)| {
                        let object_key = key(
                            "hirsel.recipe-native-object.v1",
                            &(
                                emitter,
                                unit.code.body.hash,
                                unit.code.annex.hash,
                                &unit.builtins,
                            ),
                        )?;
                        object(&store, &options, object_key, identity, &unit.module)
                    })
                    .collect::<Result<Vec<_>, NativeStop>>()?;
                (objects, false, false)
            };
        let cleanup = if let Some(module) = cleanup {
            let identity = cleanup_identity(&module);
            let cleanup_key = key(
                "hirsel.recipe-entry-cleanup.v1",
                &(emitter, root.code.body.hash),
            )?;
            objects.push(object(&store, &options, cleanup_key, &identity, &module)?);
            Some(identity.symbol())
        } else {
            None
        };
        let plan = crate::native_entry::Plan::new(
            entry.symbol(),
            entry.definition.to_string(),
            &abi,
            cleanup,
            launch_digest_unused,
            &options,
        )?
        .with_release_sites(&release_sites);
        let result = link_objects(
            &plan,
            &objects,
            None,
            &options,
            ContentHash::default(),
            || Ok(sources),
            tools.image,
            link,
            closed_image,
            counters,
            |_| Ok(0),
            self.recipe.address,
            || {
                Ok(self
                    .addresses
                    .into_iter()
                    .map(|(id, address)| (id, hirsel_core_ir::code_revision(address)))
                    .collect())
            },
            Some((self.root, fingerprint)),
        )?;
        match result {
            Linked::Built(product) => Ok(product),
            Linked::Damaged(_, _, _) => Err(NativeStop::Link(
                "recipe object cache changed during link".into(),
            )),
        }
    }
}

pub(crate) fn parse_root(text: &str) -> Result<ContentHash, &'static str> {
    const ERROR: &str = "recipe hash must contain 64 lowercase hexadecimal digits";
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ERROR);
    }
    let mut hash = [0; 32];
    for (i, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|_| ERROR)?;
    }
    Ok(ContentHash(hash))
}

#[cfg(test)]
impl Publication {
    /// The B4 fixture installs a second version of the inline widening
    /// operation. The serialized body does not change, just the version
    /// binding and the backend's emitted constant in this closed fixture.
    pub(crate) fn bump_inline_builtin(&mut self, store: &CodeStore) {
        let unit = self.units.get_mut(&self.recipe.entry).unwrap();
        let builtin = unit.module.functions[0]
            .body
            .as_ref()
            .unwrap()
            .insts
            .iter()
            .filter_map(|instruction| match instruction {
                hirsel_core_ir::Inst::BuiltinUse { definition } => Some(definition),
                _ => None,
            })
            .position(|definition| definition.is_core("float.widen"))
            .unwrap();
        let mut old = 0;
        for instruction in unit.module.functions[0]
            .body
            .as_mut()
            .unwrap()
            .insts
            .iter_mut()
        {
            if let hirsel_core_ir::Inst::Const { bits, .. } = instruction
                && *bits == 7.0f32.to_bits() as u64
            {
                *bits = 8.0f32.to_bits() as u64;
                old += 1;
            }
        }
        assert_eq!(old, 1, "B4 modifies the designated inline input");
        assert_eq!(self.recipe.groups.len(), 1);
        let frame = read_object(store, ObjectKind::Group, self.recipe.groups[0]).unwrap();
        let mut group = Group::decode(&frame, &mut DecodeBudget::unlimited()).unwrap();
        assert_eq!(group.members.len(), 1);
        let Binding::Builtin(number) = &mut group.members[0].slots[builtin] else {
            panic!("B4 expects the inline widening builtin");
        };
        assert_eq!(*number, unit.builtins[builtin]);
        *number += 1;
        unit.builtins[builtin] = *number;
        let group = store.put(ObjectKind::Group, &group.encode()).unwrap();
        self.recipe.groups[0] = group;
        self.recipe.address = Group::member_address(group.hash, 0);
        self.addresses
            .insert(self.recipe.entry.clone(), self.recipe.address);
        self.root = store
            .put(ObjectKind::Recipe, &self.recipe.encode())
            .unwrap()
            .hash;
    }
}
