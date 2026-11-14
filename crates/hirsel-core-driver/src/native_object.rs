//! Code and source-neutral rows share one native cache selection and envelope.
use crate::store::Record;
use crate::{Store, decode, native};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::source::NativeObject;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

pub(crate) const SCHEMA: u32 = 1;
#[derive(Serialize, Deserialize)]
struct Root {
    schema: u32,
    object: Record,
    debug: Option<Record>,
}

pub(crate) fn publish(
    store: &Store,
    key: ContentHash,
    object: &NativeObject,
) -> io::Result<ContentHash> {
    let identity = store.archive(|writer| {
        writer.write_all(&object.bytes)?;
        let debug = object
            .debug
            .as_ref()
            .map(|d| writer.record(d))
            .transpose()?;
        let root = writer.record(&Root {
            schema: SCHEMA,
            object: Record {
                start: 0,
                len: object.bytes.len() as u64,
            },
            debug,
        })?;
        writer.write_all(&root.len.to_le_bytes())
    })?;
    store.select(&native::selection(key), identity)?;
    Ok(identity)
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
    Ok(read(&bytes).map(|object| (identity, object)))
}

fn read(bytes: &[u8]) -> Option<NativeObject> {
    let end = bytes.len().checked_sub(8)?;
    let size = usize::try_from(u64::from_le_bytes(bytes[end..].try_into().ok()?)).ok()?;
    let start = end.checked_sub(size)?;
    let root: Root = decode(&bytes[start..end])?;
    if root.schema != SCHEMA {
        return None;
    }
    let payload = &bytes[..start];
    Some(NativeObject {
        bytes: root.object.bytes(payload)?.to_vec(),
        debug: match root.debug {
            Some(d) => Some(decode(d.bytes(payload)?)?),
            None => None,
        },
    })
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
            let bytes = store.get(identity).unwrap().unwrap();
            for end in 0..bytes.len() {
                assert!(read(&bytes[..end]).is_none());
            }
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
