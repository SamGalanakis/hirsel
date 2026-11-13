//! Immutable launch assembly over actual settled Lower and source observations.
//! Source descriptions and native descriptors are data; native import/C/Ready
//! validation remains independent. No constructor queries a mutable engine.
use super::{NativeError, ProfileSources, ReadyProgram, StageName, adapt};
use hirsel_engine::{
    LinkInput, NativeDefinitionProduct, NativeOriginalReader, NativeRecordSource,
    NativeSemanticProgram, NativeSourceRoots, RepresentationRecord, SignatureSnapshot,
    ValidatedNativeInput,
};
use hirsel_lir::stage::InputIdentity;
use hirsel_span::{Anchor, SourceMap};
use hirsel_syntax::{DefId, DefPath};
use hirsel_types::{ForeignThreads, SignatureKind, SignatureRef, TypeInterner};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

/// Source-query results supplied by the front/native producers. Entry body IDs
/// and canonical source anchors are explicit settled declaration metadata; no
/// original or derived key is interpreted as body zero.
pub struct NativeLinkFacts<'a> {
    pub program: &'a hirsel_rc::instance::Program,
    pub operations: Arc<hirsel_layout::operation::OperationRegistry>,
    pub roots: &'a NativeSourceRoots,
    pub declarations: &'a BTreeMap<DefPath, Arc<NativeDefinitionProduct>>,
    pub entries: &'a BTreeMap<DefPath, u32>,
    pub anchors: &'a BTreeMap<DefPath, Vec<Anchor>>,
    pub signatures: Arc<SignatureSnapshot>,
    pub semantic: Arc<NativeSemanticProgram>,
    pub selected: &'a [DefPath],
}

/// Bodyless source facets for one immutable native request. The semantic
/// closure is supplied by S0, independently of native discovered instances.
pub struct NativeLinkSource {
    input: InputIdentity,
    semantic: Arc<NativeSemanticProgram>,
    types: Arc<TypeInterner>,
    operations: Arc<hirsel_layout::operation::OperationRegistry>,
    functions: BTreeMap<DefPath, u32>,
    signatures: BTreeMap<DefPath, SignatureRef>,
    representations: BTreeMap<DefPath, Option<RepresentationRecord>>,
    anchors: BTreeMap<DefPath, Vec<Anchor>>,
    sources: SourceMap,
    profiles: Arc<ProfileSources>,
    foreign_links: Vec<LinkInput>,
    foreign_unreplayable: bool,
    foreign_threads: BTreeMap<hirsel_syntax::PackageId, ForeignThreads>,
    requested_run: Option<(DefPath, Arc<hirsel_run_config::ValidatedRun>)>,
    test_entries: Vec<(DefPath, u64)>,
    metadata_owner: Mutex<Option<Arc<hirsel_engine_admission::MemoryReservation>>>,
}

// Bulk collection fills the immutable B-tree nodes. These private maps have
// no insertions or removals after construction, as required by bulk_tree().
fn packed<K: Ord, V>(map: BTreeMap<K, V>) -> BTreeMap<K, V> {
    map.into_iter().collect()
}
fn bulk_map<K, V>(
    weight: &mut hirsel_query_products::retained::MetadataWeight,
    map: &BTreeMap<K, V>,
    mut visit: impl FnMut(&mut hirsel_query_products::retained::MetadataWeight, &K, &V),
) {
    weight.bulk_tree::<K, V>(map.len());
    for (key, value) in map {
        visit(weight, key, value);
    }
}

fn failed(message: impl Into<String>) -> NativeError {
    NativeError::new(StageName::Manifest, message)
}

fn retain_source_metadata(
    owner: &Mutex<Option<Arc<hirsel_engine_admission::MemoryReservation>>>,
    bytes: u64,
    allowance: &mut hirsel_engine_admission::MemoryReservation,
) -> Result<(), String> {
    let mut retained = owner.lock().unwrap_or_else(|e| e.into_inner());
    if retained.is_none() {
        *retained =
            Some(Arc::new(allowance.split(bytes).ok_or(
                "native link source metadata exceeds retained allowance",
            )?));
    }
    Ok(())
}

impl NativeLinkSource {
    pub fn from_settled(
        facts: NativeLinkFacts<'_>,
        lower: &NativeOriginalReader,
    ) -> Result<Self, NativeError> {
        let program = facts.program;
        let context = lower.record_context();
        if !program.definitions.is_empty()
            || !Arc::ptr_eq(&program.types, facts.signatures.types())
            || !Arc::ptr_eq(&program.types, context.record_types())
            || !Arc::ptr_eq(&program.types, &facts.operations.types)
            || !Arc::ptr_eq(&facts.signatures, lower.signatures())
            || !Arc::ptr_eq(&facts.signatures, facts.semantic.signatures())
            || facts.roots.foreign_links != facts.semantic.foreign_links()
            || facts.roots.foreign_unreplayable != facts.semantic.foreign_unreplayable()
            || &facts.roots.foreign_threads != facts.semantic.foreign_threads()
        {
            return Err(failed(
                "native launch source differs from its immutable source context",
            ));
        }
        for (path, implementation, signature) in facts.semantic.source_bindings() {
            let source = facts
                .declarations
                .get(path)
                .ok_or_else(|| failed(format!("missing semantic source declaration {path}")))?;
            if source.source.implementation_revision != implementation
                || source.signature.fingerprint != signature
            {
                return Err(failed(format!(
                    "semantic source observation differs for {path}"
                )));
            }
        }
        for (path, source) in facts.declarations {
            let signature = program
                .signatures
                .get(path)
                .ok_or_else(|| failed(format!("missing native source signature {path}")))?;
            let checked = facts
                .signatures
                .signature(DefId::intern(path))
                .and_then(Result::ok)
                .ok_or_else(|| failed(format!("missing S0 signature {path}")))?;
            if source.definition != *path
                || source.signature.definition.path() != path
                || signature.fingerprint != source.signature.fingerprint
                || checked.fingerprint != signature.fingerprint
            {
                return Err(failed(format!(
                    "source declaration signature differs for {path}"
                )));
            }
            let anchors = facts
                .anchors
                .get(path)
                .ok_or_else(|| failed(format!("missing settled source anchors {path}")))?;
            if anchors.is_empty() || anchors.iter().collect::<BTreeSet<_>>().len() != anchors.len()
            {
                return Err(failed(format!(
                    "missing or duplicate source anchor for {path}"
                )));
            }
            // A fact helper's code is its owner's `where` predicate, so the
            // anchors it cites belong to the owner's declaration.
            let declaration =
                hirsel_check_protocol::fact::fact_owner(path).unwrap_or_else(|| path.clone());
            for anchor in anchors {
                let owner = context.record_owner(*anchor);
                let canonical = owner.as_ref().and_then(|(owner, occurrence)| {
                    context.record_source_anchor(owner, *occurrence)
                });
                // Raw source-map aliases can share the same content-owned
                // identity and placed origin. Require the full codec identity
                // to round-trip through its selected canonical alias.
                if context.source_definition(*anchor) != Some(&declaration)
                    || canonical.is_none_or(|canonical| {
                        context.record_owner(canonical) != owner
                            || context.source_definition(canonical) != Some(&declaration)
                            || facts.roots.sources.anchor(canonical)
                                != facts.roots.sources.anchor(*anchor)
                    })
                    || facts.roots.sources.anchor(*anchor).is_none()
                {
                    return Err(failed(format!("settled source anchor differs for {path}")));
                }
            }
        }
        let selected: BTreeSet<_> = facts.selected.iter().collect();
        if selected.is_empty() || selected.len() != facts.selected.len() {
            return Err(failed(
                "native launch has no selected source or repeats a selection",
            ));
        }
        for path in selected {
            let source = facts
                .declarations
                .get(path)
                .ok_or_else(|| failed(format!("missing selected source {path}")))?;
            let (verified, _, implementation, signature) = lower
                .input(path)
                .ok_or_else(|| failed(format!("selected source has no settled Lower {path}")))?;
            // Recompute readers retain the original Lower but do not accept
            // its producer's reusable V output. Compare only the evidence
            // selected by this reader; original reads still run V themselves.
            let source_output = source.lower.as_ref().filter(|_| {
                verified.is_some()
                    || lower.verification() != hirsel_engine::Verify::On
                    || lower.query_policy().reuse
            });
            if implementation != source.source.implementation_revision
                || signature != source.signature.fingerprint
                || !facts.semantic.semantic_contains(path)
                || verified.zip(source_output).is_some_and(|(a, b)| a != b)
                || verified.is_some() != source_output.is_some()
            {
                return Err(failed(format!("selected Lower source differs for {path}")));
            }
        }
        for (path, body) in facts.entries {
            let signature = program
                .signatures
                .get(path)
                .ok_or_else(|| failed(format!("missing entry declaration {path}")))?;
            if !matches!(&signature.kind, SignatureKind::Function(function) if function.generics.is_empty())
            {
                return Err(failed(format!(
                    "entry is not a monomorphic source function {path}"
                )));
            }
            if let Some((Some(output), _, _, _)) = lower.input(path)
                && (*body >= output.invocations().body_count()
                    || *body != output.invocations().entry_body())
            {
                return Err(failed(format!(
                    "entry body differs from V's settled Lower entry {path}"
                )));
            }
        }
        let profiles = super::profile_sources_for_owners(
            &facts.roots.sources,
            facts
                .declarations
                .iter()
                .map(|(path, source)| (path, &source.source, facts.anchors[path].clone())),
        );
        if profiles.len() != facts.declarations.len() {
            return Err(failed(
                "native launch source has an unavailable primary profile anchor",
            ));
        }
        Ok(Self {
            input: program.input.clone(),
            semantic: facts.semantic,
            types: program.types.clone(),
            operations: facts.operations,
            functions: packed(facts.entries.clone()),
            signatures: packed(program.signatures.clone()),
            representations: facts
                .declarations
                .iter()
                .map(|(path, source)| (path.clone(), source.representation.clone()))
                .collect(),
            anchors: packed(facts.anchors.clone()),
            sources: facts.roots.sources.clone(),
            profiles: Arc::new(packed(profiles)),
            foreign_links: facts.roots.foreign_links.clone(),
            foreign_unreplayable: facts.roots.foreign_unreplayable,
            foreign_threads: packed(facts.roots.foreign_threads.clone()),
            requested_run: None,
            test_entries: Vec::new(),
            metadata_owner: Mutex::new(None),
        })
    }
    /// Allocation-layout bound for this copied body-free source metadata.
    /// Shared semantic/signature/source text and interner payloads retain their
    /// original source owners; this measures only newly allocated containers.
    pub fn retained_metadata_bytes(&self) -> u64 {
        use hirsel_query_products::retained::MetadataWeight as Weight;
        let mut w = Weight::default();
        let Self {
            input: _,
            semantic: _,
            types: _,
            operations: _,
            functions,
            signatures,
            representations,
            anchors,
            sources: _,
            profiles,
            foreign_links,
            foreign_unreplayable: _,
            foreign_threads,
            requested_run,
            test_entries,
            metadata_owner: _,
        } = self;
        w.add(size_of::<Self>() + 2 * size_of::<usize>());
        w.add(size_of::<hirsel_engine_admission::MemoryReservation>() + 2 * size_of::<usize>());
        bulk_map(&mut w, functions, |w, path, _| w.path(path));
        bulk_map(&mut w, signatures, |w, path, _| w.path(path));
        bulk_map(&mut w, representations, |w, path, value| {
            w.path(path);
            if let Some(value) = value {
                w.representation(value);
            }
        });
        bulk_map(&mut w, anchors, |w, path, values| {
            w.path(path);
            w.flat_vec(values);
        });
        w.arc(profiles);
        bulk_map(&mut w, profiles, |w, path, profile| {
            w.path(path);
            w.add(profile.path.capacity());
            w.add(profile.file.retained_metadata_bytes() as usize);
            w.vec(&profile.anchors, |w, (file, _)| {
                w.add(file.retained_metadata_bytes() as usize)
            });
        });
        w.vec(foreign_links, |w, link| match link {
            LinkInput::Library(name) => w.add(name.capacity()),
            LinkInput::File(path) => w.add(path.capacity()),
        });
        bulk_map(&mut w, foreign_threads, |w, package, _| {
            w.add(package.name.capacity());
            w.add(package.version.capacity());
        });
        if let Some((path, _)) = requested_run {
            w.path(path);
        }
        w.vec(test_entries, |w, (path, _)| w.path(path));
        w.bytes()
    }
    pub fn retain_metadata(
        &self,
        allowance: &mut hirsel_engine_admission::MemoryReservation,
    ) -> Result<(), String> {
        retain_source_metadata(
            &self.metadata_owner,
            self.retained_metadata_bytes(),
            allowance,
        )
    }
    pub fn metadata_allowance(&self) -> Option<Arc<hirsel_engine_admission::MemoryReservation>> {
        self.metadata_owner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Exact settled foreign policy used by the ordinary native emitter.
    pub fn foreign_dispatch(&self) -> BTreeMap<DefPath, u64> {
        super::foreign::dispatch(&self.view())
    }
    pub fn input(&self) -> &InputIdentity {
        &self.input
    }
    pub fn semantic(&self) -> &NativeSemanticProgram {
        &self.semantic
    }
    pub fn operations(&self) -> &hirsel_layout::operation::OperationRegistry {
        &self.operations
    }
    pub fn functions(&self) -> &BTreeMap<DefPath, u32> {
        &self.functions
    }
    pub fn profiles(&self) -> Arc<ProfileSources> {
        self.profiles.clone()
    }
    pub fn requested_run(&self) -> Option<(DefPath, Arc<hirsel_run_config::ValidatedRun>)> {
        self.requested_run.clone()
    }
    pub fn test_entries(&self) -> &[(DefPath, u64)] {
        &self.test_entries
    }
    /// Exact settled source anchors, including generated-source ordinals.
    pub fn source_anchors(&self) -> &BTreeMap<DefPath, Vec<Anchor>> {
        &self.anchors
    }
    pub fn sites(&self) -> super::Sites<'_> {
        super::Sites::from_anchors(
            &self.sources,
            self.anchors.iter().map(|(p, a)| (p, a.clone())).collect(),
        )
    }
    pub fn launches(&self) -> BTreeMap<DefPath, super::link::LaunchKind> {
        let mut launches: BTreeMap<_, _> = self
            .signatures
            .iter()
            .filter_map(|(path, signature)| {
                super::link::launch_kind(&self.types, signature).map(|kind| (path.clone(), kind))
            })
            .collect();
        launches
    }
    pub fn bind_run(
        &mut self,
        entry: &DefPath,
        run: Arc<hirsel_run_config::ValidatedRun>,
    ) -> Result<(), NativeError> {
        if self
            .metadata_owner
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return Err(failed("native launch metadata is already admitted"));
        }
        if !self.functions.contains_key(entry) {
            return Err(failed("native run has no selected source entry"));
        }
        let revision = self
            .semantic
            .semantic_program_revision(entry)
            .map_err(|e| failed(format!("native run source revision: {e:?}")))?;
        let kind = match self.launches().get(entry) {
            Some(super::link::LaunchKind::Test) => hirsel_run_schema::LaunchKind::Test,
            Some(super::link::LaunchKind::Zero | super::link::LaunchKind::Main(_)) => {
                hirsel_run_schema::LaunchKind::Main
            }
            None => return Err(failed("native run has no launchable source entry")),
        };
        run.check_revision(&revision, kind)
            .map_err(|e| failed(format!("native run source binding: {e}")))?;
        self.requested_run = Some((entry.clone(), run));
        Ok(())
    }
    pub fn bind_tests(&mut self, entries: &[(DefPath, u64)]) -> Result<(), NativeError> {
        if self
            .metadata_owner
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return Err(failed("native launch metadata is already admitted"));
        }
        let launches = self.launches();
        if entries.is_empty()
            || entries.iter().any(|(entry, _)| {
                !self.functions.contains_key(entry)
                    || launches.get(entry) != Some(&super::link::LaunchKind::Test)
            })
        {
            return Err(failed(
                "native test selection has no entries or a non-Test entry",
            ));
        }
        self.test_entries = entries.to_vec();
        Ok(())
    }
    pub fn bind(&self, program: &mut ReadyProgram) -> Result<(), NativeError> {
        program.bind_source(&self.view())?;
        Ok(())
    }
    /// Complete source inputs consumed by launch assembly. Logical paths,
    /// primary/generated source fields and ordered anchors are observable;
    /// physical checkout relocation is not an emitted source input.
    pub fn manifest_bytes(&self) -> Result<Vec<u8>, String> {
        let file = |file: &hirsel_span::SourceFile| {
            (
                file.logical_path().to_path_buf(),
                hirsel_syntax::ContentHash::of_bytes(file.text().as_bytes()),
            )
        };
        let profiles = self
            .profiles
            .iter()
            .map(|(path, source)| {
                (
                    path,
                    file(&source.file),
                    source.base,
                    source.generated,
                    source
                        .anchors
                        .iter()
                        .map(|(owner, base)| (file(owner), base))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        let revisions = self
            .launches()
            .into_keys()
            .map(|path| {
                self.semantic
                    .semantic_program_revision(&path)
                    .map(|revision| (path, revision.digest()))
                    .map_err(|e| format!("semantic link revision: {e:?}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let declarations = self
            .signatures
            .iter()
            .map(|(path, signature)| {
                (
                    path,
                    signature.fingerprint,
                    self.representations
                        .get(path)
                        .and_then(|r| r.as_ref())
                        .map(|r| r.representation_revision),
                )
            })
            .collect::<Vec<_>>();
        let links = self
            .foreign_links
            .iter()
            .map(|input| match input {
                LinkInput::Library(name) => (0u32, name.as_bytes().to_vec()),
                LinkInput::File(path) => (1, path.as_os_str().as_encoded_bytes().to_vec()),
            })
            .collect::<Vec<_>>();
        let dispatch = super::foreign::dispatch(&self.view());
        serde_json::to_vec(&(
            1u32,
            self.input.digest,
            declarations,
            revisions,
            profiles,
            links,
            self.foreign_unreplayable,
            dispatch.iter().collect::<Vec<_>>(),
            self.functions.iter().collect::<Vec<_>>(),
            self.requested_run
                .as_ref()
                .map(|(entry, run)| (entry, run.launch_bytes())),
            &self.test_entries,
        ))
        .map_err(|e| e.to_string())
    }
    pub fn view(&self) -> LinkSource<'_> {
        LinkSource {
            input: self.input.clone(),
            semantic: &self.semantic,
            types: &self.types,
            signatures: &self.signatures,
            representations: &self.representations,
            sources: &self.sources,
            anchors: self.anchors.iter().map(|(p, a)| (p, a.clone())).collect(),
            foreign_links: &self.foreign_links,
            foreign_unreplayable: self.foreign_unreplayable,
            foreign_threads: &self.foreign_threads,
        }
    }
}

/// Exact immutable facets consumed by the existing launch algorithms. The
/// inspection API and task assembly share these same source algorithms.
pub struct LinkSource<'a> {
    pub input: InputIdentity,
    pub semantic: &'a NativeSemanticProgram,
    pub types: &'a TypeInterner,
    pub signatures: &'a BTreeMap<DefPath, SignatureRef>,
    pub representations: &'a BTreeMap<DefPath, Option<RepresentationRecord>>,
    pub sources: &'a SourceMap,
    pub anchors: BTreeMap<&'a DefPath, Vec<Anchor>>,
    pub foreign_links: &'a Vec<LinkInput>,
    pub foreign_unreplayable: bool,
    pub foreign_threads: &'a BTreeMap<hirsel_syntax::PackageId, ForeignThreads>,
}
impl<'a> LinkSource<'a> {
    pub fn snapshot(input: &'a ValidatedNativeInput) -> Self {
        let snapshot = input.snapshot();
        Self {
            input: adapt::input_identity(snapshot),
            semantic: input.semantic_program(),
            types: &snapshot.types,
            signatures: &snapshot.signatures,
            representations: &snapshot.representations,
            sources: &snapshot.sources,
            anchors: snapshot
                .source_owners
                .iter()
                .map(|(path, owner)| {
                    (
                        path,
                        snapshot.definitions.get(path).map_or_else(
                            || vec![owner.syntax.anchor],
                            |body| hirsel_tir::source_anchors(body),
                        ),
                    )
                })
                .collect(),
            foreign_links: &snapshot.foreign_links,
            foreign_unreplayable: snapshot.foreign_unreplayable,
            foreign_threads: &snapshot.foreign_threads,
        }
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    #[test]
    fn copied_source_metadata_rejects_excess_and_keeps_one_lifetime_charge() {
        let budget = hirsel_engine_admission::ProcessBudget::new(100);
        let mut allowance = budget.reserve(100).unwrap();
        let owner = Mutex::new(None);
        assert!(retain_source_metadata(&owner, 101, &mut allowance).is_err());
        assert_eq!(allowance.bytes(), 100);
        assert_eq!(budget.reserved(), 100);
        retain_source_metadata(&owner, 37, &mut allowance).unwrap();
        retain_source_metadata(&owner, 37, &mut allowance).unwrap();
        assert_eq!(allowance.bytes(), 63);
        let survivor = owner.lock().unwrap().clone().unwrap();
        drop(allowance);
        drop(owner);
        assert_eq!(budget.reserved(), 37);
        drop(survivor);
        assert_eq!(budget.reserved(), 0);
    }
}

pub struct LinkContextInput<'a> {
    pub options: &'a super::Options,
    pub input: &'a hirsel_lir::stage::InputIdentity,
    pub manifest: &'a hirsel_core_contract::Manifest,
}
/// Shared body-free linker projection. Both ordinary and ownership consumers
/// supply only summaries/layouts derived from their own exact current C scope.
pub struct LinkAssembly {
    pub operations: hirsel_layout::operation::OperationRegistry,
    pub semantic_owners: BTreeMap<hirsel_syntax::DefPath, Option<hirsel_syntax::DefPath>>,
    pub summaries: Vec<hirsel_lir::fragment::UnitSummary>,
    pub functions: BTreeMap<hirsel_syntax::DefPath, hirsel_syntax::ContentHash>,
    pub finalizers: BTreeMap<hirsel_lir::LayoutId, hirsel_syntax::ContentHash>,
    pub table: hirsel_lir::SealedLayoutTable,
    pub statics: Vec<hirsel_lir::statics::StaticData>,
}
pub fn with_link_assembly<R>(
    input: LinkContextInput<'_>,
    source: &NativeLinkSource,
    assembly: LinkAssembly,
    use_context: impl FnOnce(&ReadyProgram) -> Result<R, String>,
) -> Result<R, String> {
    let LinkAssembly {
        operations,
        semantic_owners,
        summaries,
        functions,
        finalizers,
        table,
        statics,
    } = assembly;
    let callbacks = summaries
        .iter()
        .flat_map(|s| s.glue.callbacks.iter().map(|(path, _)| path.clone()))
        .collect();
    let entries = summaries
        .iter()
        .flat_map(|s| {
            s.glue.entries.iter().map(|(path, instance)| {
                (
                    hirsel_lir::emission::function_thunk_name(path, instance.as_ref()),
                    path.to_string(),
                )
            })
        })
        .collect();
    let mut program = ReadyProgram {
        semantic_owners,
        semantic_units: None,
        validation: input.options.validation,
        operations,
        launch: input.options.launch.clone(),
        input: input.input.clone(),
        target: input.options.target.clone(),
        table,
        manifest: input.manifest.clone(),
        units: vec![],
        ahead_environments: BTreeMap::new(),
        summaries,
        inline_instances: BTreeSet::new(),
        callbacks,
        entries,
        finalizers,
        functions,
        launches: source.launches(),
        revisions: BTreeMap::new(),
        exit_reports: BTreeMap::new(),
        statics,
        discharged: input.options.discharge.clone(),
        foreign_links: vec![],
        data_abi: None,
        foreign_unreplayable: false,
        foreign_dispatch: BTreeMap::new(),
        escaping: BTreeSet::new(),

        failure_table: None,
    };
    source.bind(&mut program).map_err(|e| e.to_string())?;
    use_context(&program)
}

#[cfg(test)]
mod tests;
