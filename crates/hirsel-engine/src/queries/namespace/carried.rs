//! One view's namespace rows, offered to the next view of the same producer.
//!
//! A row is a function of the engine state it read: the definition's items or
//! catalog candidates, each one's placed file, its declaration row, and the
//! session constants. A carried row is reused only while a witness of each of
//! those reads still holds; otherwise the row is computed as on a first view.
//! Item and file witnesses are weak: a carried row pins no source payload and
//! no lease, and a witness matches only the live object it was taken from.
use super::*;
use hirsel_span::{FileId, SourceFile};
use std::collections::HashMap;
use std::sync::Weak;

/// Owned by one demand producer and charged to its request.
pub(crate) struct ViewBase {
    pool: Arc<hirsel_engine_admission::ProcessBudget>,
    carried: Option<Carried>,
}
/// The previous view's rows and the session constants every row read.
pub(super) struct Carried {
    constants: Constants,
    catalog: Option<catalog::CatalogIdentity>,
    rows: HashMap<DefId, Row>,
    _charge: hirsel_engine_admission::MemoryReservation,
}
/// The reads every ordinary row shares.
#[derive(PartialEq)]
struct Constants {
    environment: ContentHash,
    store: ContentHash,
    package_manifest: ContentHash,
    manifests: BTreeMap<hirsel_syntax::PackageId, ContentHash>,
}
impl Constants {
    fn of(engine: &QueryEngine) -> Self {
        Self {
            environment: engine.environment_value(),
            store: engine.store_identity,
            package_manifest: engine.environment.package_manifest_hash,
            manifests: engine.package_manifests.clone(),
        }
    }
}
/// A carried row and the witness of what computing it read.
pub(super) struct Row {
    body: Arc<BodyInput>,
    declaration: Option<ContentHash>,
    /// The definition's items, in candidate order; `None` for a catalog row.
    items: Option<Box<[Weak<ItemSyntax>]>>,
    /// Each item's (or catalog candidate's) placed file, in the same order.
    files: Box<[(FileId, u32, Weak<SourceFile>)]>,
}
/// The rows the current capture leaves for the next view.
#[derive(Default)]
pub(super) struct Rows(HashMap<DefId, Row>);

fn same<T>(witness: &Weak<T>, current: &Arc<T>) -> bool {
    std::ptr::eq(witness.as_ptr(), Arc::as_ptr(current)) && witness.upgrade().is_some()
}

impl ViewBase {
    pub(crate) fn new(pool: Arc<hirsel_engine_admission::ProcessBudget>) -> Self {
        Self {
            pool,
            carried: None,
        }
    }
    /// The previous view's rows, if the reads every row shares are unchanged.
    pub(super) fn take(&mut self, engine: &QueryEngine) -> Option<Carried> {
        let mut carried = self.carried.take()?;
        if carried.constants != Constants::of(engine) {
            return None;
        }
        // Catalog rows read the catalog; item rows never do.
        if carried
            .catalog
            .as_ref()
            .is_some_and(|catalog| !catalog.matches(&engine.catalog))
        {
            carried.catalog = None;
        }
        Some(carried)
    }
    /// Keep `rows` for the next view, if the request admits their metadata.
    pub(super) fn replace(&mut self, engine: &QueryEngine, rows: Rows) {
        let bytes = rows.retained_bytes();
        self.carried = self.pool.reserve(bytes).ok().map(|charge| Carried {
            constants: Constants::of(engine),
            catalog: Some(engine.catalog.identity()),
            rows: rows.0,
            _charge: charge,
        });
    }
}
impl Carried {
    /// The carried row of `id` when every read it made still holds.
    pub(super) fn reuse(&mut self, engine: &QueryEngine, id: DefId, path: &DefPath) -> Option<Row> {
        let row = self.rows.get(&id)?;
        if row.declaration != engine.declarations.candidates(path) {
            return None;
        }
        // The item, file and declaration witnesses overlap today: the engine
        // makes items only by parsing a file, so new text is a new FileId and
        // new item Arcs, and indexing another header for a path also replaces
        // its items. An edit path that changes one without the others (text
        // edited in place, an item swapped under an unchanged file) leaves
        // that single witness as the only guard.
        let current = engine.items.get(path);
        let placed = match (&row.items, current) {
            (Some(items), Some(current)) => {
                items.len() == current.len()
                    && items.iter().zip(current).all(|(item, now)| same(item, now))
                    && row.files.len() == current.len()
                    && row.files.iter().zip(current).all(|(file, item)| {
                        placed(engine, file, engine.sources.anchor(item.anchor))
                    })
            }
            (None, None) => {
                self.catalog.is_some()
                    && engine
                        .catalog
                        .candidate_files(path)
                        .eq(row.files.iter().map(|(file, _, _)| *file))
                    && row
                        .files
                        .iter()
                        .all(|file| placed(engine, file, Some((file.0, file.1))))
            }
            _ => false,
        };
        if !placed {
            return None;
        }
        self.rows.remove(&id)
    }
}
fn placed(
    engine: &QueryEngine,
    (file, base, witness): &(FileId, u32, Weak<SourceFile>),
    current: Option<(FileId, u32)>,
) -> bool {
    current == Some((*file, *base))
        && engine
            .sources
            .file_handle(*file)
            .is_some_and(|current| same(witness, current))
}
impl Row {
    pub(super) fn body(&self) -> &Arc<BodyInput> {
        &self.body
    }
}
impl Rows {
    pub(super) fn keep(&mut self, id: DefId, row: Row) {
        self.0.insert(id, row);
    }
    /// Carry an ordinary row with the witness of what it read. A row that
    /// could read the header products (no items, no complete catalog row,
    /// no declaration row) is never carried.
    pub(super) fn offer(
        &mut self,
        engine: &QueryEngine,
        id: DefId,
        path: &DefPath,
        body: &Arc<BodyInput>,
    ) {
        let declaration = engine.declarations.candidates(path);
        let row = match engine.items.get(path) {
            Some(items) => {
                let Some(files) = items
                    .iter()
                    .map(|item| {
                        let (file, base) = engine.sources.anchor(item.anchor)?;
                        let handle = engine.sources.file_handle(file)?;
                        Some((file, base, Arc::downgrade(handle)))
                    })
                    .collect::<Option<Box<[_]>>>()
                else {
                    return;
                };
                Row {
                    body: body.clone(),
                    declaration,
                    items: Some(items.iter().map(Arc::downgrade).collect()),
                    files,
                }
            }
            None => {
                if declaration.is_none()
                    || engine.catalog.body(path).is_none()
                    || engine.catalog.facts(path).is_none()
                {
                    return;
                }
                let Some(files) = engine
                    .catalog
                    .candidate_files(path)
                    .map(|file| {
                        let handle = engine.sources.file_handle(file)?;
                        Some((file, 0, Arc::downgrade(handle)))
                    })
                    .collect::<Option<Box<[_]>>>()
                else {
                    return;
                };
                Row {
                    body: body.clone(),
                    declaration,
                    items: None,
                    files,
                }
            }
        };
        self.0.insert(id, row);
    }
    fn retained_bytes(&self) -> u64 {
        let mut weight = hirsel_query_products::retained::MetadataWeight::default();
        weight.add(size_of::<Carried>() + self.0.capacity() * (size_of::<(DefId, Row)>() + 8));
        // A witness outlives its object as that object's Arc allocation (the
        // inline value and two counts, no payload), until the next view.
        let arc = |inline: usize| inline + 2 * size_of::<usize>();
        for row in self.0.values() {
            let items = row.items.as_deref().unwrap_or_default();
            weight.add(
                size_of_val(items)
                    + items.len() * arc(size_of::<ItemSyntax>())
                    + size_of_val(&*row.files)
                    + row.files.len() * arc(size_of::<SourceFile>()),
            );
        }
        weight.bytes()
    }
}
