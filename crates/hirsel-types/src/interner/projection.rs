use super::*;
use std::collections::HashSet;

#[derive(Default)]
pub(super) struct FrozenRows {
    pub(super) rows: Box<[(TypeId, Entry)]>,
    pub(super) positions: HashMap<TypeId, u32, IdHasher>,
}

/// A source-owned subset of an existing type context. Its immutable rows keep
/// their original ids; new rows start beyond the original context's high water.
/// Derived normal forms and validation caches belong to the new context.
pub struct TypeProjection {
    pub(super) source: Arc<TypeInterner>,
    pub(super) ids: Box<[TypeId]>,
    pub(super) positions: HashMap<TypeId, u32, IdHasher>,
    pub(super) shards: [HashMap<u64, Ids, IdHasher>; SHARDS],
    high_water: u32,
}

impl TypeInterner {
    /// Prepare the local closure of `roots`, using the ordinary type-table
    /// graph edges. Base rows remain with their original immutable owner and
    /// are never demanded. Preparation copies index metadata, not type rows.
    pub fn project_reachable(
        self: &Arc<Self>,
        roots: impl IntoIterator<Item = TypeId>,
    ) -> TypeProjection {
        let _retained = self.owner.enter();
        let mut seen = HashSet::<TypeId, IdHasher>::default();
        let mut pending: Vec<_> = roots.into_iter().collect();
        let mut ids = Vec::new();
        while let Some(id) = pending.pop() {
            if id
                .position()
                .is_none_or(|position| position < self.base_len)
                || !seen.insert(id)
            {
                continue;
            }
            let Some(row) = self.get_if_loaded(id) else {
                continue;
            };
            ids.push(id);
            pending.extend(row.kind.graph_edges().into_iter().map(|edge| edge.ty));
        }
        ids.sort_unstable_by_key(|id| id.position());
        let positions = ids
            .iter()
            .enumerate()
            .map(|(position, id)| (*id, position as u32))
            .collect();
        let mut shards: [HashMap<u64, Ids, IdHasher>; SHARDS] =
            std::array::from_fn(|_| HashMap::default());
        for id in &ids {
            let row = self.get_if_loaded(*id).expect("collected local row");
            let hash = row_hash(row);
            let index = &mut shards[(hash >> 58) as usize % SHARDS];
            if index.get(&hash).is_some_and(|matches| {
                matches
                    .iter()
                    .any(|other| self.get_if_loaded(other) == Some(row))
            }) {
                continue;
            }
            match index.get_mut(&hash) {
                Some(matches) => matches.push(*id),
                None => {
                    index.insert(hash, Ids::One(*id));
                }
            }
        }
        TypeProjection {
            source: self.clone(),
            ids: ids.into(),
            positions,
            shards,
            high_water: self
                .local_start
                .checked_add(self.assigned.load(Ordering::Acquire))
                .expect("existing type interner high water"),
        }
    }
}

impl TypeProjection {
    /// Construct under the caller's admitted reservation. All existing
    /// contexts and externally held rows remain untouched and readable.
    pub fn into_interner(self) -> Arc<TypeInterner> {
        let owner = Owner::new(OwnerKind::SessionTypes);
        let _retained = owner.enter();
        let mut bytes = 0;
        let rows = self
            .ids
            .iter()
            .map(|id| {
                let row = self
                    .source
                    .get_if_loaded(*id)
                    .expect("projected local row remains immutable")
                    .clone();
                bytes += row_bytes(&row);
                (*id, Entry::new(row))
            })
            .collect();
        Arc::new(TypeInterner {
            owner: owner.clone(),
            base: self.source.base.clone(),
            base_len: self.source.base_len,
            local_start: self.high_water,
            frozen: FrozenRows {
                rows,
                positions: self.positions,
            },
            base_normals: AppendTable::new(),
            entries: AppendTable::new(),
            assigned: AtomicU32::new(0),
            shards: self.shards.map(RwLock::new),
            counters: Counters {
                row_bytes: AtomicU64::new(bytes),
                ..Default::default()
            },
        })
    }
}
