//! Publish the checked program and assemble its recipe with a local toolchain.

use super::std_release::{StdSide, std_side};
use super::*;
use crate::code_admission::{
    AdmissionError, AdmissionLimits, NativeSemantics, VerifiedRelease, admit_local,
};
use crate::code_store::{CodeStore, ObjectKind};
use crate::foreign_archive::LinkArchives;
use hirsel_core_ir::{
    FuncId, ItemId,
    address::{Binding, Group, builtin_semantics_number, foreign_address},
    decode::DecodeBudget,
    normal::{self, Annex, Site, Slot},
    release_recipe::{
        self as wire, BodyRef, ForeignFunction, ForeignLeaf, ForeignTarget, GrantBinding,
        GrantKind, Launch, ObjectRef, Recipe,
    },
    source::{ItemSource, SourceFile, SourceMap, anchored_items},
};
use hirsel_core_resolve::{ForeignArchiveCapture, ForeignModuleCapture};
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

#[derive(Clone)]
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

/// Hash the current reached annexes in body order for a cached test pass.
pub(crate) fn test_site_digest(
    product: &CheckProduct,
    test: &DefPath,
    limit: usize,
) -> Result<ContentHash, NativeStop> {
    use sha2::{Digest, Sha256};

    let reached = product.reachable(test, limit)?;
    let instances = reached.require()?;
    let items = instances
        .iter()
        .map(|identity| reached.materialized.get(identity).ir().map(anchored_items))
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let sources = product.native_sources(&items)?;
    let positions = sources.positions();
    let mut hashes = BTreeSet::new();
    for identity in instances {
        let instance = reached.materialized.get(identity);
        let unit = prepare_unit(instance.ir()?, instance.function, &positions);
        hashes.insert((unit.code.body.hash, unit.code.annex.hash));
    }
    let mut hash = Sha256::new();
    for (_, annex) in hashes {
        hash.update(annex.0);
    }
    Ok(ContentHash(hash.finalize().into()))
}

pub struct Publication {
    pub(crate) root: ContentHash,
    pub(crate) recipe: Recipe,
    pub(super) units: BTreeMap<Identity, Unit>,
    addresses: BTreeMap<Identity, ContentHash>,
    link_archives: LinkArchives,
}

const GRANT_NAMES: [&str; 13] = [
    "Dir", "Net", "Clock", "Random", "Stdout", "Stderr", "Stdin", "Env", "Process", "Tasks",
    "Args", "Secret", "Foreign",
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
        reached.require()?;
        if options.recording
            && let Some(stop) = reached.recording_stop(self, entry)?
        {
            return Err(stop);
        }
        let decl = self.entry_decl(entry)?;
        let foreign = self.release_foreign(reached, options, decl, store)?;
        let entry = Identity::plain(entry.clone());
        let names = decl.parameters.iter().map(|p| p.name.clone()).collect();
        let (units, groups, addresses) = self.release_units(&entry, reached, store, Some(names))?;
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
            grants: foreign.grants,
        };
        let recipe = Recipe {
            address: addresses[&entry],
            entry,
            groups,
            bodies,
            foreign_modules: foreign.modules,
            foreign_archives: foreign.archives,
            launch,
        };
        let root = store.put(ObjectKind::Recipe, &recipe.encode())?.hash;
        Ok(Publication {
            root,
            recipe,
            units,
            addresses,
            link_archives: foreign.link_archives,
        })
    }

    /// The units of `entry`'s reached closure, each body and annex put in
    /// `store`, with its groups and each unit's address; `names` are the
    /// entry's startup parameter names when it is a program entry.
    #[allow(clippy::type_complexity)]
    pub(super) fn release_units(
        &self,
        entry: &Identity,
        reached: &Reachable<'_>,
        store: &CodeStore,
        names: Option<Vec<String>>,
    ) -> Result<
        (
            BTreeMap<Identity, Unit>,
            Vec<hirsel_core_ir::release_recipe::ObjectRef>,
            BTreeMap<Identity, ContentHash>,
        ),
        NativeStop,
    > {
        let instances = reached.require()?;
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
            if identity == entry {
                annex.1.clone_from(&names);
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
            .groups(
                instances,
                None,
                [entry.clone()],
                &reached.materialized.foreign_addresses,
                |_, bytes| pending.push(bytes),
            )
            .ok_or_else(|| NativeStop::Entry("incomplete or inconsistent recipe closure".into()))?;
        for bytes in pending {
            groups.push(store.put(ObjectKind::Group, &bytes)?);
        }
        groups.sort_unstable_by_key(|group| group.hash);
        units.retain(|identity, _| addresses.contains_key(identity));
        Ok((units, groups, addresses))
    }
}

struct ForeignRelease {
    modules: Vec<ForeignLeaf>,
    archives: Vec<ObjectRef>,
    grants: Vec<GrantBinding>,
    link_archives: LinkArchives,
}

fn archive_ref(row: &ForeignArchiveCapture) -> ObjectRef {
    ObjectRef {
        hash: ContentHash(row.pin),
        len: row.bytes.len() as u64,
    }
}

impl CheckProduct {
    fn release_foreign(
        &self,
        reached: &Reachable<'_>,
        options: &NativeOptions,
        decl: &EntryDecl,
        store: &CodeStore,
    ) -> Result<ForeignRelease, NativeStop> {
        let mut selected = BTreeMap::new();
        let target = options.target.as_str();
        for interface in &options.foreign_grants {
            if !self
                .foreign_headers
                .values()
                .any(|(path, _)| path.definition_identity() == *interface)
            {
                return Err(NativeStop::Entry(format!(
                    "unknown grant Foreign={interface}"
                )));
            }
        }
        let grants = bindings(decl, options, |interface| {
            let mut functions = self
                .foreign_headers
                .iter()
                .filter(|(_, (path, _))| path.definition_identity() == interface);
            let (function, _) = functions
                .next()
                .ok_or_else(|| NativeStop::Entry(format!("unknown grant Foreign={interface}")))?;
            if functions.any(|(other, _)| {
                other.0.package != function.0.package || other.0.modules != function.0.modules
            }) {
                return Err(NativeStop::Entry(format!(
                    "FFI-4: two foreign modules name {interface}"
                )));
            }
            let (module, capture) = self.foreign_capture(function)?;
            let row = select_archive(&module, capture, target)?;
            let object = archive_ref(row);
            selected.insert(object.hash, row);
            Ok(object)
        })?;

        let mut modules = self.reached_foreign_modules(reached)?;
        let native_link = if modules.is_empty() || options.runtime.as_os_str().is_empty() {
            None
        } else {
            Some(crate::tool_inputs::toolchain(&options.link, target, &options.runtime)?.1)
        };
        let (reached_archives, link_archives) = self.check_foreign_archives(
            &modules,
            target,
            native_link
                .as_ref()
                .map(|link| (options.runtime.as_path(), link)),
        )?;
        selected.extend(reached_archives);
        for leaf in modules.values_mut() {
            leaf.functions
                .sort_by_cached_key(|f| normal::encode_identity(&f.identity));
        }

        let archives = selected
            .into_values()
            .map(|row| {
                let object = store.put(ObjectKind::ForeignArchive, &row.bytes)?;
                if object.hash.0 != row.pin {
                    return Err(AdmissionError::Hash(ContentHash(row.pin)).into());
                }
                Ok(object)
            })
            .collect::<Result<_, NativeStop>>()?;
        let mut modules = modules.into_values().collect::<Vec<_>>();
        modules.sort_by_cached_key(|leaf| {
            normal::encode_identity(&Identity::plain(leaf.module.clone()))
        });
        Ok(ForeignRelease {
            modules,
            archives,
            grants,
            link_archives,
        })
    }

    pub(super) fn reached_foreign_modules(
        &self,
        reached: &Reachable<'_>,
    ) -> Result<BTreeMap<String, ForeignLeaf>, NativeStop> {
        self.foreign_modules(&reached.materialized.foreign)
    }

    fn foreign_modules(
        &self,
        functions: &BTreeSet<Identity>,
    ) -> Result<BTreeMap<String, ForeignLeaf>, NativeStop> {
        let mut modules = BTreeMap::new();
        for identity in functions {
            let function = &identity.definition;
            let (module, capture) = self.foreign_capture(function)?;
            let (_, header) = self.foreign_headers.get(function).ok_or_else(|| {
                NativeStop::Entry(format!("no checked foreign header for {function}"))
            })?;
            let leaf = modules
                .entry(module.definition_identity())
                .or_insert_with(|| ForeignLeaf {
                    module,
                    symbols: capture.symbols.clone(),
                    targets: capture
                        .archives
                        .iter()
                        .map(|(target, row)| ForeignTarget {
                            target: target.clone(),
                            archive: archive_ref(row),
                        })
                        .collect(),
                    functions: Vec::new(),
                });
            leaf.functions.push(ForeignFunction {
                identity: identity.clone(),
                header: header.clone(),
            });
        }
        Ok(modules)
    }

    pub(crate) fn bind_foreign_addresses(
        &self,
        instances: &mut crate::instances::InstanceSet,
    ) -> io::Result<()> {
        let modules = self
            .foreign_modules(&instances.foreign)
            .map_err(io::Error::other)?;
        for leaf in modules.values() {
            let mut rows = Vec::new();
            leaf.encode_archive_rows(&mut rows);
            let archive_set = ObjectKind::ForeignArchive.hash(&rows);
            instances
                .foreign_addresses
                .extend(leaf.functions.iter().map(|function| {
                    (
                        function.identity.clone(),
                        foreign_address(function, &leaf.symbols, archive_set),
                    )
                }));
        }
        Ok(())
    }

    fn check_foreign_archives<'a>(
        &'a self,
        modules: &BTreeMap<String, ForeignLeaf>,
        target: &str,
        link: Option<(&Path, &crate::tool_inputs::LinkRecipe)>,
    ) -> Result<
        (
            BTreeMap<ContentHash, &'a ForeignArchiveCapture>,
            LinkArchives,
        ),
        NativeStop,
    > {
        let mut selected = BTreeMap::new();
        let mut sealed = LinkArchives::default();
        for leaf in modules.values() {
            let function = &leaf.functions[0].identity.definition;
            let (_, capture) = self.foreign_capture(function)?;
            let row = select_archive(&leaf.module, capture, target)?;
            let object = archive_ref(row);
            sealed.admit(object, leaf, target, Some(&row.path), || {
                Ok(row.bytes.to_vec())
            })?;
            selected.insert(object.hash, row);
        }
        let symbols = sealed.definitions(&mut DecodeBudget::unlimited())?;
        if let Some((runtime, link)) = link {
            symbols.check_tool_inputs(runtime, link)?;
        }
        Ok((selected, sealed))
    }

    pub(super) fn preflight_foreign_link(
        &self,
        reached: &Reachable<'_>,
        options: &NativeOptions,
    ) -> Result<LinkArchives, NativeStop> {
        if options.recording || reached.materialized.foreign.is_empty() {
            return Ok(LinkArchives::default());
        }
        let inputs = reached
            .native
            .as_ref()
            .ok_or_else(|| NativeStop::Entry("a foreign build needs a link recipe".into()))?;
        reached.require()?;
        let (_, sealed) = self.check_foreign_archives(
            &self.reached_foreign_modules(reached)?,
            &options.target,
            Some((&options.runtime, &inputs.recipe)),
        )?;
        Ok(sealed)
    }

    fn foreign_capture(
        &self,
        function: &DefPath,
    ) -> Result<(DefPath, &ForeignModuleCapture), NativeStop> {
        let path = &function.0;
        let capture = self
            .foreign_inputs
            .get(&path.package)
            .and_then(|modules| modules.get(&path.modules));
        let (Some(capture), Some((name, parent))) = (capture, path.modules.split_last()) else {
            return Err(NativeStop::Entry(format!(
                "no captured foreign table for {function}"
            )));
        };
        let module = DefPath(hirsel_core_ids::ItemPath {
            package: path.package.clone(),
            modules: parent.to_vec(),
            name: name.clone(),
        });
        Ok((module, capture))
    }
}

fn select_archive<'a>(
    module: &DefPath,
    capture: &'a ForeignModuleCapture,
    target: &str,
) -> Result<&'a ForeignArchiveCapture, NativeStop> {
    capture.archives.get(target).ok_or_else(|| {
        let targets = if capture.archives.is_empty() {
            "none".into()
        } else {
            capture
                .archives
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        AdmissionError::Refused(format!(
            "FFI-3: {} has no archive for target {target}; its archives are for {targets}",
            module.definition_identity()
        ))
        .into()
    })
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
                TlsTrust::Any => "Any".to_owned(),
            };
            format!("Tls {} {first}-{last} {trust}", rule.pattern_text())
        }
        GrantRule::Resolve(rule) => format!("Resolve {}", rule.pattern_text()),
    }
}

fn bindings(
    decl: &EntryDecl,
    options: &NativeOptions,
    mut archive: impl FnMut(&str) -> Result<ObjectRef, NativeStop>,
) -> Result<Vec<GrantBinding>, NativeStop> {
    let mut next = [0usize; GRANT_NAMES.len()];
    let mut bound = Vec::new();
    let mut named = BTreeSet::new();
    for (ordinal, param) in decl.parameters.iter().enumerate() {
        if let Some(interface) = param
            .ty
            .strip_prefix("core@1/core/Foreign[")
            .and_then(|s| s.strip_suffix(']'))
        {
            if !options.foreign_grants.contains(interface) {
                return Err(NativeStop::Entry(format!(
                    "not granted: Foreign[{interface}]"
                )));
            }
            named.insert(interface);
            bound.push(GrantBinding {
                ordinal: ordinal as u32,
                kind: GrantKind::Foreign,
                values: vec![interface.to_owned()],
                archive: Some(archive(interface)?),
            });
            continue;
        }
        let tag = param
            .ty
            .strip_prefix("core@1/core/")
            .and_then(kind)
            .ok_or_else(|| {
                NativeStop::Entry(format!("unsupported provider parameter {}", param.ty))
            })?;
        let kind = GrantKind::from_tag(tag).expect("runtime grant kind");
        if kind == GrantKind::Foreign {
            return Err(NativeStop::Entry(
                "Foreign entry grant needs an interface".into(),
            ));
        }
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
            archive: None,
        });
    }
    if options.directories.len() > next[GrantKind::Dir as usize]
        || options.foreign_grants.len() != named.len()
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
            GrantKind::Dir | GrantKind::Net | GrantKind::Env | GrantKind::Foreign => grants.extend(
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

pub(crate) fn foreign_grant_refusal(
    recipe: &Recipe,
    approved: &BTreeSet<String>,
) -> Option<String> {
    let interface = recipe
        .launch
        .grants
        .iter()
        .filter(|grant| grant.kind == GrantKind::Foreign)
        .filter_map(|grant| grant.values.first())
        .find(|interface| !approved.contains(*interface))?;
    Some(format!(
        "the recipe grants Foreign={interface}; pass --grant Foreign={interface}"
    ))
}

pub(crate) fn foreign_grant_lines(
    recipe: &Recipe,
    root: ContentHash,
) -> impl Iterator<Item = String> + '_ {
    recipe.launch.grants.iter().filter_map(move |grant| {
        let archive = grant.archive?;
        let interface = grant.values.first()?;
        Some(format!(
            "foreign: {interface}: target {}, archive sha256 {}, recipe {}",
            recipe.launch.target,
            selection(archive.hash),
            selection(root),
        ))
    })
}

pub(crate) fn foreign_recording_refusal(recipe: &Recipe) -> Option<String> {
    let functions = recipe
        .foreign_modules
        .iter()
        .flat_map(|leaf| &leaf.functions)
        .map(|function| &function.identity.definition);
    recording_refusal(&recipe.entry.definition, functions)
}

pub(crate) fn recording_refusal<'a>(
    entry: &DefPath,
    functions: impl Iterator<Item = &'a DefPath>,
) -> Option<String> {
    let function = functions.map(DefPath::definition_identity).min()?;
    Some(format!(
        "FFI-8: a recording build cannot reach foreign code: {} reaches {}",
        entry.definition_identity(),
        function,
    ))
}

fn launch_options(
    recipe: &Recipe,
    names: &[String],
    options: &NativeOptions,
) -> Result<(NativeOptions, EntryDecl), NativeStop> {
    if options.recording
        && let Some(reason) = foreign_recording_refusal(recipe)
    {
        return Err(NativeStop::ForeignRecording {
            reason,
            site: recipe.entry.definition.definition_identity(),
        });
    }
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
    options.foreign_grants.clear();
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
        if kind == GrantKind::Foreign {
            let [interface] = values.as_slice() else {
                return Err(invalid());
            };
            options.foreign_grants.insert(interface.clone());
            params.push(EntryParameter {
                ty: format!("core@1/core/Foreign[{interface}]"),
                name: name.clone(),
            });
            continue;
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
    pub fn from_verified(mut release: VerifiedRelease) -> Self {
        let link_archives = std::mem::take(&mut release.link_archives);
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
            link_archives,
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
    bindings: &mut BTreeMap<DefPath, ItemSource>,
    unit: Option<&str>,
    site: Option<&'a Site>,
) -> Option<hirsel_core_ir::Anchor> {
    let site = site?;
    let index = *seen
        .entry((site.file.as_str(), site.line, site.column))
        .or_insert_with(|| {
            let index = ItemId(items.len() as u32);
            let mut item = site_item(site);
            if let Some(unit) = unit {
                let binding = bindings[&item];
                item.0.modules = vec!["unit".into(), unit.into()];
                item.0.name = index.0.to_string();
                bindings.insert(item.clone(), binding);
            }
            items.push(item);
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
    relocate: bool,
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
    for (identity, unit) in units {
        // Cached objects reference a unit's local site numbers. The final link
        // binds those symbols to the current recipe's absolute positions.
        let symbol = relocate.then(|| hirsel_core_ir::symbol(identity.digest()));
        let symbol = symbol.as_deref();
        let module = &mut unit.module;
        let mut items = HashMap::new();
        for (function, hints) in module.functions.iter_mut().zip(&unit.annex.0) {
            let (Some(body), Some(hints)) = (function.body.as_mut(), hints) else {
                continue;
            };
            for (at, site) in body.inst_anchors.iter_mut().zip(&hints.instructions) {
                *at = anchor(
                    &mut items,
                    &mut module.items,
                    &mut sources.items,
                    symbol,
                    site.as_ref(),
                );
            }
            for (block, site) in body.blocks.iter_mut().zip(&hints.terminators) {
                block.anchor = anchor(
                    &mut items,
                    &mut module.items,
                    &mut sources.items,
                    symbol,
                    site.as_ref(),
                );
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
        self.assemble_with(options, counters, None, None)
    }

    /// [`Self::assemble`] of a program checked as `product`. An optimised
    /// LLVM build takes every library instance a separately lowered release
    /// std holds from it ([`std_side`]): the program holds none of their
    /// bodies and calls each by its exported symbol. That std is read first
    /// from `prebuilt` (a library image's store), else from `product`'s image.
    pub(crate) fn assemble_with(
        self,
        options: &NativeOptions,
        counters: crate::reach::ReachCounters,
        product: Option<&CheckProduct>,
        prebuilt: Option<&Path>,
    ) -> Result<BuildProduct, NativeStop> {
        let std = self.std_side(options, product, prebuilt)?;
        self.assemble_over(options, counters, std)
    }

    /// The separately lowered release std an optimised LLVM build of this
    /// publication under `options` takes ([`std_side`]); None for any other.
    pub(crate) fn std_side(
        &self,
        options: &NativeOptions,
        product: Option<&CheckProduct>,
        prebuilt: Option<&Path>,
    ) -> Result<Option<StdSide>, NativeStop> {
        let (options, _) = self.launch(options)?;
        if !matches!(options.backend, Backend::Llvm) || !options.optimize {
            return Ok(None);
        }
        let runtime = match &options.observed {
            Some(observed) => observed.0.runtime,
            None => {
                crate::tool_inputs::toolchain(&options.link, &options.target, &options.runtime)?
                    .0
                    .runtime
            }
        };
        std_side(
            &self.units,
            &self.recipe.entry,
            product,
            prebuilt,
            &options,
            runtime,
        )
        .map(Some)
    }

    /// The options the recipe's launch fixes over `options`, and its entry's
    /// declaration.
    fn launch(&self, options: &NativeOptions) -> Result<(NativeOptions, EntryDecl), NativeStop> {
        let names = self.units[&self.recipe.entry]
            .annex
            .1
            .as_deref()
            .ok_or_else(|| {
                NativeStop::Entry("recipe entry has no startup parameter names".into())
            })?;
        launch_options(&self.recipe, names, options)
    }

    /// Assemble the program; an optimised LLVM build over `std`, the
    /// separately lowered release std ([`Self::std_side`]), which every such
    /// build takes and no other does: the program holds none of its exports'
    /// bodies and calls each by its exported symbol.
    pub(crate) fn assemble_over(
        mut self,
        options: &NativeOptions,
        counters: crate::reach::ReachCounters,
        std: Option<StdSide>,
    ) -> Result<BuildProduct, NativeStop> {
        let entry_identity = self.recipe.entry.clone();
        let (options, decl) = self.launch(options)?;
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
        let optimised = matches!(options.backend, Backend::Llvm) && options.optimize;
        if optimised != std.is_some() {
            return Err(NativeStop::Entry(
                "an optimised LLVM build, and only one, assembles over a release std".into(),
            ));
        }
        if let Some(std) = &std {
            self.units
                .retain(|identity, _| identity == &entry_identity || !std.roots.contains(identity));
        }
        let entry = &self.recipe.entry;
        let no_points = BTreeMap::new();
        let sources = bind_annexes(
            &mut self.units,
            std.as_ref().map_or(&no_points, |std| &std.points),
            true,
        )?;
        let root = &self.units[entry];
        let (abi, cleanup) = bound_entry_abi(&decl, &entry.definition, &root.module, FuncId(0))
            .map_err(NativeStop::Entry)?;
        // Native bodies and lent std code can operate on the same live Map.
        let emission = environment(&options, tools.runtime)?;
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
        let (mut objects, closed_image, launch_digest_unused) = if let Some(std) = std {
            let emitted = release_objects(
                &self.units,
                entry,
                std,
                &options,
                tools.runtime,
                &store,
                link.uses_hirsel_launcher(),
            )?;
            release_sites = emitted.release_sites;
            (
                emitted.objects,
                emitted.closed_image,
                emitted.launch_digest_unused,
            )
        } else {
            let objects = self
                .units
                .iter()
                .map(|(identity, unit)| {
                    let object_key = key(
                        "hirsel.recipe-native-object.v2",
                        &(emitter, identity, &unit.module, &unit.builtins),
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
        .with_release_sites(&release_sites)
        .with_foreign(&self.recipe.foreign_modules);
        let result = link_objects(
            &plan,
            &objects,
            &self.link_archives,
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

/// The release objects of `units`, which hold `entry`'s closure apart from
/// the exports of `std`, over `std`: the program's image parts and the std's
/// objects. `closed_executable`: the hirsel launcher alone enters the image.
pub(super) fn release_objects(
    units: &BTreeMap<Identity, Unit>,
    entry: &Identity,
    std: StdSide,
    options: &NativeOptions,
    runtime: ContentHash,
    store: &Store,
    closed_executable: bool,
) -> Result<Emitted, NativeStop> {
    let reached = units.keys().cloned().collect::<Vec<_>>();
    let mut modules = units
        .values()
        .map(|unit| Cow::Borrowed(&unit.module))
        .collect();
    let body_keys = crate::callable_variants::prepare_inputs(
        &mut modules,
        &reached,
        crate::callable_variants::Roots::Entry(&entry.definition),
        options.instance_limit,
        |identity| {
            let owner = units[identity].owner()?;
            Ok((
                FuncId(0),
                owner,
                !identity.closures.is_empty() || !identity.dictionaries.is_empty(),
            ))
        },
    )?;
    let joined = release::join_modules(entry, &reached, &modules, |_| FuncId(0))?;
    let mut emitted = release::emit_release_image(
        entry,
        &reached,
        &body_keys,
        joined,
        options,
        runtime,
        store,
        closed_executable,
        (&std.summaries, std.key, std.lent.as_slice()),
    )?;
    emitted.objects.extend(std.objects);
    Ok(emitted)
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
