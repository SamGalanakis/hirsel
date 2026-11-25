//! Publish the checked program and assemble its recipe with a local toolchain.

use super::recipe_grants::{GRANT_NAMES, grant_name, kind, net_rule};
use super::std_release::{StdSide, std_side};
use super::*;
use crate::code_admission::{
    AdmissionError, AdmissionLimits, NativeSemantics, VerifiedRelease, admit_local,
};
use crate::code_store::{CodeStore, ObjectKind};
use crate::foreign_archive::LinkArchives;
use hirsel_core_ir::{
    FuncId,
    address::{Binding, Group, builtin_semantics_number, foreign_address},
    decode::Debit,
    normal::{self, Annex, Slot},
    release_recipe::{
        self as wire, BodyRef, ForeignFunction, ForeignLeaf, ForeignTarget, GrantBinding,
        GrantKind, Launch, ObjectRef, Recipe,
    },
    source::{SourceMap, anchored_items},
};
use hirsel_core_resolve::{ForeignArchiveCapture, ForeignModuleCapture};
#[path = "recipe_binding.rs"]
mod binding;
pub(super) use binding::bind_annexes;
use binding::{Bindings, bind_module};

#[derive(Debug, Serialize)]
pub struct AssemblyRecord {
    pub identity: ContentHash,
    pub recipe: ContentHash,
    pub toolchain: ContentHash,
    pub digest: ContentHash,
}

impl AssemblyRecord {
    pub(crate) fn new(recipe: ContentHash, toolchain: ContentHash, bytes: &[u8]) -> Self {
        let digest = crate::native_debug::ImageDigest::Assembly.hash(bytes);
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
    module: UnitModule,
    /// The values the constant slots of `code` bind (PROG-10).
    values: crate::instances::Bindings,
    annex: Annex,
    builtins: Vec<u32>,
    /// The unit's key in its store's unit index, when its publication keeps
    /// one ([`Memo`]).
    memo: Option<ContentHash>,
    /// In a coverage build, the decisions of the unit's bodies as their source
    /// gives them, which binding their anchors to the unit's sites loses
    /// ([`hirsel_core_ir::coverage::Rebound`]).
    pub(super) coverage: Option<hirsel_core_ir::coverage::UnitDecisions>,
}

#[derive(Clone)]
enum UnitModule {
    Decoded(Box<Module>),
    /// Shares the normal body already held by the materialized instance, with
    /// the values its constant slots bind.
    Encoded(Arc<[u8]>, crate::instances::Bindings),
}

impl UnitModule {
    fn into_bound(self, annex: &Annex, identity: &Identity) -> Module {
        match self {
            Self::Decoded(module) => *module,
            Self::Encoded(bytes, values) => {
                let mut module = normal::assembly_module(&bytes);
                crate::instances::bind_values(&mut module, &values);
                let symbol = hirsel_core_ir::symbol(identity.digest());
                bind_module(&mut module, annex, Some(&symbol));
                module
            }
        }
    }
}

/// What an earlier publication into the same stores made of a unit, under
/// the unit's memo key ([`CheckProduct::memo_key`]): its stored body and
/// annex, its builtins, the sites its binding reads and the items it adds,
/// and its object's key under one emitter. A unit found here is not prepared
/// again: its module is decoded from its stored body only if its object must
/// be emitted. The index is a cache; nothing it holds enters a key or a
/// recipe that the unit's preparation would not give.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Memo {
    /// The values the stored body's constant slots bind.
    values: Vec<(DefPath, hirsel_core_ir::StaticValue)>,
    body: (ContentHash, u64),
    annex: (ContentHash, u64),
    builtins: Vec<u32>,
    /// The distinct sites of the unit's annex.
    points: Vec<(String, u32, u32)>,
    /// Each item binding added to the unit's module: its index and its site
    /// in `points`.
    anchored: Vec<(u32, u32)>,
    /// An emitter, and the key of the unit's bound object under it.
    object: (ContentHash, ContentHash),
}

impl Memo {
    fn code(&self) -> BodyRef {
        let object = |(hash, len)| ObjectRef { hash, len };
        BodyRef {
            body: object(self.body),
            annex: object(self.annex),
        }
    }
}

/// The units a publication took from its memos: each one's memo key and memo.
type Hits = BTreeMap<Identity, (ContentHash, Arc<Memo>)>;

/// A store's unit index for one entry: each unit memo by its key.
#[derive(Default, PartialEq, Serialize, Deserialize)]
struct UnitIndex {
    schema: u32,
    units: BTreeMap<ContentHash, Arc<Memo>>,
}
const UNIT_INDEX_SCHEMA: u32 = 2;

/// Where a publication's unit memos come from and go: its index's key, the
/// index as read, and the code store its bodies are in.
pub(super) struct Memos {
    key: ContentHash,
    read: UnitIndex,
    code: CodeStore,
}

impl Unit {
    pub(super) fn into_module(self) -> Module {
        let UnitModule::Decoded(module) = self.module else {
            unreachable!("release assembly prepares its modules together");
        };
        *module
    }

    pub(super) fn module(&self) -> &Module {
        let UnitModule::Decoded(module) = &self.module else {
            unreachable!("this assembly prepares its modules together");
        };
        module
    }

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
        module: UnitModule::Decoded(Box::new(module)),
        values: Arc::default(),
        annex,
        builtins,
        memo: None,
        coverage: None,
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
    /// The units an earlier publication's memo gave, apart from `units`.
    memos: Hits,
    index: Option<Memos>,
    addresses: Arc<BTreeMap<Identity, ContentHash>>,
    link_archives: LinkArchives,
    /// The entry's parameters the recipe leaves ungranted, in order (R3).
    pub(crate) ungranted: Vec<String>,
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
        let _phase = crate::meter::trace::span("recipe publication");
        reached.require()?;
        if options.recording
            && let Some(stop) = reached.recording_stop(self, entry)?
        {
            return Err(stop);
        }
        let decl = self.entry_decl(entry)?;
        let foreign = self.release_foreign(entry, reached, options, decl, store)?;
        let entry = Identity::plain(entry.clone());
        let names = decl.parameters.iter().map(|p| p.name.clone()).collect();
        let error = decl
            .error
            .as_ref()
            .map(crate::entry_show::ErrorShown::encode);
        let index = Memos::open(options, &entry, store)?;
        // Keep the canonical bytes through publication. Release std selection
        // needs the decoded program, so assemble that view after publication.
        let defer_modules = matches!(options.backend, Backend::Cranelift) || options.optimize;
        let Released {
            units,
            memos,
            groups,
            addresses,
        } = self.release_units(
            &entry,
            reached,
            store,
            Some((names, error)),
            index.as_ref(),
            defer_modules,
            options.coverage.as_ref(),
        )?;
        // The bodies in identity order before their sort, as one map of every
        // unit gives them.
        let mut coded = units
            .iter()
            .map(|(identity, unit)| (identity, unit.code))
            .chain(
                memos
                    .iter()
                    .map(|(identity, (_, memo))| (identity, memo.code())),
            )
            .collect::<Vec<_>>();
        coded.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        let mut bodies = coded.into_iter().map(|(_, code)| code).collect::<Vec<_>>();
        bodies.sort_unstable_by_key(|body| body.body.hash);
        let ungranted = super::recipe_grants::ungranted(&foreign.grants, &decl.parameters);
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
        store.commit()?;
        Ok(Publication {
            root,
            recipe,
            units,
            memos,
            index,
            addresses,
            link_archives: foreign.link_archives,
            ungranted,
        })
    }

    /// The units of `entry`'s reached closure, each body and annex put in
    /// `store`, with its groups and each unit's address; `launch` is a program
    /// entry's startup parameter names and error description. A unit
    /// other than the entry whose memo `index` holds, with its body and annex
    /// still in `store`, is taken from it and not prepared.
    pub(super) fn release_units(
        &self,
        entry: &Identity,
        reached: &Reachable<'_>,
        store: &CodeStore,
        launch: Option<(Vec<String>, Option<Vec<u8>>)>,
        index: Option<&Memos>,
        defer_modules: bool,
        coverage: Option<&hirsel_core_ir::coverage::Scope>,
    ) -> Result<Released, NativeStop> {
        let _phase = crate::meter::trace::span("release units");
        let instances = reached.require()?;
        let anchored = instances
            .iter()
            .map(|i| reached.materialized.get(i).anchored_items())
            .collect::<io::Result<Vec<_>>>()?;
        let items = anchored.iter().flatten().cloned().collect::<Vec<_>>();
        let map = self.native_sources(&items)?;
        let sources = map.positions();
        let files = index.map(|_| file_digests(&map)).transpose()?;
        let local = index.filter(|_| store.is_cache() && crate::resident::serving());
        let mut units = BTreeMap::new();
        let mut memos = BTreeMap::new();
        for (identity, anchored) in instances.iter().zip(&anchored) {
            let instance = reached.materialized.get(identity);
            let memo = match (index, &files) {
                (Some(index), Some(files)) if identity != entry => {
                    let key = memo_key(identity, instance, anchored, &map, files)?;
                    if let Some(memo) = index.read.units.get(&key)
                        && store.contains(ObjectKind::Body, memo.code().body)?
                        && store.contains(ObjectKind::Annex, memo.code().annex)?
                    {
                        memos.insert(identity.clone(), (key, Arc::clone(memo)));
                        continue;
                    }
                    Some(key)
                }
                _ => None,
            };
            // The materialised body is the unit's, before binding: it still
            // has its anchors.
            let coverage = match coverage {
                Some(scope) => Some(
                    hirsel_core_ir::coverage::unit_decisions(instance.ir()?, scope)
                        .map_err(NativeStop::Coverage)?,
                ),
                None => None,
            };
            // The materialised module is `retain`'s canonical decoding of
            // `instance.code`, so its annex is read off without encoding again.
            let (mut annex, module) = if defer_modules && identity != entry {
                (
                    instance.annex(&sources)?,
                    UnitModule::Encoded(Arc::clone(&instance.code), Arc::clone(&instance.values)),
                )
            } else {
                let (annex, module) = instance.prepare_assembly(&sources)?;
                (annex, UnitModule::Decoded(Box::new(module)))
            };
            if identity == entry
                && let Some((names, error)) = &launch
            {
                annex.1 = Some(names.clone());
                annex.2.clone_from(error);
            }
            let builtins = instance
                .slots
                .iter()
                .filter_map(|identity| builtin_semantics_number(&identity.definition))
                .collect();
            let code = BodyRef {
                body: store.put_shared(ObjectKind::Body, instance.body, &instance.code)?,
                annex: store.put(ObjectKind::Annex, &annex.encode())?,
            };
            units.insert(
                identity.clone(),
                Unit {
                    code,
                    module,
                    values: Arc::clone(&instance.values),
                    annex,
                    builtins,
                    memo,
                    coverage,
                },
            );
        }
        let invalid = || NativeStop::Entry("incomplete or inconsistent recipe closure".into());
        let (mut groups, addresses) = if let Some(index) = local {
            let remembered = crate::resident::RECIPE_GROUPS
                .get_or(Some(index.key), || Ok::<_, io::Error>(Arc::default()))?;
            let mut remembered = remembered.lock().unwrap();
            if !remembered
                .as_mut()
                .is_some_and(|groups| groups.update(instances, &reached.materialized))
            {
                *remembered = Some(
                    super::recipe_groups::Groups::new(entry, instances, &reached.materialized)
                        .ok_or_else(invalid)?,
                );
            }
            let plan = &remembered.as_ref().unwrap().plan;
            let groups = plan
                .frames()
                .map(|(hash, bytes)| store.put_addressed(ObjectKind::Group, hash, bytes))
                .collect::<io::Result<Vec<_>>>()?;
            (groups, plan.addresses())
        } else {
            let mut pending = Vec::new();
            let addresses = reached
                .materialized
                .groups(
                    instances,
                    None,
                    [entry.clone()],
                    &reached.materialized.foreign_addresses,
                    |hash, bytes| pending.push((hash, bytes)),
                )
                .ok_or_else(invalid)?;
            let groups = pending
                .iter()
                .map(|(hash, bytes)| store.put_addressed(ObjectKind::Group, *hash, bytes))
                .collect::<io::Result<Vec<_>>>()?;
            (groups, Arc::new(addresses))
        };
        groups.sort_unstable_by_key(|group| group.hash);
        units.retain(|identity, _| addresses.contains_key(identity));
        memos.retain(|identity, _| addresses.contains_key(identity));
        Ok(Released {
            units,
            memos,
            groups,
            addresses,
        })
    }
}

/// A closure's units ([`CheckProduct::release_units`]): those prepared now,
/// those an earlier publication's memo gave, its groups and each unit's
/// address.
pub(super) struct Released {
    pub(super) units: BTreeMap<Identity, Unit>,
    memos: Hits,
    groups: Vec<ObjectRef>,
    addresses: Arc<BTreeMap<Identity, ContentHash>>,
}

/// Each source file's digest: its path, its text and its annex points, all
/// a site resolved in it reads.
fn file_digests(map: &SourceMap) -> io::Result<Vec<ContentHash>> {
    map.files
        .iter()
        .map(|file| {
            key(
                "hirsel.recipe-unit-file.v1",
                &(&file.path, &file.text, map.points.get(&file.path)),
            )
        })
        .collect()
}

/// A unit's memo key: every input of its preparation. The instance's
/// identity and its definition's record fix its module, as they key its
/// instance object; its body hash fixes the normal body; and its anchored
/// items' bindings, with the digests of their files, fix every site its
/// annex resolves. The entry, whose annex also takes the startup names, is
/// never memoized.
fn memo_key(
    identity: &Identity,
    instance: &crate::instances::InstanceBody,
    anchored: &[DefPath],
    map: &SourceMap,
    files: &[ContentHash],
) -> io::Result<ContentHash> {
    let positions = anchored
        .iter()
        .map(|item| {
            let binding = map.items.get(item);
            (
                item,
                binding.map(|binding| (files[binding.file as usize], binding.file_offset)),
            )
        })
        .collect::<Vec<_>>();
    key(
        "hirsel.recipe-unit.v1",
        &(
            identity,
            instance.product.as_ref().map(|product| product.record),
            instance.body,
            &instance.signatures.values,
            instance.function.0,
            positions,
        ),
    )
}

impl Memos {
    /// The bound module of the unit `identity` whose memo is `memo`, decoded
    /// from its stored body and annex as its preparation made it. Its items
    /// are bound in the source map as its memo's (`bind_annexes`).
    fn module(&self, identity: &Identity, memo: &Memo) -> Result<Module, NativeStop> {
        use std::io::Read;
        let read = |kind: ObjectKind, (hash, len): (ContentHash, u64)| {
            let mut bytes = Vec::new();
            self.code.open(kind, hash)?.read_to_end(&mut bytes)?;
            if bytes.len() as u64 != len || kind.hash(&bytes) != hash {
                return Err(NativeStop::Link(format!(
                    "a memoized unit's stored code changed: {}",
                    identity.definition
                )));
            }
            Ok(bytes)
        };
        let refused = |error| {
            NativeStop::Link(format!(
                "a memoized unit's stored code does not decode: {}: {error:?}",
                identity.definition
            ))
        };
        let body = read(ObjectKind::Body, memo.body)?;
        let mut module = normal::decode(&body, &Debit::unlimited())
            .map_err(refused)?
            .module;
        let annex = Annex::decode(
            &read(ObjectKind::Annex, memo.annex)?,
            &module,
            &Debit::unlimited(),
        )
        .map_err(refused)?;
        crate::instances::bind_values(&mut module, &memo.values);
        let symbol = hirsel_core_ir::symbol(identity.digest());
        bind_module(&mut module, &annex, Some(&symbol));
        Ok(module)
    }

    /// The unit index of `entry`'s publications under `options`, read from `options`' store:
    /// none for an optimised LLVM build, whose units a separately lowered std takes, or a coverage
    /// build. Its key names no code store: a memo is taken only while `code` holds its body and
    /// annex, and its module is read back under its hash.
    fn open(
        options: &NativeOptions,
        entry: &Identity,
        code: &CodeStore,
    ) -> Result<Option<Self>, NativeStop> {
        // A coverage build reads each unit's decisions off its body.
        if matches!(options.backend, Backend::Llvm) && options.optimize
            || options.coverage.is_some()
        {
            return Ok(None);
        }
        let key = key(
            "hirsel.recipe-unit-index.v2",
            &(crate::reuse::compiler_identity()?, entry),
        )?;
        let read = cached(&Store::cas(&options.cas)?, key)?
            .and_then(|bytes| crate::decode::<UnitIndex>(&bytes))
            .filter(|index| index.schema == UNIT_INDEX_SCHEMA)
            .unwrap_or_default();
        Ok(Some(Self {
            key,
            read,
            code: code.clone(),
        }))
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
        entry: &DefPath,
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
        let grants = bindings(entry, decl, options, |interface| {
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
            sealed.admit(
                object,
                leaf,
                target,
                Some(&row.path),
                &Debit::unlimited(),
                || Ok(row.bytes.to_vec()),
            )?;
            selected.insert(object.hash, row);
        }
        let mut symbols = sealed.definitions(&Debit::unlimited())?;
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

fn bindings(
    entry: &DefPath,
    decl: &EntryDecl,
    options: &NativeOptions,
    mut archive: impl FnMut(&str) -> Result<ObjectRef, NativeStop>,
) -> Result<Vec<GrantBinding>, NativeStop> {
    super::recipe_grants::refuse_unmatched_grants(&entry.to_string(), &decl.parameters, options)?;
    let mut next = [0usize; GRANT_NAMES.len()];
    let mut bound = Vec::new();
    for (ordinal, param) in decl.parameters.iter().enumerate() {
        if let Some(interface) = param.foreign_interface() {
            bound.push(GrantBinding {
                ordinal: ordinal as u32,
                kind: GrantKind::Foreign,
                values: vec![interface.to_owned()],
                archive: Some(archive(interface)?),
                policy: None,
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
        let mut policy = None;
        let values = match kind {
            GrantKind::Exec => match options.exec_grants.get(index) {
                Some(grant) => {
                    policy = Some(ContentHash(grant.digest()?));
                    std::iter::once(grant.request.file.clone())
                        .chain(grant.request.names.iter().cloned())
                        .collect()
                }
                None => Vec::new(),
            },
            GrantKind::Dir => options
                .directories
                .get(index)
                .iter()
                .map(|s| (*s).clone())
                .collect(),
            GrantKind::Net if index == 0 => options
                .unix_roots
                .iter()
                .map(|(label, path)| format!("UnixRoot {label} {path}"))
                .chain(options.net_rules.iter().map(net_rule))
                .collect(),
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
                    vec![grant.source.clone(), grant.policy.clone()]
                }),
            GrantKind::Cred => options
                .cred_grants
                .iter()
                .find(|grant| grant.parameter == param.name)
                .map_or_else(Vec::new, |grant| {
                    vec![grant.source.clone(), grant.policy.clone()]
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
            policy,
        });
    }
    Ok(bound)
}

pub(crate) fn display_grants(recipe: &Recipe) -> Vec<String> {
    let mut grants = Vec::new();
    for grant in &recipe.launch.grants {
        match grant.kind {
            GrantKind::Exec => {
                if let [file, names @ ..] = grant.values.as_slice() {
                    grants.push(format!("Exec={file} {}", names.join(",")));
                }
            }
            GrantKind::Dir | GrantKind::Net | GrantKind::Env | GrantKind::Foreign => grants.extend(
                grant
                    .values
                    .iter()
                    .map(|v| format!("{}={v}", grant_name(grant.kind))),
            ),
            GrantKind::Secret if !grant.values.is_empty() => grants.push("Secret".into()),
            GrantKind::Cred => {
                if let [source, _] = grant.values.as_slice() {
                    grants.push(format!("Cred={source}"));
                }
            }
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
    error: Option<crate::entry_show::ErrorShown>,
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
    options.unix_roots.clear();
    options.secret_grants.clear();
    options.cred_grants.clear();
    options.foreign_grants.clear();
    let configured_exec = std::mem::take(&mut options.exec_grants);
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
            GrantKind::Exec => match values.as_slice() {
                [] => {}
                [file, names @ ..] if !names.is_empty() && options.exec_grants.len() == index => {
                    options.exec_grants.push(crate::exec_launch::Binding {
                        request: hirsel_core_net::exec::Request::parse(&format!(
                            "{file} {}",
                            names.join(",")
                        ))
                        .map_err(|_| invalid())?,
                        policy: Some(binding.policy.ok_or_else(invalid)?.0),
                        workdir: configured_exec
                            .get(index)
                            .filter(|configured| {
                                configured.request.file == *file
                                    && configured.request.names == names
                                    && configured.policy == binding.policy.map(|policy| policy.0)
                            })
                            .and_then(|configured| configured.workdir.clone()),
                    });
                }
                _ => return Err(invalid()),
            },
            GrantKind::Dir => match values.as_slice() {
                [] => {}
                [path] if options.directories.len() == index => {
                    options.directories.push(path.clone())
                }
                _ => return Err(invalid()),
            },
            GrantKind::Net if index == 0 && !values.is_empty() => {
                let grant = hirsel_core_net::NetGrant::parse(values).map_err(NativeStop::Entry)?;
                options.net_rules = grant.rules;
                options.unix_roots = grant.roots;
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
            GrantKind::Cred => match values.as_slice() {
                [] => {}
                [source, policy] => options.cred_grants.push(
                    hirsel_core_net::CredGrant::new(name, source, policy).map_err(|_| invalid())?,
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
            error,
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
    if file.len() != expected.len {
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
        for (reference, group) in recipe.groups.iter().zip(groups) {
            for (i, member) in group.members.into_iter().enumerate() {
                let (code, body, annex) = bodies.remove(&member.body).expect("admitted body");
                let identity = body
                    .identity(FuncId(0), &Debit::unlimited())
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
                                .identity(*function, &Debit::unlimited())
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
                        module: UnitModule::Decoded(Box::new(body.module)),
                        values: Arc::default(),
                        annex,
                        builtins,
                        memo: None,
                        coverage: None,
                    },
                );
            }
        }
        Self {
            root,
            recipe,
            units,
            memos: BTreeMap::new(),
            index: None,
            addresses: Arc::new(addresses),
            link_archives,
            ungranted: Vec::new(),
        }
    }
}

/// The object of `instance`'s module under `key`: stored in one of
/// `store`'s layers (an installed image's prebuilt objects under the user's
/// store, `Store::over`), else emitted now from `module` and published in
/// `store`'s own layer.
fn object<'m>(
    store: &Store,
    options: &NativeOptions,
    key: ContentHash,
    instance: &Identity,
    module: impl FnOnce() -> Result<Cow<'m, Module>, NativeStop>,
) -> Result<ObjectProduct, NativeStop> {
    let found = crate::store::remote::load_shared(store, options, key)?;
    let (held, debug, compiled) = match found {
        Some((bare, debug, bytes)) => ((bare, bytes), debug, false),
        None => {
            let module = module()?;
            let object = emit_unit(&module, || module.link_symbols(), options, key, instance)
                .map_err(|message| NativeStop::Emit {
                    definition: instance.definition.clone(),
                    message,
                })?;
            let bare = crate::store::remote::publish(store, options, key, &object)?;
            let debug = object.debug.map(Arc::new);
            ((bare, object.bytes.into()), debug, true)
        }
    };
    Ok(ObjectProduct::shared(
        (instance.clone(), key),
        store,
        held,
        debug,
        compiled,
    ))
}

/// Where a library image's store holds the objects an installation
/// prebuilt, under the keys a program's build computes.
pub(crate) const LIBRARY_OBJECTS: &str = "native";

/// The identity of the emitter a recipe's objects are made by under
/// `options`, linking the runtime whose identity is `runtime`.
fn emitter(
    options: &NativeOptions,
    runtime: ContentHash,
    target: &hirsel_core_layout::Target,
) -> io::Result<ContentHash> {
    let emission = environment(options, runtime)?;
    key(
        "hirsel.recipe-emitter.v1",
        &(
            &emission,
            target.cpu(),
            target.llvm_features(),
            options.optimize
                && matches!(options.backend, Backend::Llvm)
                && std::env::var_os("HIRSEL_LLVM_PRINTED_ORACLE").is_some(),
        ),
    )
}

/// The key of a bound unit's object: every input of its emission. An
/// installation's prebuild and a program's assembly both key by it.
fn unit_object_key(
    emitter: ContentHash,
    identity: &Identity,
    module: &Module,
    builtins: &[u32],
) -> io::Result<ContentHash> {
    let object = (emitter, identity, module, builtins);
    match &module.coverage_rebound {
        None => key("hirsel.recipe-native-object.v2", &object),
        // An edit can move a key and keep a body and its annex.
        Some(rebound) => key("hirsel.recipe-native-object.v2", &(object, rebound)),
    }
}

impl CheckProduct {
    /// Emit the recipe object of every plain instance in `instances` (each
    /// materialized in `materialized`) under `options` into `store`, as a
    /// development build of a program reaching it emits it. A store only
    /// adds what it lacks; the number of objects emitted now.
    pub(crate) fn prebuild_recipe_objects(
        &self,
        instances: &[Identity],
        materialized: &crate::instances::InstanceSet,
        options: &NativeOptions,
        store: &Store,
    ) -> Result<usize, NativeStop> {
        let items = instances
            .iter()
            .map(|i| materialized.get(i).ir().map(anchored_items))
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let sources = self.native_sources(&items)?;
        let positions = sources.positions();
        let mut units = BTreeMap::new();
        for identity in instances {
            let instance = materialized.get(identity);
            units.insert(
                identity.clone(),
                prepare_unit(instance.ir()?, instance.function, &positions),
            );
        }
        bind_annexes(&mut units, &BTreeMap::new(), &BTreeMap::new(), true, None)?;
        let target = hirsel_core_layout::Target::from_triple(&options.target)
            .filter(|_| options.backend.supports_target(&options.target))
            .ok_or_else(|| {
                NativeStop::Entry(format!("unsupported prebuild target {}", options.target))
            })?;
        let (tools, _) =
            crate::tool_inputs::toolchain(&options.link, &options.target, &options.runtime)?;
        let emitter = emitter(options, tools.runtime, &target)?;
        let mut compiled = 0;
        for (identity, unit) in &units {
            let key = unit_object_key(emitter, identity, unit.module(), &unit.builtins)?;
            let object = object(store, options, key, identity, || {
                Ok(Cow::Borrowed(unit.module()))
            })?;
            compiled += usize::from(object.compiled);
        }
        Ok(compiled)
    }
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
    /// bodies and calls each by its exported symbol. That std, and any other
    /// build's prebuilt objects, are read first from `library` (a library
    /// image's store), else from `product`'s image.
    pub(crate) fn assemble_with(
        mut self,
        options: &NativeOptions,
        counters: crate::reach::ReachCounters,
        product: Option<&CheckProduct>,
        library: Option<&Path>,
    ) -> Result<BuildProduct, NativeStop> {
        let library = library.map(Path::to_owned).or_else(|| {
            product
                .and_then(|product| product.image.as_ref())
                .map(|image| image.store_root().to_owned())
        });
        let std = self.std_side(options, library.as_deref())?;
        self.assemble_over(options, counters, std, library.as_deref())
    }

    /// The separately lowered release std an optimised LLVM build of this
    /// publication under `options` takes ([`std_side`]), prebuilt in
    /// `library` (a library image's store) or not; None for any other.
    pub(crate) fn std_side(
        &mut self,
        options: &NativeOptions,
        library: Option<&Path>,
    ) -> Result<Option<StdSide>, NativeStop> {
        let (options, _) = self.launch(options)?;
        if !matches!(options.backend, Backend::Llvm) || !options.optimize {
            return Ok(None);
        }
        for unit in self.units.values_mut() {
            if let UnitModule::Encoded(bytes, values) = &unit.module {
                let mut module = normal::assembly_module(bytes);
                crate::instances::bind_values(&mut module, values);
                unit.module = UnitModule::Decoded(Box::new(module));
            }
        }
        std_side(&self.units, &self.recipe.entry, library, &options).map(Some)
    }

    /// The options the recipe's launch fixes over `options`, and its entry's
    /// declaration.
    fn launch(&self, options: &NativeOptions) -> Result<(NativeOptions, EntryDecl), NativeStop> {
        let annex = &self.units[&self.recipe.entry].annex;
        let names = annex.1.as_deref().ok_or_else(|| {
            NativeStop::Entry("recipe entry has no startup parameter names".into())
        })?;
        let error = annex
            .2
            .as_deref()
            .map(crate::entry_show::ErrorShown::decode)
            .transpose()
            .map_err(|e| NativeStop::Entry(e.into()))?;
        launch_options(&self.recipe, names, error, options)
    }

    /// Assemble the program; an optimised LLVM build over `std`, the
    /// separately lowered release std ([`Self::std_side`]), which every such
    /// build takes and no other does: the program holds none of its exports'
    /// bodies and calls each by its exported symbol. The side comes from
    /// `library` (a library image's store) when its std product prebuilt it.
    pub(crate) fn assemble_over(
        mut self,
        options: &NativeOptions,
        counters: crate::reach::ReachCounters,
        std: Option<StdSide>,
        library: Option<&Path>,
    ) -> Result<BuildProduct, NativeStop> {
        let _phase = crate::meter::trace::span("native assembly");
        let mut counters = counters;
        counters.std_lowered = std.as_ref().map_or(0, |std| std.lowered);
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
        let sources_digest = ContentHash::of_parts(
            "hirsel.recipe-source-bindings.v1",
            std.iter().map(|std| &std.key.0[..]).chain(
                self.recipe
                    .bodies
                    .iter()
                    .flat_map(|body| [&body.body.hash.0[..], &body.annex.hash.0[..]]),
            ),
        );
        let no_points = BTreeMap::new();
        let Bindings {
            sources,
            key: sources_key,
            rebased,
            mut bound,
        } = bind_annexes(
            &mut self.units,
            &self.memos,
            std.as_ref().map_or(&no_points, |std| &std.points),
            true,
            Some(std.as_ref().map(|std| std.key)),
        )?;
        counters.source_map_rebased = u64::from(rebased);
        let root = &self.units[entry];
        let (abi, cleanup) = bound_entry_abi(&decl, &entry.definition, root.module(), FuncId(0))
            .map_err(NativeStop::Entry)?;
        let root_body = root.code.body.hash;
        // Native bodies and lent std code can operate on the same live Map.
        let emitter = emitter(&options, tools.runtime, &target)?;
        use std::os::unix::ffi::OsStrExt;
        let fingerprint = key(
            "hirsel.assembly-environment.v2",
            &(
                std.as_ref().map(|std| std.key),
                // The bodies the program imports shape its code.
                std.as_ref().and_then(|std| std.lent.key()),
                emitter,
                tools.image,
                tools.runtime,
                tools.environment,
                link.identity,
                options.debug_lines,
                options.recording,
                &options.replay,
                options
                    .logical_charge_report
                    .as_ref()
                    .map(|path| path.as_os_str().as_bytes()),
            ),
        )?;
        let store = Store::cas(&options.cas)?;
        let layered = library.map_or_else(|| store.clone(), |root| store.clone().over(root));
        let mut release_sites = Vec::new();
        let mut release_debug_sites = Vec::new();
        let mut remarks = Vec::new();
        let (mut objects, closed_image, launch_digest_unused) = if let Some(std) = std {
            drop(bound);
            let emitted = release::release_objects(
                self.units,
                entry,
                std,
                &options,
                tools.runtime,
                &store,
                link.uses_hirsel_launcher(&options.link),
            )?;
            release_sites = emitted.release_sites;
            release_debug_sites = emitted.release_debug_sites;
            remarks = emitted.remarks;
            (
                emitted.objects,
                emitted.closed_image,
                emitted.launch_digest_unused,
            )
        } else {
            // Every unit in identity order, as one map of them gives it.
            let mut all = self
                .units
                .into_iter()
                .map(|(identity, unit)| (identity, Ok(unit)))
                .chain(
                    self.memos
                        .into_iter()
                        .map(|(identity, hit)| (identity, Err(hit))),
                )
                .collect::<Vec<_>>();
            all.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
            let mut objects = Vec::with_capacity(all.len());
            let mut kept = BTreeMap::new();
            for (identity, unit) in all {
                match unit {
                    Ok(Unit {
                        module,
                        values,
                        annex,
                        code,
                        builtins,
                        memo,
                        coverage,
                    }) => {
                        let mut module = module.into_bound(&annex, &identity);
                        if let Some(decisions) = coverage {
                            let symbol = hirsel_core_ir::symbol(identity.digest());
                            module.coverage_rebound = Some(Arc::new([(symbol, decisions)].into()));
                            // A body whose blocks are not its source's is
                            // refused here, before any engine flags it.
                            hirsel_core_ir::coverage::fragment(&module)
                                .map_err(NativeStop::Coverage)?;
                        }
                        drop(annex);
                        let object_key = unit_object_key(emitter, &identity, &module, &builtins)?;
                        objects.push(object(&layered, &options, object_key, &identity, || {
                            Ok(Cow::Owned(module))
                        })?);
                        if let (Some(memo), Some(bound)) = (memo, bound.remove(&identity)) {
                            let object = |object: ObjectRef| (object.hash, object.len);
                            kept.insert(
                                memo,
                                Arc::new(Memo {
                                    values: values.to_vec(),
                                    body: object(code.body),
                                    annex: object(code.annex),
                                    builtins,
                                    points: bound.points,
                                    anchored: bound.anchored,
                                    object: (emitter, object_key),
                                }),
                            );
                        }
                    }
                    Err((memo_key, memo)) => {
                        let index = self
                            .index
                            .as_ref()
                            .expect("a memo's publication keeps an index");
                        let (object_key, decoded) = if memo.object.0 == emitter {
                            (memo.object.1, None)
                        } else {
                            let module = index.module(&identity, &memo)?;
                            let key = unit_object_key(emitter, &identity, &module, &memo.builtins)?;
                            (key, Some(module))
                        };
                        objects.push(object(&layered, &options, object_key, &identity, || {
                            match decoded {
                                Some(module) => Ok(Cow::Owned(module)),
                                None => index.module(&identity, &memo).map(Cow::Owned),
                            }
                        })?);
                        let memo = if memo.object.0 == emitter {
                            memo
                        } else {
                            Arc::new(Memo {
                                object: (emitter, object_key),
                                ..(*memo).clone()
                            })
                        };
                        kept.insert(memo_key, memo);
                    }
                }
            }
            if let Some(index) = &self.index
                && kept != index.read.units
            {
                let written = UnitIndex {
                    schema: UNIT_INDEX_SCHEMA,
                    units: kept,
                };
                publish(&store, index.key, &crate::encode(&written)?)?;
            }
            (objects, false, false)
        };
        let cleanup = if let Some(module) = cleanup {
            let identity = cleanup_identity(&module);
            let cleanup_key = key("hirsel.recipe-entry-cleanup.v1", &(emitter, root_body))?;
            objects.push(object(&store, &options, cleanup_key, &identity, || {
                Ok(Cow::Borrowed(&module))
            })?);
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
        .with_place(crate::native_entry::reaches_place(
            self.addresses.keys().map(|i| &i.definition),
        ))
        .with_release_sites(&release_sites, &release_debug_sites, &remarks)
        .with_foreign(&self.recipe.foreign_modules);
        let result = link_objects(
            &plan,
            &objects,
            &self.link_archives,
            None,
            &options,
            sources_digest,
            || Ok((sources, sources_key)),
            tools.image,
            link,
            closed_image,
            counters,
            |_| Ok(0),
            self.recipe.address,
            || {
                Ok(Arc::unwrap_or_clone(self.addresses)
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
        let UnitModule::Decoded(module) = &mut unit.module else {
            unreachable!("the entry is prepared during publication");
        };
        let builtin = module.functions[0]
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
        for instruction in module.functions[0].body.as_mut().unwrap().insts.iter_mut() {
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
        let mut group = Group::decode(&frame, &Debit::unlimited()).unwrap();
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
        Arc::make_mut(&mut self.addresses).insert(self.recipe.entry.clone(), self.recipe.address);
        self.root = store
            .put(ObjectKind::Recipe, &self.recipe.encode())
            .unwrap()
            .hash;
    }
}
