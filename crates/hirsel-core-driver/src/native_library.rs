//! One atomic native-library product, selected by the native object's key.
//! The store's envelope covers the public object, raw bitcode and summary.
use crate::store::Record;
use crate::{Store, decode, native};
use hirsel_core_ids::ContentHash;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Write};

// Fixed-size footer; the variable summary is decoded only after the complete
// product has been read through Store's version/length/digest boundary.
const ROOT_SIZE: usize = 52;
#[derive(Serialize, Deserialize)]
struct Root {
    schema: u32,
    object: Record,
    bitcode: Record,
    summary: Record,
}

pub(crate) struct Product<S> {
    pub object: Vec<u8>,
    pub bitcode: Vec<u8>,
    pub summary: S,
}

pub(crate) fn key(
    domain: &str,
    inputs: &impl Serialize,
    consumed: &[&[hirsel_core_ir::roles::BodyFacts]],
) -> io::Result<ContentHash> {
    native::key(
        domain,
        &(
            hirsel_llvm_lazy::NATIVE_LIBRARY_SCHEMA,
            hirsel_llvm_lazy::IMPORT_PIPELINE_ID,
            hirsel_core_ir::roles::FACTS_SCHEMA,
            consumed,
            inputs,
        ),
    )
}

pub(crate) fn publish<S: Serialize>(
    store: &Store,
    key: ContentHash,
    schema: u32,
    object: &[u8],
    bitcode: &[u8],
    summary: &S,
) -> io::Result<ContentHash> {
    let identity = store.archive(|writer| {
        // Raw byte sections avoid serde's per-byte sequence traversal.
        writer.write_all(object)?;
        writer.write_all(bitcode)?;
        let summary = writer.record(summary)?;
        writer.record(&Root {
            schema,
            object: Record {
                start: 0,
                len: object.len() as u64,
            },
            bitcode: Record {
                start: object.len() as u64,
                len: bitcode.len() as u64,
            },
            summary,
        })?;
        Ok(())
    })?;
    store.select(&native::selection(key), identity)?;
    Ok(identity)
}

pub(crate) fn load<S: DeserializeOwned>(
    store: &Store,
    key: ContentHash,
    schema: u32,
) -> io::Result<Option<(ContentHash, Product<S>)>> {
    let Some(identity) = store.selection(&native::selection(key))? else {
        return Ok(None);
    };
    let Some(bytes) = store.get(identity)? else {
        return Ok(None);
    };
    Ok(read(&bytes, schema).map(|product| (identity, product)))
}

pub(crate) fn read<S: DeserializeOwned>(bytes: &[u8], schema: u32) -> Option<Product<S>> {
    let at = bytes.len().checked_sub(ROOT_SIZE)?;
    let root: Root = decode(&bytes[at..])?;
    if root.schema != schema {
        return None;
    }
    let payload = &bytes[..at];
    Some(Product {
        object: root.object.bytes(payload)?.to_vec(),
        bitcode: root.bitcode.bytes(payload)?.to_vec(),
        summary: decode(root.summary.bytes(payload)?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const SCHEMA: u32 = 1;
    #[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Summary {
        selected: Vec<String>,
        facts: Vec<(bool, bool)>,
    }
    fn summary() -> Summary {
        Summary {
            selected: vec!["public_function".into()],
            facts: vec![(true, false)],
        }
    }
    #[test]
    fn native_library_publishes_all_parts_under_one_selection() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path()).unwrap();
        let key = ContentHash::of_bytes(b"existing native-object inputs and schema");
        let identity = publish(&store, key, SCHEMA, b"object", b"raw bitcode", &summary()).unwrap();
        let (loaded, product) = load::<Summary>(&store, key, SCHEMA).unwrap().unwrap();
        assert_eq!(loaded, identity);
        assert_eq!(product.object, b"object");
        assert_eq!(product.bitcode, b"raw bitcode");
        assert_eq!(product.summary, summary());
        let files = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(files, 2, "one archive and its existing native selection");
    }
    #[test]
    fn native_library_partial_foreign_and_old_products_are_misses() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path()).unwrap();
        let key = ContentHash::of_bytes(b"native unit");
        assert!(load::<Summary>(&store, key, SCHEMA).unwrap().is_none());
        let identity = publish(&store, key, SCHEMA, b"object", b"bitcode", &summary()).unwrap();
        assert!(load::<Summary>(&store, key, 2).unwrap().is_none());
        let complete = store.get(identity).unwrap().unwrap();
        for end in 0..complete.len() {
            assert!(read::<Summary>(&complete[..end], SCHEMA).is_none());
        }
        // An older raw object can still have a valid store envelope. It is not
        // adapted to a library product and cannot supply half of one.
        let old = store
            .put(b"old ELF object without bitcode or summary")
            .unwrap();
        store.select(&native::selection(key), old).unwrap();
        assert!(load::<Summary>(&store, key, SCHEMA).unwrap().is_none());
        store.select(&native::selection(key), identity).unwrap();
        let path = store.path(identity);
        let mut disk = std::fs::read(&path).unwrap();
        disk[20] ^= 1;
        std::fs::write(path, disk).unwrap();
        assert!(load::<Summary>(&store, key, SCHEMA).unwrap().is_none());
        // The next complete publication replaces the selection atomically.
        publish(
            &store,
            key,
            SCHEMA,
            b"new object",
            b"new raw bitcode",
            &summary(),
        )
        .unwrap();
        assert_eq!(
            load::<Summary>(&store, key, SCHEMA)
                .unwrap()
                .unwrap()
                .1
                .object,
            b"new object"
        );
    }

    #[test]
    fn consumed_role_facts_invalidate_library_and_application_keys() {
        use hirsel_core_ir::roles::{BodyFacts, ParamFacts};
        let original = vec![BodyFacts {
            code: ContentHash::of_bytes(b"callee code identity"),
            params: vec![ParamFacts {
                sealed: true,
                place_blind: true,
            }],
        }];
        for domain in ["hirsel.core-object.v1", "hirsel.core-release-unit.v2"] {
            let before = key(domain, &"unchanged body and signature", &[&original]).unwrap();
            assert_eq!(
                before,
                key(
                    domain,
                    &"unchanged body and signature",
                    &[&original.clone()]
                )
                .unwrap()
            );
            for field in 0..2 {
                let mut changed = original.clone();
                if field == 0 {
                    changed[0].params[0].sealed = false;
                } else {
                    changed[0].params[0].place_blind = false;
                }
                assert_ne!(
                    before,
                    key(domain, &"unchanged body and signature", &[&changed]).unwrap()
                );
            }
        }
    }
}
