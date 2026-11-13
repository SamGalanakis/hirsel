//! Retained allocation visitors adapted from Buck2 allocative; these count backing, never wire length.
mod metadata;
mod storage;
use crate::reads::{ReadKey, Reads};
use hirsel_engine_admission::arc_bytes;
use hirsel_syntax::{ContentHash, DefPath};
use hirsel_tir::*;
use hirsel_types::{
    ContextualName, Destination, Origin, OriginRoot, Resolution, ResolvedName, SharedPath,
    SharedSlice,
};
use std::{alloc::Layout, collections::HashSet};
mod front;
pub use front::{
    analyzed_capture_bytes, retained_analyzed_bytes, retained_structural_bytes,
    retained_structural_lookup_bytes, retained_structural_path_bytes,
};
mod signature;
pub use signature::{retained_signature_payload_bytes, signature_payload};
mod syntax;
pub use syntax::retained_syntax_bytes;
#[cfg(test)]
mod type_storage;
pub use metadata::MetadataWeight;
/// Bytes retained by a DefinitionRef, including its Arc allocation. Session
/// interner rows/DefIds and shared syntax belong to the session, not this hold.
/// Shared allocations are charged once within this artifact, conservatively
/// again if another hold also retains them. No allocator-specific size classes.
pub fn retained_tir_bytes(definition: &Definition) -> u64 {
    tir_bytes(definition, Weight::default())
}

/// Archive decoding recreates shared rows at every serialized occurrence.
/// Count each occurrence independently instead of assuming source Arc sharing.
pub fn decoded_tir_bytes(definition: &Definition) -> u64 {
    tir_bytes(
        definition,
        Weight {
            count_shared_repeats: true,
            ..Weight::default()
        },
    )
}
fn tir_bytes(definition: &Definition, mut weight: Weight) -> u64 {
    weight.add(arc_bytes(definition));
    let Definition {
        caller_bodies,
        protocol_associations,
        types,
        path: _,
        signature_hash: _,
        bodies,
        entry: _,
    } = definition;
    weight.flat_vec(caller_bodies);
    weight.vec(protocol_associations, |w, r| {
        w.def_path(&r.association.source)
    });
    weight.arc_slice(types);
    weight.vec(bodies, Weight::body);
    weight.bytes
}

/// Owned buffers of lowering's returned warnings and ordered signature
/// dependencies, excluding the containing Vec/inline fields of its wrapper.
pub fn retained_lowering_output_bytes(
    diagnostics: &Vec<hirsel_diag::Diagnostic>,
    dependencies: &Vec<hirsel_types::SignatureDependency>,
) -> u64 {
    let mut weight = Weight::default();
    weight.flat_vec(dependencies);
    weight.vec(diagnostics, Weight::diagnostic);
    weight.bytes
}

pub fn retained_query_error_bytes(error: &hirsel_types::QueryError) -> u64 {
    let mut weight = Weight::default();
    weight.error(error);
    weight.bytes
}
#[derive(Default)]
pub struct Weight<'a> {
    pub bytes: u64,
    pub shared: HashSet<usize>,
    pub count_shared_repeats: bool,
    pub decode_vectors: bool,
    pub cloned_vectors: bool,
    pub source_names: Option<&'a mut dyn NameAllocationSet>,
}
pub trait NameAllocationSet: Send + Sync {
    fn first(&mut self, identity: usize) -> bool;
    fn epoch(&self) -> hirsel_types::ReadAllocationEpoch;
}
pub struct NameAllocations<const N: usize = 32768> {
    slots: [usize; N],
    overflow: u64,
    epoch: hirsel_types::ReadAllocationEpoch,
}
impl<const N: usize> Default for NameAllocations<N> {
    fn default() -> Self {
        Self {
            slots: [0; N],
            overflow: 0,
            epoch: Default::default(),
        }
    }
}
impl<const N: usize> NameAllocations<N> {
    pub fn stats(&self) -> (usize, u64) {
        (
            self.slots.iter().filter(|slot| **slot != 0).count(),
            self.overflow,
        )
    }
}
impl<const N: usize> NameAllocationSet for NameAllocations<N> {
    fn epoch(&self) -> hirsel_types::ReadAllocationEpoch {
        self.epoch
    }
    fn first(&mut self, identity: usize) -> bool {
        debug_assert!(N.is_power_of_two());
        let start = ((identity >> 4) ^ (identity >> 19)) & (N - 1);
        for offset in 0..N.min(64) {
            let slot = &mut self.slots[(start + offset) & (N - 1)];
            if *slot == identity {
                return false;
            }
            if *slot == 0 {
                *slot = identity;
                return true;
            }
        }
        // A full or clustered tracker charges unknown allocations again.
        // Bound inspection work as well as its allocation-free scratch space.
        self.overflow = self.overflow.saturating_add(1);
        true
    }
}
impl Weight<'_> {
    fn read_name_allocations(&mut self, allocations: impl IntoIterator<Item = (usize, usize)>) {
        for (identity, bytes) in allocations {
            if bytes == 0 {
                continue;
            }
            let bytes = if self.decode_vectors {
                bytes.saturating_mul(2)
            } else {
                bytes
            };
            self.allocation(identity, bytes);
        }
    }
    /// Serde recreates every serialized Arc occurrence. Sequence decoding may
    /// grow from its cautious initial capacity; doubling the producing Vec's
    /// capacity covers that geometric growth without relying on wire length.
    pub fn decoded() -> Self {
        Self {
            count_shared_repeats: true,
            decode_vectors: true,
            ..Self::default()
        }
    }
    fn first(&mut self, identity: usize) -> bool {
        self.count_shared_repeats || self.shared.insert(identity)
    }
    pub fn allocation(&mut self, identity: usize, bytes: usize) -> bool {
        if identity != 0 && !self.first(identity) {
            return false;
        }
        self.add(bytes);
        true
    }
    pub fn arc<T: ?Sized>(&mut self, value: &std::sync::Arc<T>, visit: impl FnOnce(&mut Self, &T)) {
        if self.allocation(
            std::sync::Arc::as_ptr(value).cast::<()>() as usize,
            arc_bytes(&**value),
        ) {
            visit(self, value);
        }
    }
    pub fn reads(&mut self, value: &std::sync::Arc<Reads>) {
        self.arc(value, |w, reads| {
            for (identity, bytes) in reads.allocations() {
                w.allocation(identity, bytes);
            }
        });
        if self.decode_vectors && !value.is_empty() {
            // Reads::decode builds a cautious Vec, then Reads::from_vec
            // deduplicates >=32 rows in a HashMap and collects a second Vec
            // before moving the result into its final Arc slice. Their
            // working buffers are distinct from the retained read allocation.
            let slots = value.len().saturating_mul(2).max(4);
            self.add(
                slots
                    .saturating_mul(2)
                    .saturating_mul(size_of::<(ReadKey, ContentHash)>()),
            );
            if value.len() >= 32 {
                // hashbrown raw capacity_to_buckets / calculate_layout_for:
                // 7/8 occupancy, power-of-two buckets, one control byte per
                // bucket plus a SIMD control group and alignment. Both old
                // and new tables may coexist while a growing table rehashes.
                let buckets = value.len().saturating_mul(8).saturating_add(6) / 7;
                let buckets = buckets.checked_next_power_of_two().unwrap_or(usize::MAX);
                let rows = Layout::new::<(ReadKey, ContentHash)>();
                let table = buckets
                    .saturating_mul(rows.size().saturating_add(1))
                    .saturating_add(16)
                    .saturating_add(rows.align());
                self.add(table.saturating_mul(2));
            }
        }
    }
    pub fn diagnostic(&mut self, diagnostic: &hirsel_diag::Diagnostic) {
        let hirsel_diag::Diagnostic {
            rule,
            severity: _,
            message,
            primary: _,
            secondary,
            notes,
            fix,
            obligations,
            root_cause: _,
        } = diagnostic;
        self.add(rule.retained_bytes());
        self.add(message.capacity());
        self.vec(secondary, |w, label| w.add(label.message.capacity()));
        self.vec(notes, |w, note| w.add(note.capacity()));
        if let Some(fix) = fix {
            self.add(fix.replacement.capacity());
            self.add(fix.message.capacity());
        }
        self.vec(obligations, |w, obligation| match obligation {
            hirsel_diag::Obligation::GenericBound { arguments } => w.flat_vec(arguments),
            hirsel_diag::Obligation::Multiplicity
            | hirsel_diag::Obligation::CopyProof
            | hirsel_diag::Obligation::ParameterMode { .. } => (),
        });
    }
    fn package(&mut self, package: &hirsel_syntax::PackageId) {
        self.add(package.name.capacity());
        self.add(package.version.capacity());
    }
    pub fn error(&mut self, error: &hirsel_types::QueryError) {
        match error {
            hirsel_types::QueryError::Operational(failure) => self.failure(failure),
            hirsel_types::QueryError::Diagnostics(diagnostics) => {
                self.vec(diagnostics, Weight::diagnostic)
            }
            hirsel_types::QueryError::NotImplemented => (),
        }
    }
    pub fn failure(&mut self, failure: &hirsel_types::OperationalFailure) {
        self.add(failure.operation.capacity());
        self.add(failure.message.capacity());
    }
    fn dependency(&mut self, read: &hirsel_types::DependencyRead) {
        use hirsel_types::{DependencyRead, LookupError};
        match read.as_ref() {
            DependencyRead::ProviderLookup {
                context,
                package,
                segments,
                output,
            } => {
                if self
                    .source_names
                    .as_ref()
                    .is_none_or(|seen| context.first_in(seen.epoch()))
                {
                    self.read_name_allocations(context.retained_allocations());
                }
                if self
                    .source_names
                    .as_ref()
                    .is_none_or(|seen| segments.first_in(seen.epoch()))
                {
                    self.read_name_allocations(segments.retained_allocations());
                }
                if let Some(package) = package {
                    self.package(package);
                }
                match &output.result {
                    Ok(_) => (),
                    Err(error) => self.error(error),
                }
            }
            DependencyRead::Lookup {
                context,
                package,
                segments,
                result,
                manifest: _,
            } => {
                if self
                    .source_names
                    .as_ref()
                    .is_none_or(|seen| context.first_in(seen.epoch()))
                {
                    self.read_name_allocations(context.retained_allocations());
                }
                if self
                    .source_names
                    .as_ref()
                    .is_none_or(|seen| segments.first_in(seen.epoch()))
                {
                    self.read_name_allocations(segments.retained_allocations());
                }
                if let Some(package) = package {
                    self.package(package);
                }
                match result {
                    Ok(_) => (),
                    Err(LookupError::Invisible(path)) => self.def_path(path),
                    Err(LookupError::MissingMember(paths) | LookupError::Ambiguous(paths)) => {
                        self.vec(paths, Weight::def_path)
                    }
                    Err(LookupError::Provider(diagnostics)) => {
                        self.vec(diagnostics, Weight::diagnostic)
                    }
                    Err(LookupError::Missing) => (),
                }
            }
            DependencyRead::ModuleHead {
                context,
                head,
                manifest: _,
                exposed: _,
            } => {
                if self
                    .source_names
                    .as_ref()
                    .is_none_or(|seen| context.first_in(seen.epoch()))
                {
                    self.read_name_allocations(context.retained_allocations());
                }
                self.add(head.capacity());
            }
            // Definition identities are owned by the session interner.
            DependencyRead::Signature { .. } => (),
        }
    }
    fn shared_allocation(&mut self, identity: usize, bytes: usize) -> bool {
        if let Some(seen) = &mut self.source_names {
            if !seen.first(identity) {
                return false;
            }
            self.add(bytes);
            true
        } else {
            self.allocation(identity, bytes)
        }
    }
    fn shared_dependency(&mut self, read: &hirsel_types::SharedRead) {
        let (identity, bytes) = read.retained_allocation();
        let first = if let Some(seen) = &self.source_names {
            read.first_in(seen.epoch())
        } else {
            self.first(identity)
        };
        if first {
            self.add(bytes);
            // Cloning a handle never clones the observation's owned buffers.
            let cloned = std::mem::replace(&mut self.cloned_vectors, false);
            self.dependency(read);
            self.cloned_vectors = cloned;
        }
    }
    pub fn read_log(&mut self, value: &std::sync::Arc<SharedPath<hirsel_types::SharedRead>>) {
        if !self.shared_allocation(std::sync::Arc::as_ptr(value) as usize, arc_bytes(&**value)) {
            return;
        }
        let mut path = &**value;
        while let Some(identity) = path.node_identity() {
            if !self.shared_allocation(identity, path.node_allocation_bytes()) {
                break;
            }
            self.shared_dependency(path.last().unwrap());
            path = path.prefix().unwrap();
        }
    }
    pub fn detail(&mut self, detail: &crate::DependencyReads) {
        let crate::DependencyReads {
            operational_failure,
            signatures,
            failed_signatures,
            failed_signature_keys,
            lookups,
            manifest_lookups,
            resolver,
            citations,
            closure,
        } = detail;
        if let Some(failure) = operational_failure {
            self.failure(failure);
        }
        self.flat_vec(signatures);
        self.vec(failed_signatures, Weight::def_path);
        self.vec(failed_signature_keys, |w, (path, _)| w.def_path(path));
        self.flat_vec(lookups);
        self.vec(manifest_lookups, Weight::shared_dependency);
        self.read_log(resolver);
        self.vec(citations, |w, (path, _)| w.def_path(path));
        self.flat_vec(closure);
    }
    pub fn add(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes as u64);
    }
    pub fn flat_vec<T>(&mut self, value: &Vec<T>) {
        self.add(
            (if self.cloned_vectors {
                value.len()
            } else {
                value.capacity()
            })
            .saturating_mul(if self.decode_vectors { 2 } else { 1 })
            .saturating_mul(size_of::<T>()),
        );
    }
    pub fn vec<T>(&mut self, value: &Vec<T>, mut visit: impl FnMut(&mut Self, &T)) {
        self.flat_vec(value);
        for element in value {
            visit(self, element);
        }
    }
    fn arc_slice<T>(&mut self, value: &std::sync::Arc<[T]>) {
        if self.first(value.as_ptr() as usize) {
            self.add(arc_bytes(&**value));
        }
    }
    fn shared_slice<T>(&mut self, value: &SharedSlice<T>) {
        let identity = value.storage_identity();
        if identity != 0 && self.first(identity) {
            self.add(arc_bytes(value.as_slice()));
        }
    }
    fn shared_values<T>(&mut self, value: &SharedSlice<T>, mut visit: impl FnMut(&mut Self, &T)) {
        let identity = value.storage_identity();
        if identity != 0 && self.first(identity) {
            self.add(arc_bytes(value.as_slice()));
            for element in value {
                visit(self, element);
            }
        }
    }
    pub fn definition_summary(&mut self, summary: &crate::DefinitionSummary) {
        if self.decode_vectors {
            self.add(size_of_val(&*summary.frames).saturating_mul(2));
            self.add(size_of_val(&*summary.calls).saturating_mul(2));
            self.add(size_of_val(&*summary.staged).saturating_mul(2));
        }
        self.shared_values(&summary.frames, |w, frame| {
            w.add(size_of_val(&*frame.size_bindings));
        });
        self.shared_values(&summary.calls, |w, call| {
            w.add(size_of_val(&*call.arguments));
        });
        self.shared_values(&summary.staged, |w, (_, parameters)| {
            w.add(size_of_val(&**parameters));
        });
    }
    pub fn parameter_growth(&mut self, growth: &crate::ParameterGrowth) {
        if self.decode_vectors {
            self.add(size_of_val(&*growth.edges).saturating_mul(2));
        }
        self.shared_slice(&growth.edges);
    }
    pub fn checked_summary(&mut self, summary: &crate::CheckedSummary) {
        self.definition_summary(&summary.summary);
        self.parameter_growth(&summary.growth);
    }
    pub fn generic(&mut self, value: &hirsel_types::GenericParameter) {
        match value {
            hirsel_types::GenericParameter::Type { name, bounds } => {
                self.add(name.capacity());
                self.shared_slice(bounds);
            }
            hirsel_types::GenericParameter::Size { name } => self.add(name.capacity()),
        }
    }
    fn authority(&mut self, value: &hirsel_types::AuthoritySummary) {
        match value {
            hirsel_types::AuthoritySummary::Known(terms) => {
                self.shared_values(terms, |w, term| match term {
                    hirsel_types::AuthorityTerm::Capability(path) => w.def_path(path),
                    hirsel_types::AuthorityTerm::Parameter(_)
                    | hirsel_types::AuthorityTerm::TypeParameter(_) => (),
                })
            }
            hirsel_types::AuthoritySummary::Unknown => (),
        }
    }
    pub fn signature(&mut self, value: &hirsel_types::SignatureRef) {
        if !self.first(std::sync::Arc::as_ptr(value) as usize) {
            return;
        }
        self.add(arc_bytes(&**value));
        let hirsel_types::Signature {
            metadata,
            definition: _,
            kind,
            fingerprint: _,
        } = &**value;
        let hirsel_types::SignatureMetadata {
            retention: _,
            visibility: _,
            alias: _,
            constant_alias,
            track_caller: _,
            unsafe_ffi: _,
            wrapper: _,
            roles,
            schema,
            recursive_group: _,
            implementations,
        } = metadata;
        for binder in implementations {
            self.def_path(binder.family.path());
        }
        if let Some(path) = constant_alias {
            self.add(size_of_val(&**path));
            self.def_path(path);
        }
        if self.shared_allocation(std::sync::Arc::as_ptr(roles) as usize, arc_bytes(&**roles)) {
            let hirsel_types::NominalRoles {
                unsafe_capability,
                result_type,
                tasks_capability,
                scope_capability,
                heap,
            } = &**roles;
            for path in [
                unsafe_capability,
                result_type,
                tasks_capability,
                scope_capability,
            ]
            .into_iter()
            .flatten()
            {
                self.def_path(path);
            }
            if let Some(hirsel_types::HeapRole { arena, operation }) = heap {
                self.def_path(arena);
                self.def_path(operation);
            }
        }
        self.flat_vec(schema);
        match kind {
            hirsel_types::SignatureKind::Const { generics, ty: _ } => {
                self.shared_values(generics, Weight::generic)
            }
            hirsel_types::SignatureKind::Family(family) => {
                for member in &family.members {
                    self.add(member.name.capacity());
                }
            }
            hirsel_types::SignatureKind::Implementation(implementation) => {
                self.def_path(implementation.family.path());
                for (name, actual) in &implementation.members {
                    self.add(name.capacity());
                    if let hirsel_types::MemberActual::Operation(target) = actual {
                        self.def_path(target);
                    }
                }
            }
            hirsel_types::SignatureKind::Function(function) => {
                let hirsel_types::FunctionType {
                    state_promises,
                    generics,
                    parameters,
                    result: _,
                    error: _,
                    allocation: _,
                    authority,
                    uses,
                    incomplete: _,
                    foreign,
                } = function;
                self.shared_values(state_promises, |w, promise| {
                    w.path(&promise.selector.fields);
                    w.shared_slice(&promise.unretained);
                });
                self.shared_values(generics, Weight::generic);
                self.shared_values(parameters, |w, parameter| {
                    for name in [&parameter.name, &parameter.label].into_iter().flatten() {
                        w.add(name.capacity());
                    }
                });
                self.authority(authority);
                match uses {
                    hirsel_types::Uses::Only(terms) => self.authority(terms),
                    hirsel_types::Uses::Unrestricted => (),
                }
                if let Some(foreign) = foreign {
                    self.add(foreign.abi.capacity());
                    self.add(foreign.aliasing_contract.capacity());
                }
            }
            hirsel_types::SignatureKind::Type(definition) => {
                self.add(size_of_val(&**definition));
                self.type_definition(definition);
            }
        }
    }
    pub fn type_definition(&mut self, definition: &hirsel_types::TypeDefinition) {
        let hirsel_types::TypeDefinition {
            loop_source,
            format,
            destructor_allocation: _,
            generics,
            representation: _,
            bounds,
            authority,
            capability: _,
            implementation_authority,
            serializable: _,
            serializable_check,
            layout: _,
            distinct: _,
            resource: _,
            drop,
            captures,
        } = definition;
        if let Some(hirsel_types::LoopSourceDeclaration { open, next, option }) = loop_source {
            self.def_path(open);
            self.def_path(next);
            self.def_path(option);
        }
        for path in [format, drop].into_iter().flatten() {
            self.def_path(path);
        }
        self.vec(generics, Weight::generic);
        self.flat_vec(bounds);
        self.authority(authority);
        if let Some(authority) = implementation_authority {
            self.authority(authority);
        }
        if let Some(check) = serializable_check {
            self.def_path(&check.function);
        }
        if let Some(captures) = captures {
            self.shared_slice(captures);
        }
    }
    fn path<T>(&mut self, mut value: &SharedPath<T>) {
        while let Some(identity) = value.node_identity() {
            if !self.first(identity) {
                break;
            }
            self.add(value.node_allocation_bytes());
            value = value.prefix().expect("nonempty paths have a prefix");
        }
    }
    pub fn def_path(&mut self, value: &DefPath) {
        let hirsel_syntax::ItemPath {
            package,
            modules,
            name,
        } = &value.0;
        self.add(package.name.capacity());
        self.add(package.version.capacity());
        self.add(name.capacity());
        self.vec(modules, |w, name| w.add(name.capacity()));
    }
    fn origin(&mut self, value: &Origin) {
        match &value.root {
            OriginRoot::Definition(path) => self.def_path(path),
            OriginRoot::ArgumentBrand(_)
            | OriginRoot::Expression(..)
            | OriginRoot::Parameter(_)
            | OriginRoot::Local(..)
            | OriginRoot::Sibling(_) => (),
        }
        self.path(&value.fields);
    }
    fn destination(&mut self, value: &Destination) {
        match value {
            Destination::Arena(origin) => self.origin(origin),
            Destination::Caller(parameters) => self.shared_slice(parameters),
            Destination::Heap | Destination::Region(_) | Destination::Inferred => (),
        }
    }
    pub fn constant(&mut self, value: &Constant) {
        match value {
            Constant::String(value) => self.add(value.capacity()),
            Constant::Bytes(value) => self.flat_vec(value),
            Constant::Unit
            | Constant::Bool(_)
            | Constant::Integer(_)
            | Constant::F32(_)
            | Constant::F64(_) => (),
        }
    }
    pub fn constructor(&mut self, value: &Constructor) {
        match value {
            Constructor::Record(path)
            | Constructor::Variant {
                definition: path, ..
            } => {
                self.def_path(path);
            }
            Constructor::Tuple
            | Constructor::Sequence
            | Constructor::Map
            | Constructor::Set
            | Constructor::SortedMap { .. }
            | Constructor::Box => (),
        }
    }
    fn rvalue(&mut self, value: &Rvalue) {
        match value {
            Rvalue::Constant(value) => self.constant(value),
            Rvalue::ReadConstant {
                definition,
                arguments,
            } => {
                self.def_path(definition);
                self.flat_vec(arguments);
            }
            Rvalue::FunctionReference {
                definition,
                arguments,
            } => {
                self.def_path(definition);
                self.flat_vec(arguments);
            }
            Rvalue::Construct {
                constructor,
                fields,
                destination,
            } => {
                self.constructor(constructor);
                self.flat_vec(fields);
                self.destination(destination);
            }
            Rvalue::Closure {
                captures,
                destination,
                ..
            } => {
                self.flat_vec(captures);
                self.destination(destination);
            }
            Rvalue::Size(_) | Rvalue::Use(_) | Rvalue::Unary { .. } | Rvalue::Binary { .. } => (),
        }
    }
    fn instruction(&mut self, instruction: &Instruction) {
        match &instruction.kind {
            InstructionKind::BindFields { fields, .. } => self.flat_vec(fields),
            InstructionKind::Defuse { .. }
            | InstructionKind::BindOrigin { .. }
            | InstructionKind::EndOrigin(_)
            | InstructionKind::StorageLive(_)
            | InstructionKind::StorageDead { .. }
            | InstructionKind::EndScope(_)
            | InstructionKind::Assign { .. }
            | InstructionKind::Replace { .. }
            | InstructionKind::BeginLoan { .. }
            | InstructionKind::EndLoan { .. }
            | InstructionKind::Drop { .. }
            | InstructionKind::RunCleanup(_)
            | InstructionKind::Check { .. }
            | InstructionKind::Charge(_) => (),
        }
    }
    fn terminator(&mut self, value: &Terminator) {
        match value {
            Terminator::Switch { arms, .. } => self.flat_vec(arms),
            Terminator::Call {
                callee, arguments, ..
            } => {
                self.flat_vec(arguments);
                match callee {
                    Callee::Definition { path, arguments } => {
                        self.def_path(path);
                        self.flat_vec(arguments);
                    }
                    Callee::Indirect { arguments, .. } => self.flat_vec(arguments),
                }
            }
            Terminator::Goto(_)
            | Terminator::Match { .. }
            | Terminator::Branch { .. }
            | Terminator::Return { .. }
            | Terminator::Fail { .. }
            | Terminator::Unreachable => (),
        }
    }
    fn pattern(&mut self, value: &MatchPattern) {
        match &value.kind {
            MatchPatternKind::Literal(value) => self.constant(value),
            MatchPatternKind::Range { start, end } => {
                self.constant(start);
                self.constant(end);
            }
            MatchPatternKind::Tuple(fields) => self.flat_vec(fields),
            MatchPatternKind::Record { definition, fields }
            | MatchPatternKind::Variant {
                definition, fields, ..
            } => {
                self.def_path(definition);
                self.flat_vec(fields);
            }
            MatchPatternKind::Wildcard | MatchPatternKind::Binding { .. } => (),
        }
    }
    fn body(&mut self, body: &Body) {
        let Body {
            arena_scope_grant,
            scoped_parameters,
            parameter_scope: _,
            locals,
            places,
            scopes,
            blocks,
            rvalues,
            cleanups,
            cleanup_nodes,
            entry: _,
            parameters,
            result: _,
            tail_sites,
            borrow_cursors,
            matches,
        } = body;
        if let Some(grant) = arena_scope_grant {
            self.def_path(&grant.intrinsic);
        }
        self.flat_vec(locals);
        self.vec(places, |w, place| {
            w.path(&place.projection);
            if let Some(origin) = &place.binding {
                w.origin(origin);
            }
        });
        self.flat_vec(scopes);
        self.vec(blocks, |w, block| {
            w.vec(&block.instructions, Weight::instruction);
            w.terminator(&block.terminator);
        });
        self.vec(rvalues, Weight::rvalue);
        self.flat_vec(cleanups);
        self.flat_vec(cleanup_nodes);
        self.flat_vec(parameters);
        self.vec(scoped_parameters, |w, p| w.flat_vec(&p.fields));
        self.vec(borrow_cursors, |w, cursor| {
            if let Some(cover) = &cursor.cover {
                w.flat_vec(cover);
            }
        });
        self.vec(tail_sites, |w, site| {
            w.flat_vec(&site.owned_loop_arguments);
            match &site.kind {
                TailKind::ConstructorHole(path) => w.flat_vec(path),
                TailKind::SelfTail => (),
            }
        });
        self.vec(matches, |w, table| {
            w.vec(&table.patterns, Weight::pattern);
            w.flat_vec(&table.arms);
        });
    }
}

impl Weight<'_> {
    pub fn resolution(&mut self, value: &Resolution) {
        let Resolution {
            reads,
            contextual,
            paths,
            local_functions,
            drop_binding: _,
            deferred,
            generics,
            generic_declarations,
            reference_scopes,
            expressions,
            types,
            bindings,
            dependencies,
            static_arguments,
            implementation_binders,
        } = value;
        self.read_log(reads);
        self.vec(contextual, |w, (_, name)| match name {
            ContextualName::Constructor(path)
            | ContextualName::Variant { owner: path, .. }
            | ContextualName::Field { owner: path, .. } => w.def_path(path),
        });
        let name = |w: &mut Self, name: &ResolvedName| match name {
            ResolvedName::Definition(path) => w.def_path(path),
            ResolvedName::Local(_)
            | ResolvedName::Parameter(_)
            | ResolvedName::Variant(_)
            | ResolvedName::Size(_)
            | ResolvedName::TypeParameter(_)
            | ResolvedName::Primitive(_) => (),
        };
        self.vec(paths, |w, (_, value)| name(w, value));
        self.flat_vec(local_functions);
        self.vec(deferred, |w, value| w.add(value.name.capacity()));
        self.flat_vec(generics);
        generic_declarations.visit_retained_allocations(|_, bytes| {
            self.add(bytes);
            true
        });
        self.flat_vec(reference_scopes);
        self.vec(expressions, |w, (_, value)| name(w, value));
        self.vec(types, |w, (_, value)| name(w, value));
        self.flat_vec(bindings);
        self.vec(dependencies, Self::def_path);
        self.vec(static_arguments, |w, (_, arguments)| w.flat_vec(arguments));
        self.vec(implementation_binders, |w, binder| {
            w.add(binder.name.capacity());
            w.vec(&binder.members, |w, (name, _)| w.add(name.capacity()));
        });
    }
}
