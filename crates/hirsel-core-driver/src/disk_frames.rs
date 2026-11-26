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

/// A loopback HTTP cache: `GET`, `HEAD` and `PUT` of the paths in `files`,
/// one request per connection. Its base URL.
type Files = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>>;
fn remote_fixture(files: Files) -> String {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut reader = BufReader::new(stream.unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut words = line.split_whitespace();
            let (method, path) = (words.next().unwrap().to_owned(), words.next().unwrap());
            let path = path.trim_start_matches('/').to_owned();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut files = files.lock().unwrap();
            let (status, body) = match (method.as_str(), files.get(&path)) {
                ("PUT", _) => {
                    files.insert(path, body);
                    ("200 OK", Vec::new())
                }
                ("GET", Some(bytes)) => ("200 OK", bytes.clone()),
                ("HEAD", Some(_)) => ("200 OK", Vec::new()),
                _ => ("404 Not Found", Vec::new()),
            };
            let mut stream = reader.into_inner();
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            if method != "HEAD" {
                stream.write_all(&body).unwrap();
            }
        }
    });
    url
}

/// A remote cache's bytes and records against the signed-record frame
/// (`store::signed`): a part record signed by a listed key, whose object
/// blob the remote serves under its SHA-256, is a hit whose object is the
/// builder's bytes. A blob whose bytes are another's (same length), a record
/// with no signature or a zero one, one signed by a key the user did not
/// list (under its own id, or claiming the listed key's), and one validly
/// signed by the listed key for any kind tag but the requested one (no tag
/// exists for an executable or a test verdict, so tags 4 and 5 are among
/// them) are misses that keep no blob in `o/`, and no record in `r/` but
/// the listed key's record whose blob was refused. Bug: a read that
/// uses received bytes before hashing them, or a record before checking
/// its signer, signature and kind.
#[test]
fn remote_bytes_and_records_are_used_only_when_they_check() {
    use crate::store::remote::Remote;
    use crate::store::signed::{self, Builder, Kind};
    use hirsel_core_net::key_wrap;
    let listed = key_wrap::ed25519_generate();
    let unlisted = key_wrap::ed25519_generate();
    let builder = |pkcs8: &[u8]| Builder::new(key_wrap::ed25519_public(pkcs8).unwrap());
    let sign = |pkcs8: &[u8], signer: ContentHash, tag: u8, key, blobs: &[_], payload: &[u8]| {
        let mut bytes = signed::unsigned(Kind::Object, key, signer, blobs, payload);
        bytes[12] = tag;
        let message = signed::message(&bytes);
        let alg = key_wrap::SignAlg::Ed25519;
        bytes.extend(key_wrap::sign(alg, pkcs8, &message, key_wrap::SigningRandom::Host).unwrap());
        bytes
    };
    let source = tempfile::tempdir().unwrap();
    let source = Store::cas(source.path()).unwrap();
    let object = b"\x7fELF the builder's part".to_vec();
    let other = b"\x7fELF a stranger's parts".to_vec();
    assert_eq!(object.len(), other.len());
    let native = |bytes: &[u8]| hirsel_core_ir::source::NativeObject {
        bytes: bytes.to_vec(),
        debug: None,
        coverage: None,
    };
    let key = ContentHash::of_bytes(b"a release part's key");
    let bare = crate::native_object::publish(&source, key, &native(&object)).unwrap();
    let payload = source.get(key).unwrap().unwrap();
    let blobs = [(bare.address, object.len() as u64)];
    let id = builder(&listed).id;
    let good = sign(&listed, id, Kind::Object as u8, key, &blobs, &payload);
    let unsigned = signed::unsigned(Kind::Object, key, id, &blobs, &payload);
    // Each case is a fresh remote and a fresh local store: what the remote
    // serves at the record's and the blob's paths, and whether it is a hit.
    let case = |name: &str, signer: ContentHash, record: &[u8], blob: &[u8], hit: bool| {
        let kept_record = hit || blob != object;
        let files = Files::default();
        let record_path = format!("r/{signer}/{key}");
        files.lock().unwrap().insert(record_path, record.to_vec());
        let blob_path = format!("o/{}", bare.address);
        files.lock().unwrap().insert(blob_path, blob.to_vec());
        let url = remote_fixture(files);
        let trusted = hirsel_core_ids::hex::encode(&builder(&listed).public);
        let config = toml::from_str(&format!(
            "url = {url:?}\ntrusted = [{trusted:?}]\n[transfer]\nrecord-bytes = 4096\nblob-bytes = 1024\nwork-bytes = 1024"
        )).unwrap();
        let remote = Remote::new(config).unwrap().unwrap();
        let local = tempfile::tempdir().unwrap();
        let store = Store::cas(local.path()).unwrap();
        let read = remote.object(&store, key).unwrap();
        let blobs = crate::store::collect::blobs(local.path()).len();
        let records = std::fs::read_dir(local.path().join("signed")).map_or(0, |d| d.count());
        assert_eq!(records, kept_record as usize, "{name}: records kept");
        match hit {
            true => {
                let (read_bare, read) = read.unwrap_or_else(|| panic!("{name}: a miss"));
                assert!(read_bare == bare && read.bytes == object, "{name}");
                assert!(
                    store
                        .signed(Kind::Object, key, &[builder(&listed)], 4096)
                        .unwrap()
                        .is_some()
                );
            }
            false => assert!(read.is_none() && blobs == 0, "{name}: used or kept"),
        }
    };
    case("signed by the listed key", id, &good, &object, true);
    case("another blob's bytes", id, &good, &other, false);
    case("no signature", id, &unsigned, &object, false);
    case(
        "a zero signature",
        id,
        &[&unsigned[..], &[0; 64]].concat(),
        &object,
        false,
    );
    let stranger = builder(&unlisted).id;
    let by_stranger = sign(
        &unlisted,
        stranger,
        Kind::Object as u8,
        key,
        &blobs,
        &payload,
    );
    case("an unlisted signer", stranger, &by_stranger, &object, false);
    let claimed = sign(&unlisted, id, Kind::Object as u8, key, &blobs, &payload);
    case(
        "an unlisted key claiming the listed id",
        id,
        &claimed,
        &object,
        false,
    );
    for tag in (0..=u8::MAX).filter(|&tag| tag != Kind::Object as u8) {
        let forbidden = sign(&listed, id, tag, key, &blobs, &payload);
        case(&format!("kind tag {tag}"), id, &forbidden, &object, false);
    }
}

#[test]
fn remote_transfer_policy_bounds_both_directions() {
    use crate::store::{
        remote::{Config, Remote},
        signed::{Builder, Kind},
    };
    use hirsel_core_net::key_wrap;

    assert!(toml::from_str::<Config>("url = 'http://127.0.0.1'").is_err());
    let dir = tempfile::tempdir().unwrap();
    let pkcs8 = key_wrap::ed25519_generate();
    let signing_key = dir.path().join("builder.pk8");
    std::fs::write(&signing_key, &pkcs8).unwrap();
    let builder = Builder::new(key_wrap::ed25519_public(&pkcs8).unwrap());
    let trusted = hirsel_core_ids::hex::encode(&builder.public);
    let source = Store::cas(&dir.path().join("source")).unwrap();
    let object = b"\x7fELF object at its byte limit";
    let native = hirsel_core_ir::source::NativeObject {
        bytes: object.to_vec(),
        debug: None,
        coverage: None,
    };
    let key = ContentHash::of_bytes(b"one bounded remote object");
    let bare = crate::native_object::publish(&source, key, &native).unwrap();
    let payload = source.get(key).unwrap().unwrap();
    let record_bytes = 193 + payload.len(); // Header, one blob row, lengths and signature.
    let blob_bytes = object.len();
    let files = Files::default();
    let url = remote_fixture(files.clone());
    let remote = |record, blob, work| {
        let config = toml::from_str(&format!(
            "url = {url:?}\ntrusted = [{trusted:?}]\nsigning-key = {signing_key:?}\n[transfer]\nrecord-bytes = {record}\nblob-bytes = {blob}\nwork-bytes = {work}"
        )).unwrap();
        Remote::new(config)
    };
    assert!(remote(record_bytes, blob_bytes, blob_bytes - 1).is_err());

    for (record, blob) in [
        (record_bytes - 1, blob_bytes),
        (record_bytes, blob_bytes - 1),
    ] {
        let remote = remote(record, blob, blob).unwrap().unwrap();
        remote.offer(Kind::Object, &source, key);
        remote.finish_uploads();
        assert!(files.lock().unwrap().is_empty());
    }
    let exact = remote(record_bytes, blob_bytes, blob_bytes)
        .unwrap()
        .unwrap();
    exact.offer(Kind::Object, &source, key);
    exact.finish_uploads();
    let record_path = format!("r/{}/{key}", builder.id);
    let blob_path = format!("o/{}", bare.address);
    let record = files.lock().unwrap()[&record_path].clone();
    assert_eq!(record.len(), record_bytes);
    assert_eq!(files.lock().unwrap()[&blob_path], object);

    let local = Store::cas(&dir.path().join("reader")).unwrap();
    let too_big = |bytes: &[u8]| [bytes, &[0]].concat();
    files
        .lock()
        .unwrap()
        .insert(record_path.clone(), too_big(&record));
    assert!(exact.object(&local, key).unwrap().is_none());
    files.lock().unwrap().insert(record_path, record);
    files
        .lock()
        .unwrap()
        .insert(blob_path.clone(), too_big(object));
    assert!(exact.object(&local, key).unwrap().is_none());
    assert!(!local.blob_path(bare.address).exists());
    files.lock().unwrap().insert(blob_path, object.to_vec());
    assert_eq!(exact.object(&local, key).unwrap().unwrap().1.bytes, object);

    for (record, blob) in [
        (record_bytes - 1, blob_bytes),
        (record_bytes, blob_bytes - 1),
    ] {
        let remote = remote(record, blob, blob).unwrap().unwrap();
        assert!(remote.object(&local, key).unwrap().is_none());
    }
}
