//! An emitted object, its debug rows and the record naming them. The object
//! and its debug rows are blobs, named by the SHA-256 of their bytes, so
//! equal objects made under different keys are stored once; the record is
//! framed under the object's key and names each blob with its read check, so
//! a blob damaged or swapped for another's fails that check and is a miss.
//! A coverage object's map fragment is a record named from the object's key.
use crate::store::Bare;
use crate::{Store, decode, encode};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::source::{NativeDebug, NativeObject};
use serde::{Deserialize, Serialize};
use std::io;

pub(crate) const SCHEMA: u32 = 4;
#[derive(Serialize, Deserialize)]
struct Root {
    schema: u32,
    object: Bare,
    debug: Option<Bare>,
}

const DEBUG_SCHEMA: u32 = 2;
/// The name of the coverage map fragment stored with the object whose
/// record is under `key`.
fn coverage_name(key: ContentHash) -> ContentHash {
    ContentHash::of_parts("hirsel.core-object-coverage.v2", [&key.0[..]])
}
const COVERAGE_SCHEMA: u32 = 1;

/// Store `native`'s object and debug rows as blobs, and its coverage
/// fragment under `key`'s name.
fn put_parts(
    store: &Store,
    key: ContentHash,
    native: &NativeObject,
) -> io::Result<(Bare, Option<Bare>)> {
    let object = store.put_blob(&native.bytes)?;
    let debug = match &native.debug {
        Some(debug) => Some(store.put_blob(&encode(&(DEBUG_SCHEMA, debug))?)?),
        None => None,
    };
    // A coverage object's fragment is stored before its record, so a record
    // under a coverage key always has one.
    if let Some(fragment) = &native.coverage {
        put_coverage(store, key, fragment)?;
    }
    Ok((object, debug))
}

/// Store `fragment` as the map fragment of the coverage object whose record
/// is under `key`, beside the object ([`join_coverage`]).
fn put_coverage(
    store: &Store,
    key: ContentHash,
    fragment: &hirsel_core_ir::coverage::Fragment,
) -> io::Result<()> {
    store.put_keyed(coverage_name(key), &encode(&(COVERAGE_SCHEMA, fragment))?)?;
    Ok(())
}

/// The map fragment stored with the coverage object whose record is under
/// `key` ([`crate::native::ObjectProduct::key`]), when it checks.
fn load_coverage(
    store: &Store,
    key: ContentHash,
) -> io::Result<Option<hirsel_core_ir::coverage::Fragment>> {
    Ok(store
        .get(coverage_name(key))?
        .and_then(|bytes| decode::<(u32, hirsel_core_ir::coverage::Fragment)>(&bytes))
        .and_then(|(schema, fragment)| (schema == COVERAGE_SCHEMA).then_some(fragment)))
}

/// A coverage link's site map: the fragment of each linked object, read
/// from the store that holds it (`objects`: each object's store and record
/// key), in link order, keeping the first record of each id. Records of one
/// id have the same sites; only the code variants' arm positions can differ,
/// so the first in link order is the one kept. An object with no fragment in
/// its store (a plain object, or one held in no store) adds no record, and a
/// raw record of its id is refused when the file is read.
pub(crate) fn join_coverage<'s>(
    objects: impl Iterator<Item = (Option<&'s Store>, ContentHash)>,
) -> io::Result<hirsel_core_ir::coverage::Fragment> {
    let mut map = hirsel_core_ir::coverage::Fragment::default();
    let mut seen = std::collections::BTreeSet::new();
    for (store, key) in objects {
        let Some(store) = store else {
            continue;
        };
        for record in load_coverage(store, key)?
            .into_iter()
            .flat_map(|f| f.records)
        {
            if seen.insert(record.id) {
                map.records.push(record);
            }
        }
    }
    Ok(map)
}

/// Publish an emitted object and its debug rows with their record under
/// `key`: the object.
pub(crate) fn publish(store: &Store, key: ContentHash, native: &NativeObject) -> io::Result<Bare> {
    let (object, debug) = put_parts(store, key, native)?;
    store.put_keyed(
        key,
        &encode(&Root {
            schema: SCHEMA,
            object,
            debug,
        })?,
    )?;
    Ok(object)
}

/// The file of the object blob named `address`.
pub(crate) fn path(store: &Store, address: ContentHash) -> std::path::PathBuf {
    store.object_path(address)
}

/// The object stored under `key` when every part checks; the link hands its
/// checked bytes to the linker.
pub(crate) fn load(store: &Store, key: ContentHash) -> io::Result<Option<(Bare, NativeObject)>> {
    match store.get(key)? {
        Some(payload) => load_payload(store, &payload),
        None => Ok(None),
    }
}

/// The object a record's `payload` names, its blobs read from `store`: a
/// part record, local or a listed builder's (`crate::remote`).
pub(crate) fn load_payload(
    store: &Store,
    payload: &[u8],
) -> io::Result<Option<(Bare, NativeObject)>> {
    let Some((object, debug)) = root(store, payload)? else {
        return Ok(None);
    };
    let Some(bytes) = store.get_blob(object)? else {
        return Ok(None);
    };
    Ok(Some((
        object,
        NativeObject {
            bytes,
            debug,
            coverage: None,
        },
    )))
}

/// The parts of the record under `key`, which are a function of the key:
/// the object blob it names and its decoded debug rows, when every part
/// checks.
fn record(store: &Store, key: ContentHash) -> io::Result<Option<(Bare, Option<NativeDebug>)>> {
    match store.get(key)? {
        Some(bytes) => root(store, &bytes),
        None => Ok(None),
    }
}
fn root(store: &Store, bytes: &[u8]) -> io::Result<Option<(Bare, Option<NativeDebug>)>> {
    let Some(root) = decode::<Root>(bytes).filter(|root| root.schema == SCHEMA) else {
        return Ok(None);
    };
    let debug = match root.debug {
        Some(rows) => match load_debug(store, rows)? {
            Some(debug) => Some(debug),
            None => return Ok(None),
        },
        None => None,
    };
    Ok(Some((root.object, debug)))
}

/// A stored record's key-determined parts ([`record`]) and its object's
/// checked bytes, shared by a resident compiler's requests.
pub(crate) type Shared = (
    Bare,
    Option<std::sync::Arc<NativeDebug>>,
    std::sync::Arc<[u8]>,
);

/// [`load`] with its object read and checked: a resident compiler remembers
/// each record it read with its object's bytes, which are a function of the
/// record's key alone, whichever store held them, and reads neither again.
pub(crate) fn load_shared(store: &Store, key: ContentHash) -> io::Result<Option<Shared>> {
    let shared = crate::resident::OBJECT_RECORDS.get_or(Some(key), || {
        let (object, debug) = match record(store, key) {
            Ok(Some(found)) => found,
            Ok(None) => return Err(None),
            Err(error) => return Err(Some(error)),
        };
        match store.get_blob(object) {
            Ok(Some(bytes)) => Ok((object, debug.map(std::sync::Arc::new), bytes.into())),
            Ok(None) => Err(None),
            Err(error) => Err(Some(error)),
        }
    });
    match shared {
        Ok(shared) => Ok(Some(shared)),
        Err(None) => Ok(None),
        Err(Some(error)) => Err(error),
    }
}

/// A Cranelift development instance's record, under its instance key
/// (`hirsel.core-instance.v2` over the environment, the instance's identity
/// and its template body record's address, or none for glue). Every field a
/// hit reads is a function of the key: the object and its debug rows are the
/// emission of the materialised module, and `children`, `items` and
/// `closures` are read from that module (`hirsel_core_materialize::children`
/// and `Module::items`).
#[derive(Serialize, Deserialize)]
pub(crate) struct Instance {
    pub schema: u32,
    pub identity: hirsel_core_ir::Identity,
    pub own: Option<ContentHash>,
    /// The module's PROG-10 body hash, which a hit's revision reads.
    pub body: ContentHash,
    /// The digest of the encoded environment the key was made with.
    pub env: ContentHash,
    pub object: Bare,
    pub debug: Option<Bare>,
    /// The instances the module calls outside itself, sorted and deduplicated.
    pub children: Vec<hirsel_core_ir::Identity>,
    /// Exact PROG-10 slot order, including symbolic builtins.
    pub slots: Vec<hirsel_core_ir::Identity>,
    pub signatures: crate::instances::CodeSignatures,
    pub adapters: Vec<crate::instances::Adapter>,
    /// The instances the module describes (`revision::described`).
    pub described: Vec<hirsel_core_ir::Identity>,
    pub items: Vec<hirsel_core_ids::DefPath>,
    /// Every closure a child's arguments name, deduplicated, by hash.
    pub closures: Vec<std::sync::Arc<hirsel_core_ir::Closure>>,
}
pub(crate) const INSTANCE_SCHEMA: u32 = 6;

/// Publish an emitted instance's object, debug rows and record under its
/// key; the object and the debug rows' blobs.
pub(crate) fn publish_instance(
    store: &Store,
    key: ContentHash,
    object: &NativeObject,
    mut record: Instance,
) -> io::Result<(Bare, Option<Bare>)> {
    let (bare, debug) = put_parts(store, key, object)?;
    record.object = bare;
    record.debug = debug;
    store.put_keyed(key, &encode(&record)?)?;
    Ok((record.object, record.debug))
}

/// The record stored under `key`, when it decodes and every closure's hash
/// checks against its rows. Anything else is a miss; the blobs it names are
/// checked when they are read.
pub(crate) fn load_instance(store: &Store, key: ContentHash) -> io::Result<Option<Instance>> {
    Ok(store.get(key)?.as_deref().and_then(instance))
}

/// The instance record `payload` holds, local or a listed builder's
/// (`crate::remote`), when it decodes and every closure's hash checks.
pub(crate) fn instance(payload: &[u8]) -> Option<Instance> {
    let record = decode::<Instance>(payload)?;
    let sound = record.schema == INSTANCE_SCHEMA
        && record
            .closures
            .iter()
            .all(|c| hirsel_core_ir::Closure::new(c.rows.clone()).hash == c.hash);
    sound.then_some(record)
}

/// The blobs a part record's (`instance` false) or an instance record's
/// payload names, which travel with it to a remote cache.
pub(crate) fn blobs(payload: &[u8], instance: bool) -> Option<Vec<Bare>> {
    let (object, debug) = match instance {
        true => self::instance(payload).map(|r| (r.object, r.debug))?,
        false => decode::<Root>(payload)
            .filter(|root| root.schema == SCHEMA)
            .map(|root| (root.object, root.debug))?,
    };
    Some([object].into_iter().chain(debug).collect())
}

/// Stored debug rows by their blob; a damaged or foreign blob is none.
pub(crate) fn load_debug(store: &Store, rows: Bare) -> io::Result<Option<NativeDebug>> {
    Ok(store
        .get_blob(rows)?
        .and_then(|bytes| decode::<(u32, NativeDebug)>(&bytes))
        .filter(|(schema, _)| *schema == DEBUG_SCHEMA)
        .map(|(_, debug)| debug))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ir::source::*;
    fn swap(a: &std::path::Path, b: &std::path::Path) {
        let held = a.with_extension("swap");
        std::fs::rename(a, &held).unwrap();
        std::fs::rename(b, a).unwrap();
        std::fs::rename(&held, b).unwrap();
    }
    /// Law: a record, object blob or debug-row blob swapped for another
    /// key's, or a blob whose bytes are damaged, is a miss: an object record
    /// or instance record never reads through to bytes that are not the
    /// ones it was published with, and publishing again makes it a hit.
    /// Frames carry their key and schema; a blob's check travels in the
    /// record that names it. Bug: a blob read that skipped the check (or a
    /// record read that skipped the frame's key) would hand the linker the
    /// other object's bytes.
    #[test]
    fn records_stored_under_each_others_names_are_misses() {
        let object = |bytes: &[u8]| NativeObject {
            bytes: bytes.to_vec(),
            debug: Some(NativeDebug {
                items: vec![hirsel_core_ids::DefPath::core(
                    std::str::from_utf8(bytes).unwrap(),
                )],
                sections: vec![],
                sequences: vec![],
                scopes: vec![],
            }),
            coverage: None,
        };
        let instance = |name: &str| Instance {
            schema: INSTANCE_SCHEMA,
            identity: hirsel_core_ir::Identity::plain(hirsel_core_ids::DefPath::core(name)),
            own: None,
            body: ContentHash::of_bytes(b"body"),
            env: ContentHash::of_bytes(b"env"),
            object: Default::default(),
            debug: None,
            children: vec![],
            slots: vec![],
            signatures: Default::default(),
            adapters: vec![],
            described: vec![],
            items: vec![],
            closures: vec![],
        };
        let (a, b) = (ContentHash::of_bytes(b"a"), ContentHash::of_bytes(b"b"));
        let fresh = || {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::cas(dir.path()).unwrap();
            (dir, store)
        };
        type Part = fn(&Store, ContentHash, (Bare, Option<Bare>)) -> std::path::PathBuf;
        let kinds: [(&str, Part); 3] = [
            ("record", |s, k, _| s.path(k)),
            ("object blob", |s, _, (o, _)| s.blob_path(o.address)),
            ("debug blob", |s, _, (_, d)| s.blob_path(d.unwrap().address)),
        ];
        let damage = |path: &std::path::Path| {
            let mut bytes = std::fs::read(path).unwrap();
            bytes[0] ^= 1;
            std::fs::write(path, bytes).unwrap();
        };
        let read_instance = |store: &Store, key| {
            load_instance(store, key).unwrap().is_some_and(|r| {
                store.get_blob(r.object).unwrap().is_some()
                    && load_debug(store, r.debug.unwrap()).unwrap().is_some()
            })
        };
        for (kind, part) in kinds {
            // Object records: the link reads the object and debug rows.
            let (_dir, store) = fresh();
            let parts = |key, bytes: &[u8]| {
                let object = object(bytes);
                let bare = publish(&store, key, &object).unwrap();
                (
                    bare,
                    Some(
                        store
                            .put_blob(&encode(&(DEBUG_SCHEMA, object.debug.unwrap())).unwrap())
                            .unwrap(),
                    ),
                )
            };
            let (pa, pb) = (parts(a, b"a"), parts(b, b"b"));
            swap(&part(&store, a, pa), &part(&store, b, pb));
            assert!(
                load(&store, a).unwrap().is_none(),
                "object {kind} a swapped"
            );
            assert!(
                load(&store, b).unwrap().is_none(),
                "object {kind} b swapped"
            );
            damage(&part(&store, a, pa));
            assert!(
                load(&store, a).unwrap().is_none(),
                "object {kind} a damaged"
            );
            publish(&store, a, &object(b"a")).unwrap();
            assert!(load(&store, a).unwrap().is_some(), "object a rebuilt");
            // Instance records: a hit names the object and debug rows, which
            // the link reads through the record's checks.
            let (_dir, store) = fresh();
            let pa = publish_instance(&store, a, &object(b"a"), instance("a")).unwrap();
            let pb = publish_instance(&store, b, &object(b"b"), instance("b")).unwrap();
            swap(&part(&store, a, pa), &part(&store, b, pb));
            for key in [a, b] {
                assert!(!read_instance(&store, key), "instance {kind} {key:?}");
            }
            publish_instance(&store, a, &object(b"a"), instance("a")).unwrap();
            damage(&part(&store, a, pa));
            assert!(!read_instance(&store, a), "instance {kind} damaged");
        }
    }
    #[test]
    fn native_object_caches_unchanged_code_and_optional_rows_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::cas(dir.path()).unwrap();
        for enabled in [false, true] {
            let key = ContentHash::of_bytes(if enabled { b"on" } else { b"off" });
            let object = NativeObject {
                bytes: b"unchanged ELF bytes".to_vec(),
                debug: enabled.then(|| NativeDebug {
                    items: vec![hirsel_core_ids::DefPath::core("rows")],
                    sections: vec![CodeSection {
                        symbol: debug_symbol(key, 0),
                        size: 1,
                    }],
                    sequences: vec![LineSequence {
                        range: CodeRange {
                            section: 0,
                            begin: 0,
                            end: 1,
                        },
                        rows: vec![LineRow {
                            offset: 0,
                            source: Some(NativeLocation { item: 0, start: 7 }),
                        }],
                    }],
                    scopes: vec![],
                }),
                coverage: None,
            };
            let identity = publish(&store, key, &object).unwrap();
            let (loaded, warm) = load(&store, key).unwrap().unwrap();
            assert_eq!(identity, loaded);
            assert_eq!(object, warm);
            let bytes = std::fs::read(path(&store, loaded.address)).unwrap();
            assert_eq!(bytes, object.bytes);
            publish(&store, key, &object).unwrap();
            assert_eq!(load(&store, key).unwrap().unwrap().1, object);
        }
    }
}
