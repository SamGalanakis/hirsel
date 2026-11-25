//! An emitted object, its debug rows and the record naming them, all stored
//! under names derived from the object's key: the record framed under the key
//! itself, the object bare under [`object_name`] and the debug rows framed
//! under [`debug_name`]. Every frame carries its key and a schema that its
//! reader compares, and the object's checksum travels in the record, so a
//! record, debug row or object moved under another key's name is a miss.
use crate::store::Bare;
use crate::{Store, decode, encode};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::source::{NativeDebug, NativeObject};
use serde::{Deserialize, Serialize};
use std::io;

pub(crate) const SCHEMA: u32 = 3;
#[derive(Serialize, Deserialize)]
struct Root {
    schema: u32,
    object: Bare,
    debug: Option<ContentHash>,
}

/// The name of the bare object stored for the record under `key`.
pub(crate) fn object_name(key: ContentHash) -> ContentHash {
    ContentHash::of_parts("hirsel.core-object-file.v1", [&key.0[..]])
}
/// The name of the debug rows stored for the record under `key`.
fn debug_name(key: ContentHash) -> ContentHash {
    ContentHash::of_parts("hirsel.core-object-debug.v1", [&key.0[..]])
}
const DEBUG_SCHEMA: u32 = 1;
/// The name of the coverage map fragment stored with the object named
/// `identity` ([`object_name`]), a function of the object's key that a link
/// holding the object's identity can derive.
fn coverage_name(identity: ContentHash) -> ContentHash {
    ContentHash::of_parts("hirsel.core-object-coverage.v1", [&identity.0[..]])
}
const COVERAGE_SCHEMA: u32 = 1;

/// Store `native`'s object and debug rows under the names `key` derives: the
/// object and the debug rows' name.
fn put_parts(
    store: &Store,
    key: ContentHash,
    native: &NativeObject,
) -> io::Result<(Bare, Option<ContentHash>)> {
    let object = store.put_bare(object_name(key), &native.bytes)?;
    let debug = match &native.debug {
        Some(debug) => {
            let name = debug_name(key);
            store.put_keyed(name, &encode(&(DEBUG_SCHEMA, debug))?)?;
            Some(name)
        }
        None => None,
    };
    // A coverage object's fragment is stored before its record, so a record
    // under a coverage key always has one. The record does not name it: a
    // plain record's bytes are the bytes it had before coverage.
    if let Some(fragment) = &native.coverage {
        store.put_keyed(
            coverage_name(object.address),
            &encode(&(COVERAGE_SCHEMA, fragment))?,
        )?;
    }
    Ok((object, debug))
}

/// The map fragment stored with the coverage object named `identity`
/// ([`crate::native::ObjectProduct::identity`]), when it checks.
pub(crate) fn load_coverage(
    store: &Store,
    identity: ContentHash,
) -> io::Result<Option<hirsel_core_ir::coverage::Fragment>> {
    Ok(store
        .get(coverage_name(identity))?
        .and_then(|bytes| decode::<(u32, hirsel_core_ir::coverage::Fragment)>(&bytes))
        .and_then(|(schema, fragment)| (schema == COVERAGE_SCHEMA).then_some(fragment)))
}

/// A coverage link's site map: the fragment of each linked object named in
/// `identities`, in link order, keeping the first record of each id. Records
/// of one id have the same sites; only the code variants' arm positions can
/// differ, so the first in link order is the one kept. An object with no
/// fragment here (one from an image store) adds no record, and a raw record
/// of its id is refused when the file is read.
pub(crate) fn join_coverage(
    store: &Store,
    identities: impl Iterator<Item = ContentHash>,
) -> io::Result<hirsel_core_ir::coverage::Fragment> {
    let mut map = hirsel_core_ir::coverage::Fragment::default();
    let mut seen = std::collections::BTreeSet::new();
    for identity in identities {
        for record in load_coverage(store, identity)?
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

pub(crate) fn path(store: &Store, identity: ContentHash) -> std::path::PathBuf {
    store.path(identity)
}

/// The object stored under `key` when every part checks; the link hands its
/// checked bytes to the linker.
pub(crate) fn load(store: &Store, key: ContentHash) -> io::Result<Option<(Bare, NativeObject)>> {
    let Some((object, debug)) = record(store, key)? else {
        return Ok(None);
    };
    let Some(bytes) = store.get_bare(object)? else {
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

/// The parts of the record under `key` that are a function of the key: the
/// bare object it names and its decoded debug rows, when every part checks.
fn record(store: &Store, key: ContentHash) -> io::Result<Option<(Bare, Option<NativeDebug>)>> {
    let Some(bytes) = store.get(key)? else {
        return Ok(None);
    };
    let Some(root) = decode::<Root>(&bytes) else {
        return Ok(None);
    };
    if root.schema != SCHEMA
        || root.object.address != object_name(key)
        || root.debug.is_some_and(|d| d != debug_name(key))
    {
        return Ok(None);
    }
    let debug = match root.debug {
        Some(name) => match load_debug(store, name)? {
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
/// record's key, and reads neither again.
pub(crate) fn load_shared(store: &Store, key: ContentHash) -> io::Result<Option<Shared>> {
    let shared =
        crate::resident::OBJECT_RECORDS.get_or(Some((store.root().to_owned(), key)), || {
            let (object, debug) = match record(store, key) {
                Ok(Some(found)) => found,
                Ok(None) => return Err(None),
                Err(error) => return Err(Some(error)),
            };
            match store.get_bare(object) {
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
    pub debug: Option<ContentHash>,
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
pub(crate) const INSTANCE_SCHEMA: u32 = 5;

/// Publish an emitted instance's object, debug rows and record under its
/// key; the object and the debug rows' name.
pub(crate) fn publish_instance(
    store: &Store,
    key: ContentHash,
    object: &NativeObject,
    mut record: Instance,
) -> io::Result<(Bare, Option<ContentHash>)> {
    let (bare, debug) = put_parts(store, key, object)?;
    record.object = bare;
    record.debug = debug;
    store.put_keyed(key, &encode(&record)?)?;
    Ok((record.object, record.debug))
}

/// The record stored under `key`, when it decodes, names its key's object and
/// debug rows, and every closure's hash checks against its rows. Anything
/// else is a miss.
pub(crate) fn load_instance(store: &Store, key: ContentHash) -> io::Result<Option<Instance>> {
    let Some(bytes) = store.get(key)? else {
        return Ok(None);
    };
    let Some(record) = decode::<Instance>(&bytes) else {
        return Ok(None);
    };
    let sound = record.schema == INSTANCE_SCHEMA
        && record.object.address == object_name(key)
        && record.debug.is_none_or(|d| d == debug_name(key))
        && record
            .closures
            .iter()
            .all(|c| hirsel_core_ir::Closure::new(c.rows.clone()).hash == c.hash);
    Ok(sound.then_some(record))
}

/// Stored debug rows by their name; a damaged or foreign record is none.
pub(crate) fn load_debug(store: &Store, name: ContentHash) -> io::Result<Option<NativeDebug>> {
    Ok(store
        .get(name)?
        .and_then(|bytes| decode::<(u32, NativeDebug)>(&bytes))
        .filter(|(schema, _)| *schema == DEBUG_SCHEMA)
        .map(|(_, debug)| debug))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ir::source::*;
    fn swap(store: &Store, a: ContentHash, b: ContentHash) {
        let held = store.path(a).with_extension("swap");
        std::fs::rename(store.path(a), &held).unwrap();
        std::fs::rename(store.path(b), store.path(a)).unwrap();
        std::fs::rename(&held, store.path(b)).unwrap();
    }
    /// Each kind stored under a key's name, moved under another key's name,
    /// is a miss: an object record, an instance record, a bare object and
    /// debug rows. Frames carry their key and schema; a bare object's check
    /// travels in the record that names it.
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
            let store = Store::new(dir.path()).unwrap();
            (dir, store)
        };
        type Name = fn(ContentHash) -> ContentHash;
        let kinds: [(&str, Name); 3] = [
            ("record", |k| k),
            ("bare object", object_name),
            ("debug rows", debug_name),
        ];
        for (kind, name) in kinds {
            let (_dir, store) = fresh();
            // Object records: the link reads the object and debug rows.
            publish(&store, a, &object(b"a")).unwrap();
            publish(&store, b, &object(b"b")).unwrap();
            swap(&store, name(a), name(b));
            assert!(load(&store, a).unwrap().is_none(), "object {kind} a");
            assert!(load(&store, b).unwrap().is_none(), "object {kind} b");
            // Instance records: a hit names the object and debug rows, which
            // the link reads through the record's check and name.
            let (_dir, store) = fresh();
            publish_instance(&store, a, &object(b"a"), instance("a")).unwrap();
            publish_instance(&store, b, &object(b"b"), instance("b")).unwrap();
            swap(&store, name(a), name(b));
            for key in [a, b] {
                let read = load_instance(&store, key).unwrap().is_some_and(|r| {
                    store.get_bare(r.object).unwrap().is_some()
                        && load_debug(&store, r.debug.unwrap()).unwrap().is_some()
                });
                assert!(!read, "instance {kind} {key:?}");
            }
        }
        // An object record naming another key's debug rows, each stored
        // soundly under its own name, links rows of another object.
        let (_dir, store) = fresh();
        let object_a = publish(&store, a, &object(b"a")).unwrap();
        publish(&store, b, &object(b"b")).unwrap();
        let foreign = Root {
            schema: SCHEMA,
            object: object_a,
            debug: Some(debug_name(b)),
        };
        store.put_keyed(a, &encode(&foreign).unwrap()).unwrap();
        assert!(
            load(&store, a).unwrap().is_none(),
            "object a naming b's debug rows"
        );
    }
    #[test]
    fn native_object_caches_unchanged_code_and_optional_rows_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path()).unwrap();
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
