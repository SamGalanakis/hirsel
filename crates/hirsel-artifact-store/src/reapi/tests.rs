use super::*;

// FIPS 180-2 appendix B.1 ("abc") and the SHA-256 of zero bytes, the empty
// digest REAPI servers must always serve (remote_execution.proto:333-335).
const EMPTY_HEX: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ABC_HEX: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const DOMAIN: Domain = Domain { kind: 7, schema: 3 };

fn blob(bytes: &[u8]) -> (BlobDigest, Bytes) {
    (BlobDigest::of(bytes), bytes.into())
}

#[test]
fn blob_digests_are_reapi_sha256_digests_of_raw_bytes() {
    assert_eq!(BlobDigest::of(b""), EMPTY);
    assert_eq!(EMPTY.hash(), EMPTY_HEX);
    assert_eq!(EMPTY.size_bytes(), 0);
    let abc = BlobDigest::of(b"abc");
    assert_eq!((abc.hash().as_str(), abc.size_bytes()), (ABC_HEX, 3));
    assert_eq!(BlobDigest::from_reapi(ABC_HEX, 3), Some(abc));
    assert_eq!(BlobDigest::from_reapi(EMPTY_HEX, 0), Some(EMPTY));
    // Digest.hash is exactly 64 lowercase hex digits; size_bytes is a length.
    assert_eq!(BlobDigest::from_reapi(&ABC_HEX.to_uppercase(), 3), None);
    assert_eq!(BlobDigest::from_reapi(&ABC_HEX[1..], 3), None);
    assert_eq!(BlobDigest::from_reapi(ABC_HEX, -1), None);
}

#[test]
fn framed_outputs_never_name_the_blob_digest_of_the_same_bytes() {
    for bytes in [&b""[..], b"abc", &[0; 4096]] {
        let output = ActionResult::output(DOMAIN, bytes).unwrap();
        assert_eq!(output.blob, BlobDigest::of(bytes));
        assert_eq!(output.reference, DOMAIN.reference(bytes).unwrap());
        assert_ne!(output.reference.output.0.0, output.blob.sha256);
        assert_eq!(output.reference.len, output.blob.size);
    }
}

#[test]
fn the_empty_blob_is_present_and_size_is_part_of_the_digest() {
    let directory = tempfile::tempdir().unwrap();
    let cas = DirCas::open(directory.path()).unwrap();
    let (abc, bytes) = blob(b"abc");
    let wrong_size = BlobDigest { size: 4, ..abc };
    assert_eq!(cas.find_missing(&[EMPTY, abc, abc]).unwrap(), vec![abc]);
    assert_eq!(
        &*cas
            .batch_read(&[EMPTY])
            .unwrap()
            .pop()
            .unwrap()
            .unwrap()
            .unwrap(),
        b""
    );
    let statuses = cas
        .batch_update(&[(abc, bytes.clone()), (wrong_size, bytes)])
        .unwrap();
    assert!(statuses[0].is_ok());
    assert_eq!(
        statuses[1].as_ref().unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        cas.find_missing(&[abc, wrong_size]).unwrap(),
        vec![wrong_size]
    );
    let reads = cas.batch_read(&[abc, wrong_size]).unwrap();
    assert_eq!(&**reads[0].as_ref().unwrap().as_ref().unwrap(), b"abc");
    assert!(reads[1].as_ref().unwrap().is_none());
}

#[test]
fn reads_distinguish_missing_from_corrupt_per_digest() {
    let directory = tempfile::tempdir().unwrap();
    let cas = DirCas::open(directory.path()).unwrap();
    let (good, good_bytes) = blob(b"abc");
    let (bad, bad_bytes) = blob(b"abd");
    let absent = BlobDigest::of(b"abe");
    let statuses = cas
        .batch_update(&[(good, good_bytes), (bad, bad_bytes)])
        .unwrap();
    assert!(statuses.iter().all(Result::is_ok));
    fs::write(cas.blob_path(&bad), b"abx").unwrap();
    let reads = cas.batch_read(&[good, bad, absent]).unwrap();
    assert_eq!(&**reads[0].as_ref().unwrap().as_ref().unwrap(), b"abc");
    assert_eq!(
        reads[1].as_ref().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(reads[2].as_ref().unwrap().is_none());
    // A re-upload of the correct bytes repairs the corrupt entry.
    assert!(cas.batch_update(&[blob(b"abd")]).unwrap()[0].is_ok());
    assert_eq!(
        &*cas.batch_read(&[bad]).unwrap()[0]
            .as_ref()
            .unwrap()
            .clone()
            .unwrap(),
        b"abd"
    );
}

#[test]
fn action_results_round_trip_only_over_present_blobs() {
    let directory = tempfile::tempdir().unwrap();
    let cas = DirCas::open(directory.path()).unwrap();
    let action_bytes = b"abc";
    let action = ActionDigest::of(action_bytes);
    assert_eq!(action.0.hash(), ABC_HEX);
    let root = Domain { kind: 1, schema: 1 };
    let result = ActionResult {
        outputs: vec![
            ActionOutput {
                reference: root.reference(b"root").unwrap(),
                blob: BlobDigest::of(b"root"),
            },
            ActionOutput {
                reference: DOMAIN.reference(b"").unwrap(),
                blob: EMPTY,
            },
        ],
    };
    assert!(cas.get(&action).unwrap().is_none());
    // REAPI UpdateActionResult: the action and its outputs are uploaded first.
    assert_eq!(
        cas.update(&action, &result).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    let uploaded = cas
        .batch_update(&[blob(action_bytes), blob(b"root")])
        .unwrap();
    assert!(uploaded.iter().all(Result::is_ok));
    cas.update(&action, &result).unwrap();
    assert_eq!(cas.get(&action).unwrap(), Some(result.clone()));
    assert!(cas.get(&ActionDigest::of(b"abd")).unwrap().is_none());
    let mismatched = ActionResult {
        outputs: vec![ActionOutput {
            reference: root.reference(b"root").unwrap(),
            blob: EMPTY,
        }],
    };
    assert_eq!(
        cas.update(&action, &mismatched).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    // A result whose blobs were evicted is a miss, never a partial hit.
    fs::remove_file(cas.blob_path(&BlobDigest::of(b"root"))).unwrap();
    assert!(cas.get(&action).unwrap().is_none());
    assert!(cas.batch_update(&[blob(b"root")]).unwrap()[0].is_ok());
    let path = cas.result_path(&action);
    let mut stored = fs::read(&path).unwrap();
    stored[20] ^= 1;
    fs::write(&path, stored).unwrap();
    assert_eq!(
        cas.get(&action).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
