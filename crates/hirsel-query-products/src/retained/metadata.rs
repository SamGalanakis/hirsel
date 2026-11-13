//! Copied native input allocations. Shared front payloads keep their front
//! reservation owners; this visitor never opens source or native IR.
use super::Weight;
use hirsel_syntax::DefPath;
use std::{alloc::Layout, collections::BTreeMap, sync::Arc};

/// Allocation-layout upper bound, not serialized length or an output allowance.
/// Interned rows and shared front syntax/signatures/V/summary/source-map payloads
/// are excluded: their original reservation owners must outlive the snapshot.
/// Allocator size classes and temporary visitor storage are not retained input.
#[derive(Default)]
pub struct MetadataWeight(Weight<'static>);
impl MetadataWeight {
    pub fn bytes(&self) -> u64 {
        self.0.bytes
    }
    pub fn add(&mut self, bytes: usize) {
        self.0.add(bytes);
    }
    pub fn path(&mut self, path: &DefPath) {
        self.0.def_path(path);
    }
    pub fn flat_vec<T>(&mut self, values: &Vec<T>) {
        self.0.flat_vec(values);
    }
    pub fn vec<T>(&mut self, values: &Vec<T>, mut visit: impl FnMut(&mut Self, &T)) {
        self.flat_vec(values);
        for value in values {
            visit(self, value);
        }
    }
    /// Owned Arc allocation (inline value only). Shared front payloads must not
    /// be passed here. Callers visit the backing buffers separately.
    pub fn arc<T>(&mut self, value: &Arc<T>) {
        self.add(hirsel_engine_admission::retained_arc_bytes(&**value) as usize);
    }
    pub fn program_revision(&mut self, value: &hirsel_run_schema::ProgramRevision) {
        self.add(value.retained_metadata_bytes() as usize);
    }
    pub fn code_reach(&mut self, value: &hirsel_run_schema::CodeReach) {
        if let hirsel_run_schema::CodeReach::Escapes(paths) = value {
            self.vec(paths, |w, path| w.add(path.capacity()));
        }
    }

    /// Bound a prospective clone+retain_owner or count its copied file/path
    /// buffers afterward; shared source text and original owners stay excluded.
    pub fn source_snapshot(&mut self, value: &hirsel_span::SourceMap) {
        self.add(value.retained_snapshot_metadata_bytes() as usize);
    }
    /// Rust alloc BTree nodes, library/alloc/src/collections/btree/node.rs:42-112
    /// (rust-lang/rust a7c37f9). Every non-root node has at least five entries;
    /// count each possible node as internal with eleven key/value slots and
    /// twelve edges. One empty root is allowed even after all rows are removed.
    /// Per-field alignment slack bounds Rust's private/reorderable Leaf layout.
    pub fn tree<K, V>(&mut self, len: usize) {
        let parts = [
            Layout::new::<usize>(),
            Layout::new::<u16>(),
            Layout::new::<u16>(),
            Layout::new::<[K; 11]>(),
            Layout::new::<[V; 11]>(),
            Layout::new::<[usize; 12]>(),
        ];
        let alignment = parts.iter().map(Layout::align).max().unwrap();
        let node = parts.iter().fold(alignment - 1, |n, l| {
            n.saturating_add(l.size()).saturating_add(l.align() - 1)
        });
        self.add((len / 5 + 1).saturating_mul(node));
    }
    /// Pack an owned private map before charging its immutable retained rows.
    /// Rust alloc's bulk_push fills eleven-key nodes before opening subtrees;
    /// fix_right_border_of_plentiful redistributes their keys without new nodes.
    /// See rust-lang/rust b940084d7, btree/append.rs:8-70, map.rs:2544-2554,
    /// and fix.rs:105-119.
    /// Repacking here establishes the bound even after arbitrary inserts/removes.
    pub fn packed_map<K: Ord, V>(
        &mut self,
        values: &mut BTreeMap<K, V>,
        mut visit: impl FnMut(&mut Self, &K, &V),
    ) {
        *values = std::mem::take(values).into_iter().collect();
        self.bulk_tree::<K, V>(values.len());
        for (key, value) in values.iter() {
            visit(self, key, value);
        }
    }
    /// Bound nodes of a bulk-collected immutable map. The caller must retain
    /// the construction invariant: no insertion or removal after collection.
    pub fn bulk_tree<K, V>(&mut self, len: usize) {
        if len != 0 {
            let parts = [
                Layout::new::<usize>(),
                Layout::new::<u16>(),
                Layout::new::<u16>(),
                Layout::new::<[K; 11]>(),
                Layout::new::<[V; 11]>(),
                Layout::new::<[usize; 12]>(),
            ];
            let alignment = parts.iter().map(Layout::align).max().unwrap();
            let bytes = parts.iter().fold(alignment - 1, |n, part| {
                n.saturating_add(part.size())
                    .saturating_add(part.align() - 1)
            });
            let mut keys = len;
            let mut nodes = 0usize;
            loop {
                keys /= 12;
                nodes = nodes.saturating_add(keys.saturating_add(1));
                if keys == 0 {
                    break;
                }
            }
            self.add(nodes.saturating_mul(bytes));
        }
    }
    pub fn map<K, V>(&mut self, values: &BTreeMap<K, V>, mut visit: impl FnMut(&mut Self, &K, &V)) {
        self.tree::<K, V>(values.len());
        for (key, value) in values {
            visit(self, key, value);
        }
    }
    pub fn representation(&mut self, value: &hirsel_tir::RepresentationRecord) {
        let hirsel_tir::RepresentationRecord {
            definition,
            representation_revision: _,
            generics,
            private_shape: _,
            layout_promises: _,
            resource: _,
            finalizer,
            unsafe_ffi_package: _,
        } = value;
        self.path(definition);
        self.flat_vec(generics);
        for generic in generics {
            self.0.generic(generic);
        }
        if let Some((path, _)) = finalizer {
            self.path(path);
        }
    }
    pub fn materialized(&mut self, value: &hirsel_tir::Materialized) {
        self.value_node(&value.root);
    }
    fn value_node(&mut self, value: &hirsel_tir::ValueNode) {
        use hirsel_tir::ValueKind;
        match &value.kind {
            ValueKind::Constant(value) => self.0.constant(value),
            ValueKind::FunctionReference {
                definition,
                arguments,
            } => {
                self.path(definition);
                self.flat_vec(arguments);
            }
            ValueKind::Construct {
                constructor,
                fields,
            } => {
                self.0.constructor(constructor);
                self.vec(fields, Self::value_node);
            }
        }
    }
    /// Compact private CT-5 hash tables before counting their allocation.
    /// hashbrown raw/mod.rs shrink_to (889-949), capacity_to_buckets (105-166),
    /// calculate_layout_for: after shrink_to_fit, buckets are bounded by the
    /// smallest power of two accommodating len at 7/8 load. Unlike capacity(),
    /// this remains valid for a map that previously retained deleted buckets.
    pub fn instance_bodies(&mut self, value: &mut hirsel_tir::InstanceBodies) {
        self.tree::<hirsel_syntax::DefId, ()>(value.declarations.len());
        self.tree::<hirsel_syntax::DefId,std::collections::HashMap<hirsel_types::GeneratedActualKey,hirsel_tir::InstanceBody>>(value.instances.len());
        for values in value.instances.values_mut() {
            values.shrink_to_fit();
            if !values.is_empty() {
                let buckets = values
                    .len()
                    .saturating_mul(8)
                    .div_ceil(7)
                    .checked_next_power_of_two()
                    .unwrap_or(usize::MAX)
                    .max(4);
                // 16-byte control group plus alignment of the key/value pair.
                let entry =
                    Layout::new::<(hirsel_types::GeneratedActualKey, hirsel_tir::InstanceBody)>();
                self.add(
                    buckets
                        .saturating_mul(entry.size().saturating_add(1))
                        .saturating_add(16)
                        .saturating_add(entry.align() - 1),
                );
            }
            for value in values.values() {
                self.flat_vec(&value.arguments);
            }
        }
    }
}
