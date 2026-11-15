//! Code and source-neutral rows share one native cache selection and envelope.
use crate::store::Bare;
use crate::{Store, decode, encode, native};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::source::NativeObject;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io;

pub(crate) const SCHEMA: u32 = 2;
#[derive(Serialize, Deserialize)]
struct Root {
    schema: u32,
    object: Bare,
    debug: Option<ContentHash>,
}

/// Publish an emitted object and its debug rows and select them under `key`:
/// the object, and the descriptor its bytes were written through.
pub(crate) fn publish(
    store: &Store,
    key: ContentHash,
    native: &NativeObject,
) -> io::Result<(Bare, File)> {
    let (object, file) = store.put_bare(&native.bytes)?;
    let debug = native
        .debug
        .as_ref()
        .map(|debug| encode(debug).and_then(|bytes| store.put(&bytes)))
        .transpose()?;
    let root = store.put(&encode(&Root {
        schema: SCHEMA,
        object,
        debug,
    })?)?;
    store.select(&native::selection(key), root)?;
    Ok((object, file))
}

pub(crate) fn path(store: &Store, identity: ContentHash) -> std::path::PathBuf {
    store.path(identity)
}

/// The object selected under `key` when every part checks, with the
/// descriptor its bytes were checked through; the link hands that descriptor
/// to the linker.
pub(crate) fn load(
    store: &Store,
    key: ContentHash,
) -> io::Result<Option<(Bare, NativeObject, File)>> {
    let Some(identity) = store.selection(&native::selection(key))? else {
        return Ok(None);
    };
    let Some(bytes) = store.get(identity)? else {
        return Ok(None);
    };
    let Some(root) = decode::<Root>(&bytes) else {
        return Ok(None);
    };
    if root.schema != SCHEMA {
        return Ok(None);
    }
    let Some((bytes, file)) = store.get_bare(root.object)? else {
        return Ok(None);
    };
    let debug = match root.debug {
        Some(identity) => {
            let Some(bytes) = store.get(identity)? else {
                return Ok(None);
            };
            let Some(debug) = decode(&bytes) else {
                return Ok(None);
            };
            Some(debug)
        }
        None => None,
    };
    Ok(Some((root.object, NativeObject { bytes, debug }, file)))
}

/// A Cranelift development instance's record, under its instance key
/// (`hirsel.core-instance.v1` over the environment, the instance's identity
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
    /// The digest of the encoded environment the key was made with.
    pub env: ContentHash,
    pub object: Bare,
    pub debug: Option<ContentHash>,
    /// The instances the module calls outside itself, sorted and deduplicated.
    pub children: Vec<hirsel_core_ir::Identity>,
    pub items: Vec<hirsel_core_ids::DefPath>,
    /// Every closure a child's arguments name, deduplicated, by hash.
    pub closures: Vec<std::sync::Arc<hirsel_core_ir::Closure>>,
}
pub(crate) const INSTANCE_SCHEMA: u32 = 1;

/// Publish an emitted instance's object, debug rows and record, and select the
/// record under its key; the object, the debug rows' address and the
/// descriptor the object's bytes were written through.
pub(crate) fn publish_instance(
    store: &Store,
    key: ContentHash,
    object: &NativeObject,
    mut record: Instance,
) -> io::Result<(Bare, Option<ContentHash>, File)> {
    let (bare, file) = store.put_bare(&object.bytes)?;
    record.object = bare;
    record.debug = object
        .debug
        .as_ref()
        .map(|debug| encode(debug).and_then(|bytes| store.put(&bytes)))
        .transpose()?;
    let address = store.put(&encode(&record)?)?;
    store.select(&native::selection(key), address)?;
    Ok((record.object, record.debug, file))
}

/// The record selected under `key`, when it decodes and every closure's hash
/// checks against its rows. Anything else is a miss.
pub(crate) fn load_instance(store: &Store, key: ContentHash) -> io::Result<Option<Instance>> {
    let Some(address) = store.selection(&native::selection(key))? else {
        return Ok(None);
    };
    let Some(bytes) = store.get(address)? else {
        return Ok(None);
    };
    let Some(record) = decode::<Instance>(&bytes) else {
        return Ok(None);
    };
    let sound = record.schema == INSTANCE_SCHEMA
        && record
            .closures
            .iter()
            .all(|c| hirsel_core_ir::Closure::new(c.rows.clone()).hash == c.hash);
    Ok(sound.then_some(record))
}

/// Stored debug rows by their record's address; a damaged record is none.
pub(crate) fn load_debug(
    store: &Store,
    address: ContentHash,
) -> io::Result<Option<hirsel_core_ir::source::NativeDebug>> {
    Ok(store.get(address)?.and_then(|bytes| decode(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ir::source::*;
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
            };
            let (identity, _) = publish(&store, key, &object).unwrap();
            let (loaded, warm, _) = load(&store, key).unwrap().unwrap();
            assert_eq!(identity, loaded);
            assert_eq!(object, warm);
            let bytes = std::fs::read(path(&store, loaded.address)).unwrap();
            assert_eq!(bytes, object.bytes);
            let old = store.put(&object.bytes).unwrap();
            store.select(&native::selection(key), old).unwrap();
            assert!(
                load(&store, key).unwrap().is_none(),
                "no old object-only representation"
            );
            publish(&store, key, &object).unwrap();
            assert_eq!(load(&store, key).unwrap().unwrap().1, object);
        }
    }
}
