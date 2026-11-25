use super::*;

/// Each stored kind of a content store against its hand-built frame: a record
/// is `HCORE VERSION origin payload len check address`, a selection `address
/// check`, and a blob its plain bytes under the checksum its referrer holds
/// (each check the xxh3-64 of what precedes it). Every flipped byte, the
/// checksum field's included, every power-loss shape (short, zero-filled,
/// missing) and another blob's whole bytes at its name read as nothing; the
/// whole frame reads back. An executable is a blob: its image damaged, or
/// swapped for its own debug file's bytes, is a miss, so the build links
/// again rather than run it. Bug: a read that skips the blob check.
#[test]
fn damaged_disk_bytes_are_a_miss() {
    use sha2::Digest;
    use twox_hash::XxHash3_64;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::cas(dir.path()).unwrap();
    let payload = b"published payload";
    let mut record = b"HCORE\0\0\0".to_vec();
    record.extend(crate::store::VERSION.to_le_bytes());
    record.extend([0; 32]);
    record.extend(payload);
    record.extend((payload.len() as u64).to_le_bytes());
    let key = ContentHash(sha2::Sha256::digest(&record).into());
    record.extend(XxHash3_64::oneshot(&record).to_le_bytes());
    record.extend(key.0);
    assert_eq!(store.put(payload).unwrap(), key);
    assert_eq!(std::fs::read(store.path(key)).unwrap(), record);
    let mut selection = key.0.to_vec();
    selection.extend(b"live");
    selection.extend(XxHash3_64::oneshot(&selection).to_le_bytes());
    store.select("live", key).unwrap();
    let live = store.selection_path("live");
    assert_eq!(std::fs::read(&live).unwrap(), selection);
    let object = b"\x7fELF object bytes".as_slice();
    let bare = store.put_blob(object).unwrap();
    assert_eq!(
        bare.address.0,
        <[u8; 32]>::from(sha2::Sha256::digest(object))
    );
    assert_eq!(bare.check, XxHash3_64::oneshot(object));
    let other = b"\x7fELF other bytes".as_slice();
    store.put_blob(other).unwrap();
    let image = b"\x7fELF linked image".as_slice();
    let debug = b"\x7fELF detached debug".as_slice();
    let link_key = ContentHash::of_bytes(b"link key");
    crate::native_debug::NativeImage::new(image.to_vec().into(), debug.to_vec())
        .publish(&store, link_key)
        .unwrap();
    let swaps = |kind: &str| match kind {
        "blob" => Some(other),
        "executable" => Some(debug),
        _ => None,
    };
    let kinds: [(&str, PathBuf, &[u8], &dyn Fn() -> bool); 4] = [
        ("record", store.path(key), &record, &|| {
            store.get(key).unwrap().is_some()
        }),
        ("selection", live, &selection, &|| {
            store.selection("live").unwrap() == Some(key)
        }),
        ("blob", store.blob_path(bare.address), object, &|| {
            store.get_blob(bare).unwrap().is_some()
        }),
        (
            "executable",
            store.blob_path(crate::store::Bare::of(image).address),
            image,
            &|| {
                // Any image at all is a hit: a damaged one that loads would
                // be run. The whole read's bytes are checked below.
                crate::native_debug::NativeImage::load(&store, link_key)
                    .unwrap()
                    .is_some()
            },
        ),
    ];
    for (kind, path, good, reads) in kinds {
        let write = |bytes: &[u8]| std::fs::write(&path, bytes).unwrap();
        for at in 0..good.len() {
            let mut damaged = good.to_vec();
            damaged[at] ^= 1;
            write(&damaged);
            assert!(!reads(), "{kind}: byte {at}");
        }
        for len in 0..good.len() {
            write(&good[..len]);
            assert!(!reads(), "{kind}: {len} bytes");
        }
        write(&vec![0; good.len()]);
        assert!(!reads(), "{kind}: zero-filled");
        if let Some(swapped) = swaps(kind) {
            write(swapped);
            assert!(!reads(), "{kind}: another blob's bytes");
        }
        std::fs::remove_file(&path).unwrap();
        assert!(!reads(), "{kind}: missing");
        write(good);
        assert!(reads(), "{kind}: whole");
    }
    assert_eq!(store.get(key).unwrap().unwrap(), payload);
    assert_eq!(store.get_blob(bare).unwrap().unwrap(), object);
    let loaded = crate::native_debug::NativeImage::load(&store, link_key)
        .unwrap()
        .unwrap();
    assert!(*loaded.bytes == image && loaded.debug == debug);
}
