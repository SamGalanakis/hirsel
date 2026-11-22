//! Native products compose the checked body and the two engines directly.
use super::*;
use crate::store::Bare;
use hirsel_core_ir::Identity;
use hirsel_core_layout::{Layout, Recipe};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::process::Command;

#[path = "entry_cleanup.rs"]
mod cleanup;
#[path = "recipe_native.rs"]
pub(crate) mod recipe_build;
#[path = "release_native.rs"]
mod release;
#[path = "std_release.rs"]
pub(crate) mod std_release;
use cleanup::result_cleanup;

pub const TARGET: &str = "x86_64-unknown-linux-gnu";
use crate::tool_inputs::SUPPORT_LIBS as DIRECT_LINK_SUPPORT_LIBS;
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum Backend {
    Cranelift,
    Llvm,
}
impl Backend {
    pub fn supports_target(self, triple: &str) -> bool {
        match self {
            Self::Cranelift => hirsel_cranelift::core::supports_target(triple),
            Self::Llvm => hirsel_llvm_lazy::core::supports_target(triple),
        }
    }
}
impl std::str::FromStr for Backend {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cranelift" => Ok(Self::Cranelift),
            "llvm" => Ok(Self::Llvm),
            _ => Err("expected cranelift or llvm"),
        }
    }
}
/// Where a native link's link editor and its argv come from.
#[derive(Clone, Debug)]
pub enum Link {
    /// Capture the link recipe through this C driver (`--cc`, or the pinned
    /// LLVM's clang for a compiler that is not installed).
    Driver(PathBuf),
    /// The recipe an installation captured at install: its own `bin/ld.lld`
    /// and the system's CRT files. No C driver runs.
    Installed(PathBuf),
}
impl Link {
    /// Tests drive the C driver they link through directly.
    #[cfg(test)]
    pub(crate) fn driver(&self) -> &Path {
        match self {
            Link::Driver(driver) => driver,
            Link::Installed(_) => panic!("tests link through a C driver"),
        }
    }
}
/// Replay configuration for the launcher-only `hirsel replay` / `test --replay` path.
#[derive(Clone, Debug, Serialize)]
pub struct ReplayPlan {
    pub path: String,
    pub diagnose: bool,
}

/// Runtime and linker inputs are explicit, independently produced artifacts.
#[derive(Clone)]
pub struct NativeOptions {
    /// Link and enter the recording runtime (`hirsel build --recording`, `run --record`, `replay`, `test --replay`).
    pub recording: bool,
    /// Launcher-only replay plan (`hirsel replay`, `test --replay` replay phase).
    pub replay: Option<ReplayPlan>,
    pub backend: Backend,
    pub optimize: bool,
    /// Source-neutral native line rows; failure positions are always enabled.
    pub debug_lines: bool,
    pub target: String,
    pub runtime: PathBuf,
    pub link: Link,
    pub store: PathBuf,
    /// The release parts every program shares: each optimised definition's
    /// object under its exact key (the user's cache by default).
    pub parts: PathBuf,
    pub output: PathBuf,
    pub root_limit: u64,
    pub instance_limit: usize,
    /// Paths are opened relative to the executable's working directory at startup.
    pub directories: Vec<String>,
    /// Explicit host authority, using the runtime's grant-kind bit positions.
    pub grant_mask: u64,
    pub net_rules: Vec<hirsel_core_net::GrantRule>,
    pub secret_grants: Vec<hirsel_core_net::SecretGrant>,
    pub foreign_grants: BTreeSet<String>,
    pub(crate) exec_grants: Vec<crate::exec_launch::Binding>,
    pub env_patterns: Option<hirsel_core_net::EnvPatterns>,
    /// Diagnostic build: exact root reservation peak, unsuitable for timing samples.
    pub logical_charge_report: Option<PathBuf>,
    /// The toolchain this request already observed for its request record's
    /// key: the build links with it rather than observing again, so the
    /// executable is the one the key names.
    pub(crate) observed: Option<
        Arc<(
            crate::tool_inputs::Toolchain,
            crate::tool_inputs::LinkRecipe,
        )>,
    >,
}
pub(crate) const DEFAULT_INSTANCE_LIMIT: usize = 65_536;

impl NativeOptions {
    pub(crate) fn bind_exec(&mut self) -> Result<(), NativeStop> {
        self.exec_grants
            .iter_mut()
            .try_for_each(crate::exec_launch::Binding::seal_policy)
    }

    /// A request's options before its flags: the development engine
    /// (Cranelift), unoptimised, without line tables or charge report. An
    /// installation prebuilds for exactly these.
    pub fn development(
        target: String,
        runtime: PathBuf,
        link: Link,
        store: PathBuf,
        output: PathBuf,
    ) -> Self {
        Self {
            backend: Backend::Cranelift,
            optimize: false,
            debug_lines: false,
            target,
            runtime,
            link,
            store,
            parts: crate::PackageProduct::default_cache().join("release-parts"),
            output,
            root_limit: 64 * 1024 * 1024,
            instance_limit: DEFAULT_INSTANCE_LIMIT,
            recording: false,
            replay: None,
            directories: vec![],
            grant_mask: 0,
            net_rules: vec![],
            secret_grants: vec![],
            foreign_grants: BTreeSet::new(),
            exec_grants: vec![],
            env_patterns: None,
            logical_charge_report: None,
            observed: None,
        }
    }
}
#[derive(Debug)]
pub enum NativeStop {
    Check(String),
    Instances(InstanceStop),
    Entry(String),
    ForeignRecording {
        reason: String,
        site: String,
    },
    Archive(crate::code_admission::AdmissionError),
    Emit {
        definition: DefPath,
        message: String,
    },
    Link(String),
    Input(io::Error),
    /// The build succeeded and its executable is stored; writing it to the
    /// requested output failed.
    Output {
        product: Box<BuildProduct>,
        error: io::Error,
    },
}
impl fmt::Display for NativeStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Check(s) => write!(f, "check: {s}"),
            Self::Instances(s) => write!(f, "instances: {s}"),
            Self::Entry(s) => write!(f, "entry: {s}"),
            Self::ForeignRecording { reason, .. } => write!(f, "refused: {reason}"),
            Self::Archive(error) => error.fmt(f),
            Self::Emit {
                definition,
                message,
            } => write!(f, "emit {definition}: {message}"),
            Self::Link(s) => write!(f, "link: {s}"),
            Self::Input(e) => write!(f, "input: {e}"),
            Self::Output { error, .. } => f.write_str(&output_stop(error)),
        }
    }
}
impl std::error::Error for NativeStop {}
impl From<io::Error> for NativeStop {
    fn from(e: io::Error) -> Self {
        Self::Input(e)
    }
}
impl From<crate::code_admission::AdmissionError> for NativeStop {
    fn from(error: crate::code_admission::AdmissionError) -> Self {
        Self::Archive(error)
    }
}
pub struct ObjectProduct {
    pub private_variants: usize,
    pub instance: Identity,
    pub identity: ContentHash,
    pub path: PathBuf,
    /// The object's read check (`Bare::check`).
    pub(crate) check: u64,
    /// The descriptor this request checked or wrote the object's bytes
    /// through, which the link hands the linker. None: a stored object named
    /// by a record, which the link opens and checks through that descriptor.
    pub(crate) held: Option<std::fs::File>,
    pub debug: Option<hirsel_core_ir::source::NativeDebug>,
    /// Stored debug rows: their store and address. A link with debug lines
    /// reads them when `debug` is not in hand.
    pub(crate) debug_record: Option<(Store, ContentHash)>,
    pub compiled: bool,
}
impl ObjectProduct {
    /// A stored object named by a record this request did not write.
    fn stored(instance: Identity, store: &Store, object: Bare, debug: Option<ContentHash>) -> Self {
        Self {
            private_variants: 0,
            instance,
            identity: object.address,
            path: crate::native_object::path(store, object.address),
            check: object.check,
            held: None,
            debug: None,
            debug_record: debug.map(|d| (store.clone(), d)),
            compiled: false,
        }
    }
    /// An object this request read and checked, or emitted and wrote,
    /// through `held`.
    fn held(
        instance: Identity,
        store: &Store,
        (object, held): (Bare, std::fs::File),
        native: hirsel_core_ir::source::NativeObject,
        debug: Option<ContentHash>,
        compiled: bool,
    ) -> Self {
        Self {
            private_variants: 0,
            instance,
            identity: object.address,
            path: crate::native_object::path(store, object.address),
            check: object.check,
            held: Some(held),
            debug: native.debug,
            debug_record: debug.map(|d| (store.clone(), d)),
            compiled,
        }
    }
    fn bare(&self) -> Bare {
        Bare {
            address: self.identity,
            check: self.check,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct BuildProduct {
    /// Native implementation copies, never semantic type instances.
    pub private_variants: usize,
    pub executable: PathBuf,
    /// The executable's key: its objects' identities, the entry plan, the
    /// sources digest and the link inputs. Its record is stored under it.
    pub identity: ContentHash,
    /// The entry's PROG-10 code address.
    pub address: ContentHash,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipe: Option<ContentHash>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assembly: Option<recipe_build::AssemblyRecord>,
    /// The entry's PROG-9 revision.
    pub revision: ContentHash,
    pub compiled: usize,
    pub linked: bool,
    /// True means diagnostic instrumentation; timings are not performance samples.
    pub observation_mode: bool,
    pub debug_lines: bool,
    /// The walk's and the link's work counters.
    pub reach: crate::reach::ReachCounters,
}
/// The libLLVM an LLVM object's key takes (`tool_inputs::llvm_library`):
/// none for Cranelift, which is part of the compiler executable.
fn engine_library(backend: Backend) -> io::Result<Option<ContentHash>> {
    matches!(backend, Backend::Llvm)
        .then(crate::tool_inputs::llvm_library)
        .transpose()
}
pub(crate) fn key(domain: &str, value: &impl Serialize) -> io::Result<ContentHash> {
    Ok(ContentHash::of_parts(domain, [&encode(value)?[..]]))
}
pub(super) fn selection(hash: ContentHash) -> String {
    hash.0.iter().map(|b| format!("{b:02x}")).collect()
}
/// A keyed record, an executable or an entry record, is stored framed under
/// its key: its bytes are a function of the key's inputs and are checked on
/// every read, so no digest of them names them.
fn cached(store: &Store, key: ContentHash) -> io::Result<Option<Vec<u8>>> {
    store.get(key)
}
fn publish(store: &Store, key: ContentHash, bytes: &[u8]) -> io::Result<()> {
    store.put_keyed(key, bytes)
}
// A unit is the producer's complete module, including type shapes and finalizers.
// The cache chooses its granularity; this boundary never strips type/drop inputs.
fn emit_unit(
    module: &Module,
    symbols: impl FnOnce() -> Vec<String>,
    options: &NativeOptions,
    object_key: ContentHash,
    instance: &Identity,
) -> Result<hirsel_core_ir::source::NativeObject, String> {
    // Slots whose needed ranges never meet share a home before any engine sees
    // the module (hirsel_core_ir::extent). Stored bodies and this object's key are
    // the unmerged IR; the compiler identity in every key names this step.
    let mut shared = module.clone();
    shared.external_call_sites = !options.foreign_grants.is_empty();
    let Module {
        functions,
        signatures,
        ..
    } = &mut shared;
    for body in functions.iter_mut().filter_map(|f| f.body.as_mut()) {
        hirsel_core_ir::extent::share_slots(body, signatures);
    }
    let module = &shared;
    match options.backend {
        Backend::Cranelift => hirsel_cranelift::core::emit_object_with_charge_observation(
            module,
            &symbols(),
            &hirsel_core_layout::compute(&module.types).expect("producer layouts"),
            &hirsel_cranelift::core::Options {
                target: &options.target,
                // Library bodies share the image's per-instance object cache.
                // Optimise them once while retaining cheap user-code emission.
                optimize: options.optimize
                    || instance.definition.package_is("core", "1")
                    || instance.definition.package_is("std", "1"),
                debug_lines: options.debug_lines,
            },
            object_key,
            options.logical_charge_report.is_some(),
        )
        .map_err(|e| e.to_string()),
        Backend::Llvm => hirsel_llvm_lazy::core::emit_object_relocatable_for_target(
            module,
            hirsel_core_layout::Target::from_triple(&options.target)
                .ok_or_else(|| format!("unsupported LLVM target {}", options.target))?,
            if options.optimize {
                hirsel_llvm_lazy::core::Optimization::Speed
            } else {
                hirsel_llvm_lazy::core::Optimization::None
            },
            options.logical_charge_report.is_some(),
            options.debug_lines,
        )
        .map_err(|e| e.to_string())
        .and_then(|bytes| {
            hirsel_llvm_lazy::core::finish_native_object(bytes, object_key, options.debug_lines)
        }),
    }
}

/// The environment every development object key starts with, encoded: the
/// object schema, the compiler, the runtime ABI, the engine library, the
/// target, the backend and the emission settings.
pub(crate) fn environment(options: &NativeOptions, runtime: ContentHash) -> io::Result<Vec<u8>> {
    encode(&(
        crate::native_object::SCHEMA,
        compiler_identity()?,
        runtime,
        engine_library(options.backend)?,
        &options.target,
        options.backend,
        options.optimize,
        options.debug_lines,
        options.logical_charge_report.is_some(),
        !options.foreign_grants.is_empty(),
        matches!(options.backend, Backend::Llvm) && !hirsel_llvm_lazy::core::facts_enabled(),
    ))
}
/// `domain` over the encoded `(environment, rest)`: a tuple encodes as its
/// fields in order.
fn environment_key(
    domain: &str,
    environment: &[u8],
    rest: &impl Serialize,
) -> io::Result<ContentHash> {
    let mut bytes = environment.to_vec();
    bytes.extend(encode(rest)?);
    Ok(ContentHash::of_parts(domain, [&bytes[..]]))
}
/// A build's native inputs: its toolchain and link recipe, and for a
/// Cranelift development build the instance records its walk reads.
pub(crate) struct NativeInputs {
    pub toolchain: crate::tool_inputs::Toolchain,
    pub recipe: crate::tool_inputs::LinkRecipe,
    pub records: Option<crate::instances::Records>,
}
pub(crate) fn native_inputs(
    product: &CheckProduct,
    options: &NativeOptions,
) -> io::Result<NativeInputs> {
    let (toolchain, recipe) = match &options.observed {
        Some(observed) => observed.as_ref().clone(),
        None => crate::tool_inputs::toolchain(&options.link, &options.target, &options.runtime)?,
    };
    // LLVM development objects keep the v4 key, and LLVM release reads full IR.
    let records = if matches!(options.backend, Backend::Cranelift) && !options.optimize {
        let env = environment(options, toolchain.runtime)?;
        Some(crate::instances::Records {
            digest: ContentHash::of_parts("hirsel.core-environment.v1", [&env[..]]),
            env,
            store: Store::new(&options.store)?,
            library: product
                .image
                .as_ref()
                .map(|i| Store::new(&i.store_root().join("native")))
                .transpose()?,
        })
    } else {
        None
    };
    Ok(NativeInputs {
        toolchain,
        recipe,
        records,
    })
}

/// The table entries a Cranelift development build's objects give the reach
/// record, with their closures.
#[derive(Default)]
pub(crate) struct Table {
    entries: Vec<crate::reach::Fresh>,
    closures: Vec<Arc<hirsel_core_ir::Closure>>,
    by_hash: std::collections::BTreeMap<ContentHash, u32>,
}
impl Table {
    #[allow(clippy::too_many_arguments)]
    fn push<'c>(
        &mut self,
        instances: &[Identity],
        foreign: &BTreeSet<Identity>,
        identity: &Identity,
        own: Option<ContentHash>,
        body: ContentHash,
        location: crate::instances::Location,
        object: Bare,
        debug: Option<ContentHash>,
        children: impl IntoIterator<Item = (Option<u32>, &'c Identity)>,
        slots: &[Identity],
        signatures: &crate::instances::CodeSignatures,
        adapters: &[crate::instances::Adapter],
        described: impl IntoIterator<Item = (Option<u32>, &'c Identity)>,
        items: Vec<DefPath>,
        closures: impl IntoIterator<Item = &'c Arc<hirsel_core_ir::Closure>>,
    ) {
        // A child's position in the prior record is its position here when
        // the instance there is the same.
        let index = |(hint, c): (Option<u32>, &Identity)| match hint {
            Some(k) if instances.get(k as usize) == Some(c) => k,
            _ => instances
                .binary_search(c)
                .expect("the instances an instance calls or describes are reached")
                as u32,
        };
        let children = children
            .into_iter()
            .filter(|(_, child)| !foreign.contains(child))
            .map(index)
            .collect();
        let described = described.into_iter().map(index).collect();
        let closures = closures
            .into_iter()
            .map(|c| {
                *self.by_hash.entry(c.hash).or_insert_with(|| {
                    self.closures.push(Arc::clone(c));
                    self.closures.len() as u32 - 1
                })
            })
            .collect();
        self.entries.push(crate::reach::Fresh {
            identity: identity.clone(),
            own,
            body,
            location,
            object,
            debug,
            children,
            slots: slots.to_vec(),
            signatures: signatures.clone(),
            adapters: adapters.to_vec(),
            described,
            items,
            closures,
        });
    }
}
/// A link's outcome: the build, or the instance whose stored object or debug
/// rows failed their check and were removed.
enum Linked {
    Built(BuildProduct),
    /// The stored objects (or debug rows) of these instances failed their
    /// check: the instances, how many objects this pass compiled, and its
    /// work counters.
    Damaged(Vec<Identity>, usize, crate::reach::ReachCounters),
}

/// A release image's objects and what its link needs to know of them.
struct Emitted {
    objects: Vec<ObjectProduct>,
    /// The image was specialized for the hirsel launcher alone entering it
    /// (`step_contexts::specialize`): the entry runs with no step bound current.
    closed_image: bool,
    launch_digest_unused: bool,
    release_sites: Vec<hirsel_core_ir::source::ReleaseSite>,
}

impl CheckProduct {
    /// The objects of `entry`'s reached closure: each checked body as an
    /// independent development object, or, for an optimised LLVM build, the
    /// release image parts of the closure over the release std, and the std's
    /// objects.
    pub fn emit_reachable(
        &self,
        entry: &DefPath,
        options: &NativeOptions,
    ) -> Result<Vec<ObjectProduct>, NativeStop> {
        let reached = self.reachable_for(entry, options)?;
        self.preflight_foreign_link(&reached, options)?;
        if !(matches!(options.backend, Backend::Llvm) && options.optimize) {
            return Ok(self.emit_reached(entry, &reached, options)?.0);
        }
        reached.require_native(self, entry, options)?;
        let inputs = reached.native.as_ref().expect("a build's reachable set");
        let root = Identity::plain(entry.clone());
        let code = crate::code_store::CodeStore::new(options.store.join("code"));
        let (mut units, _, _) = self.release_units(&root, &reached, &code, None)?;
        let runtime = inputs.toolchain.runtime;
        let std = std_release::std_side(&units, &root, Some(self), None, options, runtime)?;
        units.retain(|identity, _| identity == &root || !std.roots.contains(identity));
        recipe_build::bind_annexes(&mut units, &std.points, true)?;
        let store = Store::new(&options.store)?;
        Ok(
            recipe_build::release_objects(&units, &root, std, options, runtime, &store, false)?
                .objects,
        )
    }
    /// The reached instances' objects in link order, and for a Cranelift
    /// development build whose walk drafted a reach record, their table.
    fn emit_reached(
        &self,
        entry: &DefPath,
        reached: &Reachable<'_>,
        options: &NativeOptions,
    ) -> Result<(Vec<ObjectProduct>, Option<Table>), NativeStop> {
        let inputs = reached.native.as_ref().expect("a build's reachable set");
        let instances = reached.require_native(self, entry, options)?;
        if matches!(options.backend, Backend::Llvm) && options.optimize {
            return Err(NativeStop::Entry(
                "an optimised LLVM build is assembled from its recipe over the release std".into(),
            ));
        }
        self.emit_instances(
            instances,
            &reached.materialized,
            inputs,
            reached.reach.as_ref(),
            options,
        )
    }
    /// Emit `instances` for `options`, each a hit on its stored object or
    /// compiled and published under its key, with the table `reach`'s draft
    /// gathers; an instance `reach` expanded is read from its prior record.
    pub(crate) fn emit_instances(
        &self,
        instances: &[Identity],
        materialized: &crate::instances::InstanceSet,
        inputs: &NativeInputs,
        reach: Option<&crate::reach::Use>,
        options: &NativeOptions,
    ) -> Result<(Vec<ObjectProduct>, Option<Table>), NativeStop> {
        if !options.backend.supports_target(&options.target) {
            return Err(NativeStop::Entry(format!(
                "{:?} engine does not support target {}",
                options.backend, options.target
            )));
        }
        let store = Store::new(&options.store)?;
        if matches!(options.backend, Backend::Llvm) && options.optimize {
            return Err(NativeStop::Entry(
                "an optimised LLVM build is assembled from its recipe over the release std".into(),
            ));
        }
        let library_store = self
            .image
            .as_ref()
            .map(|i| Store::new(&i.store_root().join("native")))
            .transpose()?;
        let mut objects = Vec::new();
        let mut names = Vec::new();
        if let Some(records) = &inputs.records {
            let prior = reach.and_then(|r| r.prior.as_ref());
            let mut table = reach
                .and_then(|r| r.draft.as_ref())
                .map(|_| Table::default());
            // A hit's instances are its record's, in order.
            let hit = reach.is_some_and(|r| r.hit);
            let mut expansions = materialized.expansions(instances);
            for (k, instance) in instances.iter().enumerate() {
                let expanded = expansions.next().flatten().or(hit.then_some(k as u32));
                if let Some(i) = expanded {
                    let prior = prior.expect("an expanded instance has its record");
                    let e = prior.entry(i as usize);
                    objects.push(ObjectProduct::stored(
                        instance.clone(),
                        records.store(e.location),
                        e.object,
                        e.debug,
                    ));
                    if let Some(table) = &mut table {
                        table.push(
                            instances,
                            &materialized.foreign,
                            instance,
                            e.own,
                            e.body,
                            e.location,
                            e.object,
                            e.debug,
                            e.children
                                .iter()
                                .map(|&c| (Some(c), &prior.entry(c as usize).identity)),
                            &e.slots,
                            &e.signatures,
                            &e.adapters,
                            e.described
                                .iter()
                                .map(|&c| (Some(c), &prior.entry(c as usize).identity)),
                            prior.items(e).cloned().collect(),
                            prior.closures_of(e),
                        );
                    }
                    continue;
                }
                let (object_key, location) = materialized.keys[instance];
                let store = records.store(location);
                if let Some(record) = materialized.hits.get(instance) {
                    objects.push(ObjectProduct::stored(
                        instance.clone(),
                        store,
                        record.object,
                        record.debug,
                    ));
                    if let Some(table) = &mut table {
                        table.push(
                            instances,
                            &materialized.foreign,
                            instance,
                            record.own,
                            record.body,
                            location,
                            record.object,
                            record.debug,
                            record.children.iter().map(|c| (None, c)),
                            &record.slots,
                            &record.signatures,
                            &record.adapters,
                            record.described.iter().map(|c| (None, c)),
                            record.items.clone(),
                            &record.closures,
                        );
                    }
                    continue;
                }
                let module = materialized.get(instance).ir()?;
                let symbols = materialized.link_symbols(instance, &mut names)?;
                let object = emit_unit(module, || symbols, options, object_key, instance).map_err(
                    |message| NativeStop::Emit {
                        definition: instance.definition.clone(),
                        message,
                    },
                )?;
                let fields = &materialized.fields[instance];
                let record = crate::native_object::Instance {
                    schema: crate::native_object::INSTANCE_SCHEMA,
                    identity: instance.clone(),
                    own: fields.own,
                    body: fields.body,
                    env: records.digest,
                    object: Default::default(),
                    debug: None,
                    children: fields.children.clone(),
                    slots: fields.slots.clone(),
                    signatures: fields.signatures.clone(),
                    adapters: fields.adapters.clone(),
                    described: fields.described.clone(),
                    items: module.items.clone(),
                    closures: fields.closures.clone(),
                };
                let (bare, debug, held) =
                    crate::native_object::publish_instance(store, object_key, &object, record)?;
                if let Some(table) = &mut table {
                    table.push(
                        instances,
                        &materialized.foreign,
                        instance,
                        fields.own,
                        fields.body,
                        location,
                        bare,
                        debug,
                        fields.children.iter().map(|c| (None, c)),
                        &fields.slots,
                        &fields.signatures,
                        &fields.adapters,
                        fields.described.iter().map(|c| (None, c)),
                        module.items.clone(),
                        &fields.closures,
                    );
                }
                objects.push(ObjectProduct::held(
                    instance.clone(),
                    store,
                    (bare, held),
                    object,
                    debug,
                    true,
                ));
            }
            return Ok((objects, table));
        }
        // LLVM development: the v4 key, the environment, the instance, its
        // body key and its module's hash.
        let env = environment(options, inputs.toolchain.runtime)?;
        let mut scratch = Vec::new();
        for instance in instances {
            // An image's definitions keep their objects in its store; a glue
            // instance has no definition and is the request's.
            let store = if !self.definitions.contains_key(&instance.definition)
                && !hirsel_core_front::is_glue(&instance.definition)
            {
                library_store.as_ref().unwrap_or(&store)
            } else {
                &store
            };
            let instance_body = materialized.get(instance);
            let module = instance_body.ir()?;
            let object_key = environment_key(
                "hirsel.core-object.v4",
                &env,
                &(
                    &instance,
                    instance_body.product.as_ref().map(|p| &p.key),
                    instance_body.ir_hash(&mut scratch)?,
                ),
            )?;
            let (held, object, compiled) = if let Some((bare, object, file)) =
                crate::native_object::load(store, object_key)?
            {
                ((bare, file), object, false)
            } else {
                let symbols = materialized.link_symbols(instance, &mut names)?;
                let object = emit_unit(module, || symbols, options, object_key, instance).map_err(
                    |message| NativeStop::Emit {
                        definition: instance.definition.clone(),
                        message,
                    },
                )?;
                (
                    crate::native_object::publish(store, object_key, &object)?,
                    object,
                    true,
                )
            };
            objects.push(ObjectProduct::held(
                instance.clone(),
                store,
                held,
                object,
                None,
                compiled,
            ));
        }
        Ok((objects, None))
    }
    /// Build through the real process runtime. Unsupported checker/entry products
    /// stop explicitly; no alternate scalar-main or interpreter path exists.
    pub fn build(
        &self,
        entry: &DefPath,
        options: &NativeOptions,
    ) -> Result<BuildProduct, NativeStop> {
        let reached = self.reachable_for(entry, options)?;
        self.build_reachable(entry, &reached, options)
    }
    /// Build `reached`, which `reachable_for` collected for `options`. A stored
    /// object whose bytes fail their check at the link is removed, and the
    /// build walks again with the instances refused by that link rebuilt; an
    /// instance rebuilt by an earlier pass is reused like any other, and the
    /// link checks it again. The product's work counters are every pass's.
    pub fn build_reachable(
        &self,
        entry: &DefPath,
        reached: &Reachable<'_>,
        options: &NativeOptions,
    ) -> Result<BuildProduct, NativeStop> {
        let mut bound;
        let options = if options
            .exec_grants
            .iter()
            .any(|grant| grant.policy.is_none())
        {
            bound = options.clone();
            bound.bind_exec()?;
            &bound
        } else {
            options
        };
        // An optimised LLVM build is assembled from its published recipe over
        // the separately lowered release std, like `build`.
        if matches!(options.backend, Backend::Llvm) && options.optimize {
            self.preflight_foreign_link(reached, options)?;
            reached.require_native(self, entry, options)?;
            let code = crate::code_store::CodeStore::new(options.store.join("code"));
            return self
                .publish_release(
                    entry,
                    reached,
                    options,
                    &code,
                    hirsel_core_ids::launch::DEFAULT_N_PLACE,
                )?
                .assemble_with(options, reached.counters, Some(self), None);
        }
        let mut refused = BTreeSet::new();
        let mut rebuild = BTreeSet::new();
        let mut compiled = 0;
        let mut work = crate::reach::ReachCounters::default();
        let mut linked = self.build_once(entry, reached, options, &rebuild)?;
        loop {
            match linked {
                Linked::Built(mut product) => {
                    product.compiled += compiled;
                    let reach = &mut product.reach;
                    reach.expanded += work.expanded;
                    reach.instance_path += work.instance_path;
                    reach.objects_verified += work.objects_verified;
                    reach.objects_refused = refused.len() as u64;
                    return Ok(product);
                }
                Linked::Damaged(instances, before, counters) => {
                    compiled += before;
                    work.expanded += counters.expanded;
                    work.instance_path += counters.instance_path;
                    work.objects_verified += counters.objects_verified;
                    for instance in &instances {
                        if !refused.insert(instance.clone()) {
                            return Err(NativeStop::Link(format!(
                                "rebuilt object of {} failed its check",
                                instance.definition
                            )));
                        }
                    }
                    rebuild = instances.into_iter().collect();
                    let again = self.reachable_rebuilding(entry, options, &rebuild)?;
                    linked = self.build_once(entry, &again, options, &rebuild)?;
                }
            }
        }
    }
    fn build_once(
        &self,
        entry: &DefPath,
        reached: &Reachable<'_>,
        options: &NativeOptions,
        damaged: &BTreeSet<Identity>,
    ) -> Result<Linked, NativeStop> {
        let link_archives = self.preflight_foreign_link(reached, options)?;
        let instances = reached.require_native(self, entry, options)?;
        let foreign_modules = self.reached_foreign_modules(reached)?;
        let address = reached.address(entry)?;
        let inputs = reached
            .native
            .as_ref()
            .ok_or_else(|| NativeStop::Entry("a build needs `reachable_for` its options".into()))?;
        let reach = reached.reach.as_ref();
        let prior = reach.and_then(|r| r.prior.as_ref());
        // The items of instances expanded from the record, by its path
        // indices, and those of the rest.
        let mut recorded_items: Vec<u32> = Vec::new();
        let mut items: Vec<&DefPath> = Vec::new();
        let hit = reach.is_some_and(|r| r.hit);
        let mut expansions = reached.materialized.expansions(instances);
        for (k, instance) in instances.iter().enumerate() {
            let expanded = expansions.next().flatten().or(hit.then_some(k as u32));
            match expanded {
                Some(i) => recorded_items.extend(&prior.unwrap().entry(i as usize).items),
                None => items.extend(reached.materialized.items(instance)?),
            }
        }
        // A hit's items are its record's: with the same request files they
        // have the record's sources, and the rows are built only for a link.
        let files = reach
            .map(|_| crate::reach::files_digest(self))
            .transpose()?;
        let same_files = prior.is_some_and(|p| Some(p.head.files) == files);
        let recorded = prior.filter(|_| hit && same_files).map(|p| p.head.sources);
        let rows = || self.link_rows(prior, &recorded_items, &items, same_files);
        let source_rows = match recorded {
            Some(_) => None,
            None => Some(rows()?),
        };
        let sources = match (&source_rows, recorded) {
            (Some(rows), _) => crate::reach::sources_digest(self, rows)?,
            (None, Some(sources)) => sources,
            (None, None) => unreachable!("rows are built without a recorded digest"),
        };
        let (mut objects, table) = self.emit_reached(entry, reached, options)?;
        let store = Store::new(&options.store)?;
        let (link, cleanup) = match (reach, prior) {
            (Some(reach), Some(prior))
                if reach.entry_live
                    && prior
                        .link()
                        .cleanup
                        .as_ref()
                        .is_none_or(|c| !damaged.contains(&c.identity)) =>
            {
                let cleanup =
                    prior.link().cleanup.as_ref().map(|c| {
                        ObjectProduct::stored(c.identity.clone(), &store, c.object, c.debug)
                    });
                (prior.link().clone(), cleanup)
            }
            _ => {
                let (abi, cleanup) = self.bind_entry_native(entry, options, inputs, damaged)?;
                let link = crate::reach::EntryLink {
                    abi,
                    cleanup: cleanup.as_ref().map(|o| crate::reach::Cleanup {
                        identity: o.instance.clone(),
                        object: o.bare(),
                        debug: o.debug_record.as_ref().map(|(_, d)| *d),
                    }),
                };
                (link, cleanup)
            }
        };
        objects.extend(cleanup);
        let counters = reached.counters;
        // The record is published once the executable is stored: a walk's
        // draft with its table, or a hit's sections under its current head.
        let record = |executable: ContentHash| -> io::Result<u64> {
            let Some(reach) = reach else {
                return Ok(0);
            };
            let content = match (&reach.draft, &table) {
                (Some(draft), Some(table)) => crate::reach::Content::Drafted {
                    constants: &reach.constants,
                    draft,
                    instances: &table.entries,
                    closures: &table.closures,
                    entry: &link,
                    places: source_rows.as_deref().unwrap_or_default(),
                    foreign: &reached.materialized.foreign,
                },
                _ if reach.hit => crate::reach::Content::Held {
                    places: source_rows.as_deref(),
                },
                _ => return Ok(0),
            };
            let records = inputs
                .records
                .as_ref()
                .expect("a reach record has its records");
            let head = crate::reach::Head {
                sources,
                files: files.expect("a record is drafted with its reach"),
                verified: self.root,
                executable,
                environment: records.digest,
                toolchain: inputs.toolchain.image,
                recipe: inputs.recipe.identity,
                tools: inputs.toolchain.environment,
                policy: key(
                    "hirsel.core-entry-policy.v1",
                    &crate::native_entry::policy(options),
                )?,
                address,
                instance_limit: options.instance_limit as u64,
            };
            Ok(crate::reach::publish(&store, reach.selection, prior, &head, content)? as u64)
        };
        let entry_plan = crate::native_entry::Plan::new(
            Identity::plain(entry.clone()).symbol(),
            entry.to_string(),
            &link.abi,
            link.cleanup.as_ref().map(|c| c.identity.symbol()),
            false,
            options,
        )?
        .with_foreign(foreign_modules.values());
        let image = self
            .image
            .as_ref()
            .map(|i| Store::new(&i.store_root().join("native")))
            .transpose()?;
        link_objects(
            &entry_plan,
            &objects,
            &link_archives,
            image.as_ref(),
            options,
            sources,
            || match &source_rows {
                Some(rows) => self.source_map(rows),
                None => self.source_map(&rows()?),
            },
            inputs.toolchain.image,
            inputs.recipe.clone(),
            false,
            counters,
            record,
            address,
            || reached.revisions(),
            None,
        )
    }
    /// The entry's declaration part, bound by check.
    fn entry_decl(&self, entry: &DefPath) -> Result<&EntryDecl, NativeStop> {
        let binding = self
            .native_entry
            .as_ref()
            .filter(|(path, _)| path == entry)
            .map(|(_, decl)| decl)
            .or_else(|| self.test_entries.get(entry))
            .ok_or_else(|| {
                NativeStop::Entry("no checked entry selected for this definition".into())
            })?;
        binding.as_ref().map_err(|e| NativeStop::Entry(e.clone()))
    }
    /// The entry's ABI from its declaration part and its body's IR, and its
    /// result cleanup module.
    fn entry_abi(
        &self,
        entry: &DefPath,
        decl: &EntryDecl,
    ) -> Result<Result<(EntryAbi, Option<Module>), String>, NativeStop> {
        let product = match self.demand_body(entry) {
            Ok(product) => product,
            Err(_) => return Ok(Err("entry has no checked body product".into())),
        };
        let module = product.ir()?;
        Ok(entry_abi(decl, entry, &product, module))
    }
    /// The entry's ABI and its cleanup object. A Cranelift development build
    /// reads both from the entry record and the cleanup's instance record, and
    /// demands the entry body's IR only on a miss; LLVM keeps the v4 key for
    /// the cleanup object.
    fn bind_entry_native(
        &self,
        entry: &DefPath,
        options: &NativeOptions,
        inputs: &NativeInputs,
        damaged: &BTreeSet<Identity>,
    ) -> Result<(EntryAbi, Option<ObjectProduct>), NativeStop> {
        let decl = self.entry_decl(entry)?;
        let store = Store::new(&options.store)?;
        let Some(records) = &inputs.records else {
            let (abi, cleanup) = self.entry_abi(entry, decl)?.map_err(NativeStop::Entry)?;
            let Some(module) = cleanup else {
                return Ok((abi, None));
            };
            let env = environment(options, inputs.toolchain.runtime)?;
            let object_key = environment_key(
                "hirsel.core-entry-cleanup.v4",
                &env,
                &crate::body::ir_hash(&module, &mut Vec::new())?,
            )?;
            let (held, object, compiled) = if let Some((bare, object, file)) =
                crate::native_object::load(&store, object_key)?
            {
                ((bare, file), object, false)
            } else {
                let object = emit_unit(
                    &module,
                    || module.link_symbols(),
                    options,
                    object_key,
                    &cleanup_identity(&module),
                )
                .map_err(|message| NativeStop::Emit {
                    definition: entry.clone(),
                    message,
                })?;
                (
                    crate::native_object::publish(&store, object_key, &object)?,
                    object,
                    true,
                )
            };
            let object = ObjectProduct::held(
                cleanup_identity(&module),
                &store,
                held,
                object,
                None,
                compiled,
            );
            return Ok((abi, Some(object)));
        };
        let Ok(product) = self.demand_body(entry) else {
            return Err(NativeStop::Entry(
                "entry has no checked body product".into(),
            ));
        };
        let own = product.record;
        // The entry record is keyed by the environment, the entry's identity,
        // its body record's address, the link-plan policy and the declaration
        // part. The address commits to the entry module, so to the closures of
        // the `Result` and `Exit` rows the ABI is read from.
        let entry_key = environment_key(
            "hirsel.core-entry.v1",
            &records.env,
            &(
                Identity::plain(entry.clone()),
                own,
                crate::native_entry::policy(options),
                decl,
            ),
        )?;
        let mut computed = None;
        let record = match entry_record(&store, entry_key)? {
            Some(record) => record,
            None => {
                let bound = self.entry_abi(entry, decl)?;
                let record = EntryRecord {
                    schema: ENTRY_SCHEMA,
                    abi: bound
                        .as_ref()
                        .map(|(abi, _)| abi.clone())
                        .map_err(Clone::clone),
                    cleanup: bound
                        .as_ref()
                        .ok()
                        .and_then(|(_, m)| m.as_ref())
                        .map(cleanup_identity),
                };
                publish(&store, entry_key, &encode(&record)?)?;
                computed = bound.ok().and_then(|(_, m)| m);
                record
            }
        };
        let abi = record.abi.map_err(NativeStop::Entry)?;
        let Some(identity) = record.cleanup else {
            return Ok((abi, None));
        };
        // The cleanup is a function of the entry module: its key takes the
        // entry body's address as its `own`.
        let object_key = records.key(&identity, Some(own))?;
        let hit = match damaged.contains(&identity) {
            true => None,
            false => crate::native_object::load_instance(&store, object_key)?.filter(|r| {
                r.identity == identity && r.own == Some(own) && r.env == records.digest
            }),
        };
        if let Some(record) = hit {
            return Ok((
                abi,
                Some(ObjectProduct::stored(
                    identity,
                    &store,
                    record.object,
                    record.debug,
                )),
            ));
        }
        let module = match computed {
            Some(module) => module,
            None => self
                .entry_abi(entry, decl)?
                .map_err(NativeStop::Entry)?
                .1
                .ok_or_else(|| NativeStop::Entry("entry cleanup is unavailable".into()))?,
        };
        let object = emit_unit(
            &module,
            || module.link_symbols(),
            options,
            object_key,
            &identity,
        )
        .map_err(|message| NativeStop::Emit {
            definition: entry.clone(),
            message,
        })?;
        let code = hirsel_core_ir::normal::encode(
            &module,
            hirsel_core_ir::FuncId((module.functions.len() - 1) as u32),
        );
        let record = crate::native_object::Instance {
            schema: crate::native_object::INSTANCE_SCHEMA,
            identity: identity.clone(),
            own: Some(own),
            body: code.hash(),
            env: records.digest,
            object: Default::default(),
            debug: None,
            children: vec![],
            slots: code
                .slots
                .iter()
                .map(|slot| slot.identity(&module))
                .collect(),
            signatures: Default::default(),
            adapters: vec![],
            described: code.described,
            items: module.items.clone(),
            closures: vec![],
        };
        let (bare, debug, held) =
            crate::native_object::publish_instance(&store, object_key, &object, record)?;
        Ok((
            abi,
            Some(ObjectProduct::held(
                identity,
                &store,
                (bare, held),
                object,
                debug,
                true,
            )),
        ))
    }
}
/// The entry's ABI: its result's recipe, where an `Exit` status sits in it,
/// and its provider parameters. Read from the entry body's module, in the
/// build; the declaration part comes from check.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EntryAbi {
    pub(super) recipe: Recipe,
    pub(super) exit_offset: Option<u64>,
    pub(super) parameters: Vec<EntryParameter>,
}
/// The part of an entry's binding that check reads from the declaration
/// table, while it is live: the error is `Exit`, and the provider parameters.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EntryDecl {
    pub(super) exit: bool,
    pub(super) parameters: Vec<EntryParameter>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct EntryParameter {
    pub(super) ty: String,
    pub(super) name: String,
}
pub(crate) type EntryBinding = (DefPath, Result<EntryDecl, String>);
/// A Cranelift development entry record (`hirsel.core-entry.v1`): the ABI
/// read from the entry body's module, and the identity of its result cleanup.
#[derive(Serialize, Deserialize)]
struct EntryRecord {
    schema: u32,
    abi: Result<EntryAbi, String>,
    cleanup: Option<Identity>,
}
const ENTRY_SCHEMA: u32 = 1;
fn entry_record(store: &Store, key: ContentHash) -> io::Result<Option<EntryRecord>> {
    Ok(cached(store, key)?
        .and_then(|bytes| crate::decode::<EntryRecord>(&bytes))
        .filter(|r| r.schema == ENTRY_SCHEMA))
}
/// A cleanup module's own instance: its last.
fn cleanup_identity(module: &Module) -> Identity {
    module.identity(hirsel_core_ir::InstanceId(
        module.instances.len() as u32 - 1,
    ))
}
/// Check's part of an entry binding, from the declaration table alone.
pub(crate) fn bind_entry(p: &Program<'_>, entry: &DefPath) -> EntryBinding {
    use hirsel_core_front::TyKind;
    let binding = (|| {
        let fd = p.function(entry).ok_or("entry declaration unavailable")?;
        let TyKind::Adt(result, args) = p.types.kind(fd.result) else {
            return Err("PROG-3 requires Result[(), E]");
        };
        let result = &p.decls[result.index()];
        if result.path != DefPath::from_full("core@1/core/Result").unwrap()
            || args.len() != 2
            || !matches!(p.types.kind(args[0]), TyKind::Unit)
        {
            return Err("PROG-3 requires Result[(), E]");
        }
        let exit = matches!(
            p.types.kind(args[1]),
            TyKind::Adt(d, a) if a.is_empty()
                && p.decls[d.index()].path == DefPath::from_full("core@1/core/Exit").unwrap()
        );
        let parameters = fd
            .params
            .iter()
            .map(|param| {
                let TyKind::Adt(d, args) = p.types.kind(param.ty) else {
                    return Err("entry parameter has no provider handle type");
                };
                let mut ty = p.decls[d.index()].path.to_string();
                if ty == "core@1/core/Foreign" {
                    let [argument] = args.as_slice() else {
                        return Err("Foreign requires its interface");
                    };
                    let TyKind::Adt(interface, _) = p.types.kind(*argument) else {
                        return Err("Foreign requires its interface");
                    };
                    ty.push('[');
                    ty.push_str(&p.decls[interface.index()].path.definition_identity());
                    ty.push(']');
                }
                Ok(EntryParameter {
                    ty,
                    name: p.index.symbols.text(param.name).to_owned(),
                })
            })
            .collect::<Result<Vec<_>, &str>>()?;
        Ok(EntryDecl { exit, parameters })
    })()
    .map_err(str::to_owned);
    (entry.clone(), binding)
}
/// The build's part of an entry binding, from the entry body's module.
fn entry_abi(
    decl: &EntryDecl,
    entry: &DefPath,
    product: &DefinitionProduct,
    module: &Module,
) -> Result<(EntryAbi, Option<Module>), String> {
    let function = product
        .check
        .function
        .ok_or("entry body is not fully lowered")?;
    bound_entry_abi(decl, entry, module, function)
}

fn bound_entry_abi(
    decl: &EntryDecl,
    entry: &DefPath,
    module: &Module,
    function: hirsel_core_ir::FuncId,
) -> Result<(EntryAbi, Option<Module>), String> {
    let binding = (|| {
        let signature = &module.signatures[module.functions[function.index()].signature.index()];
        let dest = signature
            .dest
            .ok_or("entry result has no emitted destination")?;
        let recipes =
            hirsel_core_layout::compute(&module.types).map_err(|_| "invalid entry layout")?;
        let recipe = recipes[dest.index()].clone();
        let exit_offset = if decl.exit {
            let hirsel_core_layout::Body::Variant(cases) = &module.types[dest.index()].body else {
                return Err("entry Result needs a variant");
            };
            let Some(hirsel_core_layout::FieldTy::Inline(exit)) = cases
                .get(1)
                .and_then(|case| case.fields.first())
                .map(|field| &field.ty)
            else {
                return Err("entry Exit needs an inline result payload");
            };
            let Layout::Fixed(result) = &recipe.layout else {
                return Err("entry Result is not fixed");
            };
            let Layout::Fixed(exit) = &recipes[exit.index()].layout else {
                return Err("entry Exit is not fixed");
            };
            let offset = result
                .offsets
                .get(1)
                .and_then(|fields| fields.first())
                .zip(exit.offsets.first().and_then(|fields| fields.first()))
                .ok_or("entry Exit has no status field")?;
            Some(offset.0 + offset.1)
        } else {
            None
        };
        let cleanup = (!recipe.plain).then(|| result_cleanup(module, dest, entry));
        Ok((
            EntryAbi {
                recipe,
                exit_offset,
                parameters: decl.parameters.clone(),
            },
            cleanup,
        ))
    })();
    binding.map_err(|e: &str| e.to_owned())
}

/// Runtime CAP-4 kind numbers. Args is always supplied by the process constructor.
pub fn grant_kind(name: &str) -> Option<u8> {
    recipe_build::kind(name)
}

pub(crate) fn command(command: &mut Command) -> Result<Vec<u8>, NativeStop> {
    let result = command
        .output()
        .map_err(|e| NativeStop::Link(format!("launch {command:?}: {e}")))?;
    if !result.status.success() {
        return Err(NativeStop::Link(format!(
            "{command:?}: {}\n{}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(result.stdout)
}

fn direct_link(
    recipe: &crate::tool_inputs::LinkRecipe,
    inputs: &[PathBuf],
    support_libs: &[&str],
    output: &Path,
    optimize: bool,
) -> Result<(), NativeStop> {
    // The editor runs under an empty environment, in an empty directory of
    // its own: a script's relative name finds nothing there
    // (crate::link_closure). It writes a fixed relative name there, since
    // the output's name enters the executable (a version script's base
    // definition), and the executable is then moved to `output`.
    let parent = output.parent().filter(|p| !p.as_os_str().is_empty());
    let directory = tempfile::tempdir_in(parent.unwrap_or(Path::new(".")))?;
    let mut process = crate::tool_inputs::tool_command(&recipe.linker);
    process.current_dir(directory.path());
    if optimize {
        // Keep the executable's code revision even when no tape reads it.
        process.args(["--gc-sections", "--undefined=hirsel_code_revision"]);
    }
    let mut inserted = false;
    for argument in &recipe.args {
        if argument == crate::tool_inputs::INPUT_MARKER {
            for input in inputs {
                process.arg(std::path::absolute(input)?);
            }
            for library in support_libs {
                process.arg(library);
            }
            inserted = true;
        } else {
            process.arg(argument);
        }
    }
    if !inserted {
        return Err(NativeStop::Link(
            "direct link recipe has no input marker".into(),
        ));
    }
    if !optimize {
        process.arg("-O0");
    }
    process.arg("-o").arg("program");
    command(&mut process)?;
    std::fs::rename(directory.path().join("program"), output)?;
    Ok(())
}

/// Raise the soft descriptor limit to the hard limit once, as Go does since
/// 1.19: a link holds one descriptor per object. The limit a link may use.
fn descriptor_limit() -> u64 {
    static LIMIT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit and setrlimit read and write the struct passed.
        unsafe {
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
                return 1024;
            }
            if limit.rlim_cur < limit.rlim_max {
                let raised = libc::rlimit {
                    rlim_cur: limit.rlim_max,
                    rlim_max: limit.rlim_max,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &raised) == 0 {
                    limit = raised;
                }
            }
        }
        limit.rlim_cur
    })
}

#[cfg(test)]
thread_local! {
    /// Runs when the link starts, before it opens or checks any object, with
    /// the objects' paths.
    pub(crate) static BEFORE_OPEN: std::cell::RefCell<Option<Box<dyn FnMut(&[PathBuf])>>> =
        const { std::cell::RefCell::new(None) };
    /// The image unit the last link on this thread read: its key, its file
    /// and whether the link prelinked it.
    pub(crate) static UNIT: std::cell::RefCell<Option<(ContentHash, PathBuf, bool)>> =
        const { std::cell::RefCell::new(None) };
    /// Set: a link reads the image objects one by one, not as their unit.
    pub(crate) static SEPARATE_IMAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Runs between the objects' check and the linker's spawn, with their paths.
    pub(crate) static AFTER_CHECK: std::cell::RefCell<Option<Box<dyn FnMut(&[PathBuf])>>> =
        const { std::cell::RefCell::new(None) };
}

/// One descriptor per object, through which its bytes were checked or
/// written: an object this request read or emitted brings its own, and a
/// stored object named by a record is opened once and checked through that
/// descriptor. The linker names each by `/proc/<pid>/fd/N` of this process,
/// which resolves to that inode, so no store path is reopened after a check.
/// Every stored object is checked; each that fails is removed, and the
/// instances of all of them are returned.
fn open_objects(
    objects: &[&ObjectProduct],
    counters: &mut crate::reach::ReachCounters,
) -> Result<Result<Vec<std::fs::File>, Vec<Identity>>, NativeStop> {
    use std::io::Read;
    let limit = descriptor_limit();
    if objects.len() as u64 + 64 > limit {
        return Err(NativeStop::Link(format!(
            "{} objects exceed the descriptor limit of {limit}",
            objects.len()
        )));
    }
    #[cfg(test)]
    BEFORE_OPEN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(&objects.iter().map(|o| o.path.clone()).collect::<Vec<_>>());
        }
    });
    let mut files = Vec::with_capacity(objects.len());
    let mut refused = Vec::new();
    let mut bytes = Vec::new();
    for object in objects {
        if let Some(held) = &object.held {
            files.push(held.try_clone()?);
            continue;
        }
        let mut file = match std::fs::File::open(&object.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                refused.push(object.instance.clone());
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        crate::meter::count(crate::meter::Count::FilesOpened, 1);
        crate::meter::count(crate::meter::Count::ObjectsOpened, 1);
        bytes.clear();
        file.read_to_end(&mut bytes)?;
        counters.objects_verified += 1;
        if crate::store::checksum(&bytes) != object.check {
            drop(file);
            match std::fs::remove_file(&object.path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            refused.push(object.instance.clone());
            continue;
        }
        files.push(file);
    }
    if !refused.is_empty() {
        return Ok(Err(refused));
    }
    #[cfg(test)]
    AFTER_CHECK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(&objects.iter().map(|o| o.path.clone()).collect::<Vec<_>>());
        }
    });
    Ok(Ok(files))
}

#[allow(clippy::too_many_arguments)]
fn link_objects(
    entry_plan: &crate::native_entry::Plan<'_>,
    objects: &[ObjectProduct],
    link_archives: &crate::foreign_archive::LinkArchives,
    image: Option<&Store>,
    options: &NativeOptions,
    sources_digest: ContentHash,
    sources: impl FnOnce() -> io::Result<hirsel_core_ir::source::SourceMap>,
    toolchain: ContentHash,
    recipe: crate::tool_inputs::LinkRecipe,
    closed_image: bool,
    mut counters: crate::reach::ReachCounters,
    record: impl FnOnce(ContentHash) -> io::Result<u64>,
    address: ContentHash,
    revisions: impl FnOnce() -> Result<BTreeMap<Identity, ContentHash>, NativeStop>,
    assembly: Option<(ContentHash, ContentHash)>,
) -> Result<Linked, NativeStop> {
    let revision = hirsel_core_ir::code_revision(address);
    // The executable key includes the mode-specific lld options.
    let link_mode = if options.optimize {
        "release-link-gc-sections"
    } else {
        "development-link-O0"
    };
    let identities: Vec<_> = objects.iter().map(|o| o.identity).collect();
    let archive_pins: Vec<_> = link_archives.pins().collect();
    // The image objects are linked as their image unit. The source map
    // enters by its digest, and the runtime headers by the compiler identity,
    // which embeds them.
    let link_key = match assembly {
        Some(assembly) => key("hirsel.recipe-final-link.v1", &assembly)?,
        None => key(
            "hirsel.core-executable.v6",
            &(
                compiler_identity()?,
                toolchain,
                recipe.identity,
                DIRECT_LINK_SUPPORT_LIBS,
                entry_plan,
                &identities,
                &archive_pins,
                sources_digest,
                &options.target,
                options.backend,
                options.optimize,
                options.debug_lines,
                link_mode,
            ),
        )?,
    };
    let store = Store::new(&options.store)?;
    let (bytes, linked) = if let Some(bytes) = cached(&store, link_key)? {
        (bytes, false)
    } else {
        let mut sources = sources()?;
        let temp = store.scratch()?;
        let entry = temp.path().join("entry.o");
        // The entry's and the described instances' revisions are a function
        // of the objects, so of the key: they are made only for a miss.
        let revisions: Vec<_> = revisions()?
            .into_iter()
            .map(|(identity, revision)| {
                (
                    hirsel_core_ir::revision_symbol(identity.digest()),
                    revision.0,
                )
            })
            .collect();
        let output = temp.path().join("program");
        let compiled = objects.iter().filter(|o| o.compiled).count();
        let debug = if options.debug_lines {
            let mut rows = Vec::with_capacity(objects.len());
            let mut refused = Vec::new();
            for object in objects {
                match (&object.debug, &object.debug_record) {
                    (Some(debug), _) => rows.push(Cow::Borrowed(debug)),
                    (None, Some((store, address))) => {
                        match crate::native_object::load_debug(store, *address)? {
                            Some(debug) => rows.push(Cow::Owned(debug)),
                            None => refused.push(object.instance.clone()),
                        }
                    }
                    (None, None) => {}
                }
            }
            if !refused.is_empty() {
                return Ok(Linked::Damaged(refused, compiled, counters));
            }
            let debug = crate::native_debug::bind_side_products(
                rows.iter().map(|d| &**d),
                &sources,
                &options.target,
            )
            .map_err(|error| NativeStop::Link(format!("native debug binding: {error}")))?;
            Some(debug)
        } else {
            None
        };
        let expected_link = recipe.identity;
        let recapture = || {
            let recipe = crate::tool_inputs::recapture_link_recipe(&options.link, &options.target)
                .map_err(|error| NativeStop::Link(format!("direct link refused: {error}")))?;
            if assembly.is_some() && recipe.identity != expected_link {
                return Err(NativeStop::Link(
                    "link inputs changed during recipe assembly".into(),
                ));
            }
            if closed_image && !recipe.uses_hirsel_launcher() {
                return Err(NativeStop::Link(
                    "direct link refused: process entry inputs changed".into(),
                ));
            }
            Ok(recipe)
        };
        let mut recipe = recipe;
        let mut missing = crate::tool_inputs::missing_recipe_inputs(&recipe);
        if !missing.is_empty() {
            recipe = recapture()?;
            missing = crate::tool_inputs::missing_recipe_inputs(&recipe);
        }
        if !missing.is_empty() {
            return Err(NativeStop::Link(format!(
                "direct link refused: missing recipe inputs: {}",
                missing
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        // The image objects are linked as their unit, prelinked here when no
        // unit is stored under their key; the rest are linked one by one.
        #[cfg(test)]
        let image = image.filter(|_| !SEPARATE_IMAGE.get());
        let (in_image, separate): (Vec<&ObjectProduct>, Vec<&ObjectProduct>) = objects
            .iter()
            .partition(|o| image.is_some_and(|image| image.holds(&o.path)));
        let unit = match image.filter(|_| !in_image.is_empty()) {
            None => None,
            Some(image) => {
                let identities: Vec<_> = in_image.iter().map(|o| o.identity).collect();
                let unit_key =
                    crate::image_unit::key(toolchain, &recipe, &options.target, &identities)?;
                // The unit and whether this link prelinked it, which a test reads.
                let (_unit, file, _prelinked) = match crate::image_unit::load(image, unit_key)? {
                    Some((unit, file)) => (unit, file, false),
                    None => {
                        let files = match open_objects(&in_image, &mut counters)? {
                            Ok(files) => files,
                            Err(refused) => {
                                return Ok(Linked::Damaged(refused, compiled, counters));
                            }
                        };
                        let (unit, file) =
                            crate::image_unit::prelink(image, unit_key, &recipe.linker, &files)?;
                        (unit, file, true)
                    }
                };
                #[cfg(test)]
                UNIT.set(Some((unit_key, image.path(_unit.address), _prelinked)));
                Some(file)
            }
        };
        let files = match open_objects(&separate, &mut counters)? {
            Ok(files) => files,
            Err(refused) => return Ok(Linked::Damaged(refused, compiled, counters)),
        };
        // The linker opens each checked object through this process's
        // descriptor table, so it reads the inode that was checked and inherits
        // nothing: a spawn that cleared close-on-exec would need fork, whose
        // child holds every thread's writable descriptors until it execs.
        let pid = std::process::id();
        let mut inputs = Vec::with_capacity(objects.len() + 4);
        inputs.push(entry.clone());
        inputs.extend(unit.iter().chain(&files).map(|file| {
            PathBuf::from(format!(
                "/proc/{pid}/fd/{}",
                std::os::fd::AsRawFd::as_raw_fd(file)
            ))
        }));
        crate::native_entry::retain_referenced(&mut sources, &inputs[1..])?;
        let bytes = entry_plan
            .emit(&options.target, &sources, &revision.0, &revisions)
            .map_err(|error| NativeStop::Link(format!("entry object: {error}")))?;
        std::fs::write(&entry, bytes)?;
        if let Some(debug) = debug {
            let debug_path = temp.path().join("debug.o");
            std::fs::write(&debug_path, debug)?;
            inputs.push(debug_path);
        }
        let link = |recipe| {
            let mut inputs = inputs.clone();
            link_archives.append_link_inputs(&mut inputs, &options.runtime, recipe)?;
            inputs.push(options.runtime.clone());
            direct_link(
                recipe,
                &inputs,
                DIRECT_LINK_SUPPORT_LIBS,
                &output,
                options.optimize,
            )
        };
        if let Err(first) = link(&recipe) {
            if matches!(first, NativeStop::Archive(_)) {
                return Err(first);
            }
            let missing = crate::tool_inputs::missing_recipe_inputs(&recipe);
            if missing.is_empty() {
                return Err(first);
            }
            let refreshed = recapture()?;
            let remaining = crate::tool_inputs::missing_recipe_inputs(&refreshed);
            if !remaining.is_empty() {
                return Err(NativeStop::Link(format!(
                    "direct link refused: missing recipe inputs: {}",
                    remaining
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            link(&refreshed)?;
        }
        drop((unit, files));
        let bytes = std::fs::read(output)?;
        crate::native_entry::check_linked_sites(&bytes).map_err(NativeStop::Link)?;
        publish(&store, link_key, &bytes)?;
        (bytes, true)
    };
    // The reach record names the stored executable; an output failure
    // below leaves both reusable.
    match record(link_key)? {
        0 => {}
        written => counters.record_bytes = written,
    }
    let product = BuildProduct {
        private_variants: objects.iter().map(|o| o.private_variants).sum(),
        executable: options.output.clone(),
        identity: link_key,
        address,
        recipe: assembly.map(|(root, _)| root),
        assembly: assembly
            .map(|(root, toolchain)| recipe_build::AssemblyRecord::new(root, toolchain, &bytes)),
        revision,
        compiled: objects.iter().filter(|o| o.compiled).count(),
        linked,
        observation_mode: options.logical_charge_report.is_some(),
        debug_lines: options.debug_lines,
        reach: counters,
    };
    // Copy from integrity-checked bytes even on a hit; a damaged materialized file
    // is never trusted as the executable cache product.
    match deliver(&options.output, &bytes) {
        Ok(()) => Ok(Linked::Built(product)),
        Err(error) => Err(NativeStop::Output {
            product: Box::new(product),
            error,
        }),
    }
}
/// How a request reports that writing its executable to the output failed.
pub(crate) fn output_stop(error: &io::Error) -> String {
    format!("output: {error}")
}
/// Write a checked executable's `bytes` to `output`, its directory made.
pub(crate) fn deliver(output: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    crate::executable::write(output, bytes, 0o755)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn source_request(source: &str, dir: &Path) -> CheckRequest {
        let mut request = crate::tests::request(source, dir);
        let core = dir.join("core.hirsel");
        std::fs::write(&core, "pub type Result[T, E] = Ok(T) | Error(E)\npub type Exit: key { status: u8 }\npub opaque type Args\npub opaque type Stdout\npub opaque type Dir\npub opaque type Net\npub opaque type Clock\npub opaque type Random\npub opaque type Stderr\npub opaque type Stdin\npub opaque type Env\npub opaque type Process\npub opaque type Tasks\n").unwrap();
        request.inputs.packages[0]
            .dependencies
            .push(package("core"));
        request.inputs.packages.push(PackageManifest {
            namespace: None,
            package: package("core"),
            sources: vec![hirsel_core_resolve::ModuleSource {
                foreign: false,
                captured: None,
                file: core,
                modules: vec!["core".into()],
                signatures: true,
            }],
            dependencies: vec![],
        });
        request.entry = Some(DefPath::from_full("app@1/main/main").unwrap());
        request
    }
    include!("core_ops_native.rs");
    include!("provider_m3_native.rs");
    include!("list_native.rs");
    include!("numeric_native.rs");
    include!("instance_native.rs");
    include!("callable_native.rs");
    include!("finalizer_native.rs");
    include!("startup_native.rs");
    include!("test_rules_native.rs");
    include!("test_run_native.rs");
    include!("clone_native.rs");
    include!("record_native.rs");
    include!("source_sites_native.rs");
    include!("facts_native.rs");
    include!("object_key_native.rs");
    include!("instance_record_native.rs");
    include!("prebuild_native.rs");
    include!("image_unit_native.rs");
    include!("std_release_native.rs");
    include!("reach_native.rs");
    include!("link_closure_native.rs");
    include!("recording_build_native.rs");
    include!("request_sequences.rs");
    include!("revision_native.rs");
    include!("build_identity.rs");

    #[test]
    fn fmt_files_match_check_sites_and_prepare_the_whole_directory_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("a.hirsel");
        let good_source = b"fn a(){1}";
        std::fs::write(&good, good_source).unwrap();
        let bad_source = "// header\r\nfn bad() { /* é→😀 */ # }\r\n";
        let request = crate::tests::request(bad_source, dir.path());
        let bad = &request.inputs.packages[0].sources[0].file;
        let manifest = dir.path().join("inputs.json");
        std::fs::write(&manifest, serde_json::to_vec(&request.inputs).unwrap()).unwrap();

        let run = |args: &[&str]| {
            let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
            process::tape_request(&args, dir.path())
        };
        let (status, _, check_stderr) = run(&[
            "check",
            "--inputs",
            manifest.to_str().unwrap(),
            "--store",
            request.store.to_str().unwrap(),
        ]);
        assert_eq!(status, Some(1), "{check_stderr}");
        let (status, stdout, fmt_stderr) = run(&["fmt", dir.path().to_str().unwrap()]);
        assert_eq!(status, Some(1), "{fmt_stderr}");
        // There are 21 scalars before `#` on line 2, including three multibyte scalars.
        let site = format!("{}:2:22-23", bad.display());
        assert!(
            check_stderr.contains(&format!("{site}: LANG-1 ")),
            "{check_stderr}"
        );
        assert!(
            fmt_stderr.contains(&format!("{site}: LANG-1: ")),
            "{fmt_stderr}"
        );
        assert_eq!(stdout, format!("{}\n", good.display()));
        assert_eq!(std::fs::read(&good).unwrap(), good_source);
        assert_eq!(std::fs::read(bad).unwrap(), bad_source.as_bytes());

        let (status, stdout, stderr) = run(&["fmt", "--check", good.to_str().unwrap()]);
        assert_eq!(status, Some(1), "{stderr}");
        assert_eq!(stdout, format!("{}\n", good.display()));
        assert_eq!(std::fs::read(&good).unwrap(), good_source);
        let (status, _, stderr) = run(&["fmt", good.to_str().unwrap()]);
        assert_eq!(status, Some(0), "{stderr}");
        assert_eq!(std::fs::read(&good).unwrap(), b"fn a() {\n    1\n}\n");
    }

    #[test]
    fn fmt_stdin_checks_exact_bytes_and_renders_refusals() {
        use std::io::Write as _;
        use std::process::Stdio;

        let dir = tempfile::tempdir().unwrap();
        let run = |opts: &[&str], source: &[u8]| {
            let args = ["fmt", "--stdin"]
                .into_iter()
                .chain(opts.iter().copied())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let mut child = process::tape_request_command(&args, dir.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(source).unwrap();
            let output = child.wait_with_output().unwrap();
            let payload = |bytes: Vec<u8>| {
                String::from_utf8(bytes)
                    .unwrap()
                    .split_once(process::CHILD_MARK)
                    .unwrap()
                    .1
                    .to_owned()
            };
            (
                output.status.code(),
                payload(output.stdout),
                payload(output.stderr),
            )
        };
        let input = b"pub fn f(x:u64)->u64\r\n";
        let canonical = "pub fn f(x: u64) -> u64\n";
        let (status, stdout, stderr) = run(&["--signatures"], input);
        assert_eq!(
            (status, stdout.as_str(), stderr.as_str()),
            (Some(0), canonical, "")
        );
        for (source, expected) in [(input.as_slice(), 1), (canonical.as_bytes(), 0)] {
            let (status, stdout, stderr) = run(&["--check", "--signatures"], source);
            assert_eq!(
                (status, stdout.as_str(), stderr.as_str()),
                (Some(expected), "", "")
            );
        }
        let (status, stdout, stderr) = run(&[], b"// ok\r\n\xff");
        assert_eq!((status, stdout.as_str()), (Some(1), ""));
        assert_eq!(stderr, "<stdin>:2:1-2: LANG-1: source is not UTF-8\n");
        let (status, stdout, stderr) =
            run(&[], "// header\r\nfn bad() { /* é→😀 */ # }\r\n".as_bytes());
        assert_eq!((status, stdout.as_str()), (Some(1), ""));
        assert!(stderr.contains("<stdin>:2:22-23: LANG-1: "), "{stderr}");
    }

    fn run_clean(binary: &Path, exit: i32) {
        let result = Command::new(binary).output().unwrap();
        assert_eq!(
            result.status.code(),
            Some(exit),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
    }
    pub(crate) fn native_options(backend: Backend, dir: &Path) -> NativeOptions {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        NativeOptions {
            recording: false,
            replay: None,
            backend,
            optimize: false,
            debug_lines: false,
            target: TARGET.into(),
            runtime: root
                .join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native runtime archive")),
            link: Link::Driver(
                PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang"),
            ),
            store: dir.join("native"),
            parts: dir.join("parts"),
            output: dir.join("program"),
            root_limit: 1_048_576,
            // Glue operations are instances of their own: a program's walk counts each.
            instance_limit: 256,
            directories: vec![],
            grant_mask: 0,
            net_rules: vec![],
            secret_grants: vec![],
            foreign_grants: BTreeSet::new(),
            exec_grants: vec![],
            env_patterns: None,
            logical_charge_report: None,
            observed: None,
        }
    }

    #[test]
    fn shared_foreign_thunk_links_sealed_code_and_runs_from_each_backend() {
        use crate::code_store::ObjectKind;
        use crate::foreign_archive::LinkArchives;
        use hirsel_core_ir::release_recipe::{ForeignFunction, ForeignLeaf, ObjectRef};

        let dir = tempfile::tempdir().unwrap();
        let path = |name| dir.path().join(name);
        let mut options = native_options(Backend::Cranelift, dir.path());
        options.foreign_grants.insert("ffi_laws@1/laws/Laws".into());
        let Link::Driver(cc) = &options.link else {
            unreachable!()
        };
        let adapter = path("adapter.c");
        std::fs::write(
            &adapter,
            r#"
#include <stdint.h>
static uint8_t cell;
static uint64_t released;
uint64_t hfl_measure(int8_t s, float f) { return s == -3 && f == 1.25f ? 42 : 0; }
void *hfl_make(void) { return &cell; }
void hfl_free(void *p) { if (p == &cell) released++; }
uint64_t hfl_freed(void) { return released; }
#define ABI "hfl_measure waits (i8, f32) -> u64\n" \
            "hfl_make stack () -> new ffi_laws@1/laws/Obj\n" \
            "hfl_free waits (take ffi_laws@1/laws/Obj) -> ()\n" \
            "hfl_freed stack () -> u64\n"
__attribute__((used, section(".hirsel_abi")))
static const char abi[sizeof(ABI) - 1] = ABI;
"#,
        )
        .unwrap();
        command(
            Command::new(cc)
                .args(["-std=c11", "-O2", "-fPIC", "-c"])
                .arg(adapter)
                .arg("-o")
                .arg(path("adapter.o")),
        )
        .unwrap();
        command(
            Command::new(cc.with_file_name("llvm-ar"))
                .arg("rcs")
                .arg(path("adapter.a"))
                .arg(path("adapter.o")),
        )
        .unwrap();
        let bytes = std::fs::read(path("adapter.a")).unwrap();
        let archive = ObjectRef {
            hash: ObjectKind::ForeignArchive.hash(&bytes),
            len: bytes.len() as u64,
        };
        let entry = DefPath::from_full("ffi_laws@1/laws/main").unwrap();
        let identity = Identity::plain(entry.clone());
        let leaf = ForeignLeaf {
            module: DefPath::from_full("ffi_laws@1/native").unwrap(),
            symbols: "hfl_".into(),
            targets: vec![],
            functions: [
                ("measure", "(core@1/core/Foreign[ffi_laws@1/laws/Laws],i8,f32)->u64 waits"),
                ("make", "(core@1/core/Foreign[ffi_laws@1/laws/Laws])->core@1/core/Option[core@1/foreign/Ptr[ffi_laws@1/laws/Obj]]"),
                ("free", "(core@1/core/Foreign[ffi_laws@1/laws/Laws],take core@1/foreign/Ptr[ffi_laws@1/laws/Obj])->()waits"),
                ("freed", "(core@1/core/Foreign[ffi_laws@1/laws/Laws])->u64"),
            ].into_iter().map(|(name, header)| ForeignFunction {
                identity: Identity::plain(DefPath::from_full(&format!("ffi_laws@1/native/{name}")).unwrap()),
                header: header.into(),
            }).collect(),
        };
        let mut archives = LinkArchives::default();
        archives
            .admit(archive, &leaf, TARGET, None, || Ok(bytes))
            .unwrap();
        let module = hirsel_core_ir::text::parse(
            r#"
core v1
type t0 = own "Result" | Ok { } | Error { }
type t1 = own "core@1/foreign/Ptr[ffi_laws@1/laws/Obj]" value u64 { handle: u64 }
type t2 = own "core@1/core/Option[core@1/foreign/Ptr[ffi_laws@1/laws/Obj]]" | None { } | Some { p: t1 }
type t3 = resource "ffi_laws@1/laws/Owner" drop f4 { p: t2 }
sig g0 = (u64) -> () dest t0
sig g1 = (u64, i8, f32) -> (u64)
sig g2 = (u64) -> () dest t2
sig g3 = (u64, u64) -> ()
sig g4 = (ptr) -> ()
sig g5 = (u64) -> (u64)
inst n0 = ffi_laws@1/laws/main [] []
inst n1 = ffi_laws@1/native/measure [] []
inst n2 = ffi_laws@1/native/make [] []
inst n3 = ffi_laws@1/native/free [] []
inst n4 = ffi_laws@1/laws/finalize [] []
inst n5 = ffi_laws@1/native/freed [] []
fn f0 = n0 g0 {
 slot s0 = t3
 addr a0 = v0 : t0
 addr a1 = s0 : t3
 addr a2 = s0 : ptr
 addr a3 = v9 : t3
 addr a4 = a3 field 0 : t2
b0(v0: ptr, v1: u64):
 address a0
 v2: i8 = const -3
 v3: f32 = const 0x3fa00000
 v4: u64 = call f1 g1 (v1, v2, v3)
 v5: u64 = const 42
 v6: bool = eq v4, v5
 branch v6, b1, b3
b1:
 v7: u64 = const 16
 v8: u64 = const 48
 v9: ptr, v10: u64 = alloc resource grant 0 v7 align 8 charge v8
 v11: ptr = const 0x0
 v12: bool = ne v9, v11
 branch v12, b2, b3
b2:
 live s0
 address a1
 address a2
 address a3
 address a4
 call f2 g2 (&a4, v1)
 store a2, v9
 drop a1 t3
 dead s0
 v13: u64 = call f5 g5 (v1)
 v14: u64 = const 1
 v15: bool = eq v13, v14
 branch v15, b4, b3
b3:
 set_case a0, 1
 return
b4:
 set_case a0, 0
 return
}
fn f1 = n1 g1 extern
fn f2 = n2 g2 extern
fn f3 = n3 g3 extern
fn f4 = n4 g4 {
b0(v0: ptr):
 return
}
fn f5 = n5 g5 extern
"#,
        )
        .unwrap();
        let plan_abi = EntryAbi {
            recipe: hirsel_core_layout::compute(&module.types)
                .unwrap()
                .remove(0),
            exit_offset: None,
            parameters: vec![EntryParameter {
                ty: "core@1/core/Foreign[ffi_laws@1/laws/Laws]".into(),
                name: "f".into(),
            }],
        };
        let (_, recipe) =
            crate::tool_inputs::toolchain(&options.link, TARGET, &options.runtime).unwrap();
        for backend in [Backend::Cranelift, Backend::Llvm] {
            options.backend = backend;
            let object = emit_unit(
                &module,
                || module.link_symbols(),
                &options,
                ContentHash::default(),
                &identity,
            )
            .unwrap();
            let plan = crate::native_entry::Plan::new(
                identity.symbol(),
                entry.to_string(),
                &plan_abi,
                None,
                false,
                &options,
            )
            .unwrap()
            .with_foreign([&leaf]);
            let startup = plan
                .emit(TARGET, &Default::default(), &[0; 32], &[])
                .unwrap();
            std::fs::write(path("unit.o"), object.bytes).unwrap();
            std::fs::write(path("startup.o"), startup).unwrap();
            let mut inputs = vec![path("startup.o"), path("unit.o")];
            archives
                .append_link_inputs(&mut inputs, &options.runtime, &recipe)
                .unwrap();
            inputs.push(options.runtime.clone());
            let output = path("program");
            direct_link(&recipe, &inputs, DIRECT_LINK_SUPPORT_LIBS, &output, false).unwrap();
            run_clean(&output, 0);
        }
    }
    fn exercise(backend: Backend) {
        let dir = tempfile::tempdir().unwrap();
        let mut options = native_options(backend, dir.path());
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let literal = "pub fn main() -> Result[(), Exit] { Error(Exit(status: 7u8)) }\n";
        let mut driver = Driver::default();
        let product = driver
            .check(source_request(literal, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{}",
            serde_json::to_string(&(
                product.machine_diagnostics().unwrap(),
                product.machine_incomplete().unwrap()
            ))
            .unwrap()
        );
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(built.compiled, 1);
        assert!(built.linked);
        run_clean(&built.executable, 7);
        let cached = Driver::default()
            .check(source_request(literal, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(cached.rechecked.is_empty());
        let warm = cached.build(&entry, &options).unwrap();
        assert_eq!(warm.compiled, 0);
        assert!(!warm.linked);
        std::fs::write(&warm.executable, b"damaged executable").unwrap();
        let restored = cached.build(&entry, &options).unwrap();
        assert_eq!(restored.compiled, 0);
        assert!(!restored.linked);
        run_clean(&restored.executable, 7);
        std::fs::remove_file(&restored.executable).unwrap();
        let missing = cached.build(&entry, &options).unwrap();
        assert_eq!(missing.compiled, 0);
        assert!(!missing.linked);
        run_clean(&missing.executable, 7);

        let source = "pub fn main() -> Result[(), Exit] { Error(Exit(status: answer())) }\nfn answer() -> u8 { add(3u8, 4u8) }\nfn add(a: u8, b: u8) -> u8 { a + b }\nfn unused[T](take x: T) -> T { x }\ntype Broken = Missing\n";
        let product = driver
            .check(source_request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(
            !product.accepted(),
            "unreached body/declaration errors still refuse package check"
        );
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(built.compiled, 3);
        run_clean(&built.executable, 7);
        let warm = product.build(&entry, &options).unwrap();
        assert_eq!(warm.compiled, 0);
        assert!(!warm.linked);
        let edited = source.replace("a + b", "a + b + 1u8");
        let product = driver
            .check(source_request(&edited, dir.path()), &mut Meter::new(false))
            .unwrap();
        let changed = product.build(&entry, &options).unwrap();
        assert_eq!(changed.compiled, 1);
        assert!(changed.linked);
        run_clean(&changed.executable, 8);
        let objects = product.emit_reachable(&entry, &options).unwrap();
        let callee = objects
            .iter()
            .find(|o| o.instance.definition.0.name == "add")
            .unwrap();
        let store = Store::new(&options.store).unwrap();
        std::fs::write(store.path(callee.identity), b"bad object").unwrap();
        // The executable's key names the object by identity. A Cranelift
        // build reads a stored object only to link it, so the executable's
        // hit reads none and the next link checks each stored object it did
        // not write: the damaged one is rebuilt, beside the edited caller. An
        // LLVM development build reads every object it emits, so the damaged
        // one is rebuilt at once, under the same identity, and the executable
        // still hits.
        let (rebuilt, repaired_work) = match backend {
            Backend::Cranelift => (0, (2, 1)),
            Backend::Llvm => (1, (1, 0)),
        };
        let warm = product.build(&entry, &options).unwrap();
        assert_eq!((warm.compiled, warm.linked), (rebuilt, false));
        run_clean(&warm.executable, 8);
        let caller = edited.replace("add(3u8, 4u8)", "add(3u8, 5u8)");
        let product = driver
            .check(source_request(&caller, dir.path()), &mut Meter::new(false))
            .unwrap();
        let repaired = product.build(&entry, &options).unwrap();
        assert_eq!(
            (repaired.compiled, repaired.reach.objects_refused),
            repaired_work
        );
        run_clean(&repaired.executable, 9);

        let overflowing = source.replace("add(3u8, 4u8)", "add(255u8, 1u8)");
        let product = driver
            .check(
                source_request(&overflowing, dir.path()),
                &mut Meter::new(false),
            )
            .unwrap();
        let built = product.build(&entry, &options).unwrap();
        let result = Command::new(built.executable).output().unwrap();
        assert_eq!(result.status.code(), Some(101));
        assert!(result.stdout.is_empty());
        assert!(
            String::from_utf8(result.stderr)
                .unwrap()
                .starts_with("Overflow at ")
        );

        let source =
            "pub fn main(a: Args, out: Stdout) -> Result[(), Exit] { Error(Exit(status: 7u8)) }\n";
        let product = driver
            .check(source_request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        let refused = product.build(&entry, &options).unwrap();
        assert_not_started(
            &Command::new(refused.executable).output().unwrap(),
            "not started: grant `out` is not in the run's configuration\n",
        );
        options.grant_mask = 1 << grant_kind("Stdout").unwrap();
        let granted = product.build(&entry, &options).unwrap();
        run_clean(&granted.executable, 7);
        options.grant_mask |= 1 << grant_kind("Stderr").unwrap();
        let changed_grants = product.build(&entry, &options).unwrap();
        assert_eq!(changed_grants.compiled, 0);
        assert!(
            changed_grants.linked,
            "grant configuration is part of the executable key"
        );
        run_clean(&changed_grants.executable, 7);
        let source = "pub fn main(out: Stdout, d: Dir, a: Args, other: Dir, net: Net, clock: Clock, random: Random, err: Stderr, input: Stdin, env: Env, process: Process, tasks: Tasks) -> Result[(), Exit] { Error(Exit(status: 7u8)) }\n";
        let product = driver
            .check(source_request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        options.grant_mask = (1 << 10) - 2;
        options.env_patterns = hirsel_core_net::EnvPatterns::parse("HIRSEL_FIXTURE_*");
        options.net_rules =
            vec![hirsel_core_net::GrantRule::parse("Connect 0.0.0.0/0 0-65535").unwrap()];
        let escaped = dir.path().join("directory \"é");
        std::fs::create_dir(&escaped).unwrap();
        options.directories = vec![
            dir.path().display().to_string(),
            escaped.display().to_string(),
        ];
        let built = product.build(&entry, &options).unwrap();
        run_clean(&built.executable, 7);
        // Two Dir parameters require two explicit directories, in ordinal order.
        options.directories.pop();
        let refused = product.build(&entry, &options).unwrap();
        assert_not_started(
            &Command::new(refused.executable).output().unwrap(),
            "not started: grant `other` is not in the run's configuration\n",
        );
        // A declared record crosses result and view-argument ABIs between units.
        // Its u64 field has padding before it, so reading it at byte 1 is wrong.
        options.optimize = matches!(backend, Backend::Llvm);
        let record = "type Pair { a: u8, b: u64 }\nfn make() -> Pair { Pair(a: 7u8, b: 300u64) }\nfn read(p: Pair) -> u8 { if p.b == 300u64 { p.a } else { 1u8 } }\npub fn main() -> Result[(), Exit] { let p = make(); Error(Exit(status: read(p))) }\n";
        let product = driver
            .check(source_request(record, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{}",
            serde_json::to_string(&(
                product.machine_diagnostics().unwrap(),
                product.machine_incomplete().unwrap()
            ))
            .unwrap()
        );
        let built = product.build(&entry, &options).unwrap();
        // A development build compiles one unit per reached instance.
        if !options.optimize {
            assert_eq!(built.compiled, 3);
        }
        run_clean(&built.executable, 7);
        let edited = record.replace("b: 300u64", "b: 301u64");
        let product = driver
            .check(source_request(&edited, dir.path()), &mut Meter::new(false))
            .unwrap();
        let built = product.build(&entry, &options).unwrap();
        run_clean(&built.executable, 1);
        if options.optimize {
            // Release counts the definitions left out of line after optimization.
            assert!(
                built.compiled > 0,
                "the edited record needs new release code"
            );

            // A fresh native store has to link using the shared release parts.
            let store =
                std::mem::replace(&mut options.store, dir.path().join("another-record-native"));
            let output = std::mem::replace(
                &mut options.output,
                dir.path().join("another-record-program"),
            );
            let reused = product.build(&entry, &options).unwrap();
            options.store = store;
            options.output = output;
            assert_eq!((reused.compiled, reused.linked), (0, true));
            run_clean(&reused.executable, 1);
        } else {
            assert_eq!(built.compiled, 1);
        }
        let source = "pub fn main() -> Result[(), Exit] { Ok(()) }\n";
        let product = driver
            .check(source_request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        let built = product.build(&entry, &options).unwrap();
        run_clean(&built.executable, 0);
    }
    fn capability_entry(backend: Backend, kind: &str, expected_kind: u8) {
        if expected_kind == 0 {
            crate::openat2_required::require();
        }
        let dir = tempfile::tempdir().unwrap();
        let mut options = native_options(backend, dir.path());
        let source = format!("pub fn main(cap: {kind}) -> Result[(), Exit] {{ Ok(()) }}\n");
        let product = Driver::default()
            .check(source_request(&source, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{kind}: {}",
            serde_json::to_string(&(
                product.machine_diagnostics().unwrap(),
                product.machine_incomplete().unwrap()
            ))
            .unwrap()
        );
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        if expected_kind != 10 {
            let refused = product.build(&entry, &options).unwrap();
            assert_not_started(
                &Command::new(refused.executable).output().unwrap(),
                "not started: grant `cap` is not in the run's configuration\n",
            );
        }
        // Numbers come from hirsel_core_rt_host.h, independently of grant_kind.
        match expected_kind {
            10 => {}
            0 => options.directories.push(dir.path().display().to_string()),
            _ => {
                options.grant_mask = 1 << expected_kind;
                if expected_kind == 7 {
                    options.env_patterns = hirsel_core_net::EnvPatterns::parse("HIRSEL_FIXTURE_*");
                }
                if expected_kind == 1 {
                    options.net_rules = vec![
                        hirsel_core_net::GrantRule::parse("Connect 0.0.0.0/0 0-65535").unwrap(),
                    ];
                }
            }
        }
        let built = product.build(&entry, &options).unwrap();
        run_clean(&built.executable, 0);
    }
    macro_rules! capability_entries {
        ($(($kind:ident, $expected_kind:literal, $cranelift:ident, $llvm:ident)),+ $(,)?) => {$(
            #[test]
            #[ignore = "explicit native integration target"]
            fn $cranelift() { capability_entry(Backend::Cranelift, stringify!($kind), $expected_kind); }
            #[test]
            #[ignore = "explicit native integration target"]
            fn $llvm() { capability_entry(Backend::Llvm, stringify!($kind), $expected_kind); }
        )+};
    }
    capability_entries!(
        (Args, 10, args_cranelift_entry, args_llvm_entry),
        (Dir, 0, dir_cranelift_entry, dir_llvm_entry),
        (Net, 1, net_cranelift_entry, net_llvm_entry),
        (Clock, 2, clock_cranelift_entry, clock_llvm_entry),
        (Random, 3, random_cranelift_entry, random_llvm_entry),
        (Stdout, 4, stdout_cranelift_entry, stdout_llvm_entry),
        (Stderr, 5, stderr_cranelift_entry, stderr_llvm_entry),
        (Stdin, 6, stdin_cranelift_entry, stdin_llvm_entry),
        (Env, 7, env_cranelift_entry, env_llvm_entry),
        (Process, 8, process_cranelift_entry, process_llvm_entry),
        (Tasks, 9, tasks_cranelift_entry, tasks_llvm_entry),
    );
    fn product_entry(backend: Backend, optimize: bool) {
        let dir = tempfile::tempdir().unwrap();
        let mut options = native_options(backend, dir.path());
        options.optimize = optimize;
        let source =
            "pub fn main() -> Result[(), Exit] { Error(Exit(status: identity(answer()))) }\n";
        let mut request = source_request(source, dir.path());
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("core.hirsel"))
            .unwrap()
            .write_all(b"pub fn answer() -> u8 { 7u8 }\npub fn identity[T](take x: T) -> T { x }\npub fn unused_bad() -> u64 { true }\n")
            .unwrap();
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let checked = Driver::default()
            .check(request.clone(), &mut Meter::new(false))
            .unwrap();
        let native = checked.build(&entry, &options).unwrap();
        run_clean(&native.executable, 7);
        request.store = dir.path().join("image-request");
        let cache = dir.path().join("products");
        let core = request
            .inputs
            .packages
            .iter()
            .find(|p| p.package == package("core"))
            .unwrap()
            .clone();
        drop(
            PackageProduct::sources(
                CorpusManifest {
                    core: package("core"),
                    packages: vec![core],
                },
                &cache,
                &mut Meter::new(false),
            )
            .unwrap(),
        );
        let installed = Driver::default()
            .check_reachable_with_product(
                request,
                &dir.path().join("products"),
                &mut Meter::new(false),
            )
            .unwrap();
        let native = installed.build(&entry, &options).unwrap();
        run_clean(&native.executable, 7);
        assert_eq!(installed.rechecked.len(), 1);
        // A build reads the bodies its program reaches: `answer` and
        // `identity`. An optimised LLVM build also lowers the release std of
        // an image that holds none prebuilt, which reads every definition the
        // image holds to find its plain roots, so `unused_bad` too.
        let decoded = if backend == Backend::Llvm && optimize {
            3
        } else {
            2
        };
        assert_eq!(installed.image.as_ref().unwrap().decoded_bodies(), decoded);
        // A second application's private native store: development reuses
        // core's objects and compiles its own entry; a release build links no
        // library object, and every part of the same program is already in
        // the shared parts cache, so it generates none.
        let mut other_options = native_options(backend, dir.path());
        other_options.optimize = optimize;
        other_options.store = dir.path().join("another-app-native");
        if optimize {
            let built = installed.build(&entry, &other_options).unwrap();
            assert_eq!(built.identity, native.identity);
            assert_eq!(built.compiled, 0);
            run_clean(&built.executable, 7);
        } else {
            let objects = installed.emit_reachable(&entry, &other_options).unwrap();
            let core = objects
                .iter()
                .filter(|o| o.instance.definition.package_is("core", "1"))
                .collect::<Vec<_>>();
            assert_eq!(core.len(), 2);
            assert!(core.iter().all(|o| !o.compiled));
            assert_eq!(objects.iter().filter(|o| o.compiled).count(), 1);
        }
        // A cached negative body is refused only when this entry reaches it.
        let mut request = source_request(
            "pub fn main()->Result[(),Exit] { let _=unused_bad(); Ok(()) }",
            dir.path(),
        );
        // source_request refreshes its small core fixture; restore the same package.
        std::fs::OpenOptions::new().append(true).open(dir.path().join("core.hirsel")).unwrap().write_all(b"pub fn answer() -> u8 { 7u8 }\npub fn identity[T](take x: T) -> T { x }\npub fn unused_bad() -> u64 { true }\n").unwrap();
        request.store = dir.path().join("reached-negative");
        let refused = Driver::default()
            .check_reachable_with_product(request, &cache, &mut Meter::new(false))
            .unwrap();
        assert!(
            matches!(refused.build(&entry,&options),Err(NativeStop::Check(message)) if message.contains("unused_bad"))
        );
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn published_core_entry_cranelift() {
        product_entry(Backend::Cranelift, false);
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn published_core_entry_llvm() {
        product_entry(Backend::Llvm, false);
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn release_builds_published_core_whole_program() {
        product_entry(Backend::Llvm, true);
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn prog3_prog6_cranelift_source_entries_link_run_and_reuse() {
        exercise(Backend::Cranelift)
    }
    #[test]
    #[ignore = "explicit native integration target"]
    fn prog3_prog6_llvm_source_entries_link_run_and_reuse() {
        exercise(Backend::Llvm)
    }
}
