//! Code and source-neutral rows share one native cache selection and envelope.
use crate::{Store, decode, encode, native};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::source::NativeObject;
use serde::{Deserialize, Serialize};
use std::io;

pub(crate) const SCHEMA: u32 = 2;
#[derive(Serialize, Deserialize)]
struct Root {
    schema: u32,
    object: ContentHash,
    debug: Option<ContentHash>,
}

pub(crate) fn publish(
    store: &Store,
    key: ContentHash,
    object: &NativeObject,
) -> io::Result<ContentHash> {
    let object_identity = store.put_bare(&object.bytes)?;
    let debug = object
        .debug
        .as_ref()
        .map(|debug| encode(debug).and_then(|bytes| store.put(&bytes)))
        .transpose()?;
    let root = store.put(&encode(&Root {
        schema: SCHEMA,
        object: object_identity,
        debug,
    })?)?;
    store.select(&native::selection(key), root)?;
    Ok(object_identity)
}

pub(crate) fn path(store: &Store, identity: ContentHash) -> std::path::PathBuf {
    store.path(identity)
}

pub(crate) fn load(
    store: &Store,
    key: ContentHash,
) -> io::Result<Option<(ContentHash, NativeObject)>> {
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
    let Some(bytes) = store.get_bare(root.object)? else {
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
    Ok(Some((root.object, NativeObject { bytes, debug })))
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
            let identity = publish(&store, key, &object).unwrap();
            let (loaded, warm) = load(&store, key).unwrap().unwrap();
            assert_eq!(identity, loaded);
            assert_eq!(object, warm);
            let bytes = store.get_bare(loaded).unwrap().unwrap();
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
