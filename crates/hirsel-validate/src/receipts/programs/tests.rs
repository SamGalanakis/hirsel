use super::*;
use crate::receipts::Installation;
use hirsel_artifact_store::{Action, Address, Association as Header, Domain, Store};
use std::sync::Arc;

struct Domain_ {
    _root: tempfile::TempDir,
    _state: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    verifier: ContentHash,
}
impl Domain_ {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let domain = Self {
            root: root.path().join("products"),
            state: state.path().join("private"),
            _root: root,
            _state: state,
            verifier: ContentHash::of_bytes(b"program record verifier"),
        };
        domain.register(b"first header");
        domain
    }
    /// Publish a header into the root and register a fresh generation for it.
    fn register(&self, header: &[u8]) {
        fs::create_dir_all(&self.root).unwrap();
        let store = Store::new(self.root.clone());
        let reference = store
            .put(
                Domain {
                    kind: 31,
                    schema: 1,
                },
                header,
            )
            .unwrap();
        let address = Address(ContentHash::of_bytes(b"ordinary"));
        let action = Action(ContentHash::default());
        let install = Installation::begin(&self.root, &self.state, self.verifier).unwrap();
        store.associate(address, action, reference).unwrap();
        install
            .finish((address, Header { action, reference }))
            .unwrap();
    }
    fn authority(&self) -> Option<ProgramAuthority> {
        let registered =
            Installation::registered(&self.root, &self.state, self.verifier, Arc::default())?;
        ProgramAuthority::new(vec![registered.authority])
    }
    fn records(&self) -> ProgramRecords {
        self.authority().unwrap().records().unwrap()
    }
}

const ADDRESS: ContentHash = ContentHash([7; 32]);
const KEY: ContentHash = ContentHash([9; 32]);

fn record<'a>(executable: &'a [u8], diagnostics: &'a [u8]) -> ProgramRecord<'a> {
    ProgramRecord {
        operation: ProgramOperation::Build,
        kind: ProgramKind::Success,
        status: 0,
        address: ADDRESS,
        key: KEY,
        image: None,
        diagnostics,
        executable: Some(executable),
    }
}
fn hit(records: &ProgramRecords) -> Option<ProgramHit> {
    match records.lookup(ADDRESS, ProgramOperation::Build, |_| Some(KEY)) {
        Lookup::Hit(hit) => Some(hit),
        Lookup::Miss { .. } => None,
    }
}
fn generation(records: &ProgramRecords) -> u64 {
    match records.lookup(ADDRESS, ProgramOperation::Build, |_| None) {
        Lookup::Miss { generation } => generation,
        Lookup::Hit(_) => unreachable!(),
    }
}
fn executable_bytes(hit: &ProgramHit, directory: &Path) -> Vec<u8> {
    let out = directory.join("out");
    let _ = fs::remove_file(&out);
    hit.materialize(&out).unwrap();
    fs::read(out).unwrap()
}
fn programs(domain: &Domain_) -> PathBuf {
    domain.records().root.clone()
}

/// Bug: B accepted under bytes alone. A copied store, a record whose
/// registration was replaced or removed, a forged association and a
/// substituted record (E' with H' naming it) must each miss.
#[test]
fn only_the_registration_that_published_a_record_answers_it() {
    let domain = Domain_::new();
    let records = domain.records();
    assert_eq!(
        records.publish(&record(b"E", b"d"), 0).unwrap(),
        Published::Committed { generation: 1 }
    );
    assert!(hit(&records).is_some());

    // Copied store: another domain holding the same files.
    let other = Domain_::new();
    let copy = programs(&other);
    fs::remove_dir_all(&copy).unwrap();
    let status = std::process::Command::new("cp")
        .arg("-a")
        .arg(&records.root)
        .arg(&copy)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(hit(&other.records()).is_none(), "a copied store answered");

    // Forged association: any byte changed fails the MAC.
    let journal = records.journal(ADDRESS);
    let original = fs::read(&journal).unwrap();
    for at in [9, 16, 30, 100, JOURNAL_BYTES - 1] {
        let mut forged = original.clone();
        forged[at] ^= 1;
        fs::write(&journal, &forged).unwrap();
        assert!(hit(&records).is_none(), "forged byte {at} answered");
    }
    fs::write(&journal, &original).unwrap();
    assert!(hit(&records).is_some());

    // Substituted record: E' and an H' naming E', under the old association.
    let association = records.current(ADDRESS).unwrap();
    let mut substitute = fs::read(records.blob(association.record)).unwrap();
    let h = DIRECTORY_BYTES;
    let mut header = ProgramHeader::decode(&substitute[h..h + HEADER_BYTES]).unwrap();
    header.executable = Some((ContentHash::of_bytes(b"E'"), 2));
    substitute[h..h + HEADER_BYTES].copy_from_slice(&header.encode());
    fs::write(records.blob(association.record), &substitute).unwrap();
    assert!(hit(&records).is_none(), "a substituted header answered");

    // Replaced registration: a new generation for the same root.
    let fresh = Domain_::new();
    let records = fresh.records();
    records.publish(&record(b"E", b"d"), 0).unwrap();
    fresh.register(b"second header");
    assert!(
        hit(&fresh.records()).is_none(),
        "a replaced registration answered"
    );
    // Removed registration: no authority at all.
    for entry in fs::read_dir(&fresh.state).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "registration") {
            fs::remove_file(path).unwrap();
        }
    }
    assert!(fresh.authority().is_none());
}

/// Bug: a record answering another operation, or a build failure keyed
/// against the wrong P. The key is chosen by kind and the operation by H.
#[test]
fn records_answer_only_their_operation_and_kind() {
    let domain = Domain_::new();
    let records = domain.records();
    let check_p = ContentHash([1; 32]);
    let build_p = ContentHash([2; 32]);
    let failure = ProgramRecord {
        operation: ProgramOperation::Build,
        kind: ProgramKind::SourceDiagnostics,
        status: 1,
        address: ADDRESS,
        key: check_p,
        image: None,
        diagnostics: b"error",
        executable: None,
    };
    records.publish(&failure, 0).unwrap();
    let keyed = |kind| match kind {
        ProgramKind::Success => Some(build_p),
        ProgramKind::SourceDiagnostics => Some(check_p),
    };
    let Lookup::Hit(found) = records.lookup(ADDRESS, ProgramOperation::Build, keyed) else {
        panic!("failing build missed under check-P");
    };
    assert_eq!(found.diagnostics, b"error");
    assert!(found.header.executable.is_none());
    drop(found);
    assert!(matches!(
        records.lookup(ADDRESS, ProgramOperation::Check, keyed),
        Lookup::Miss { .. }
    ));
    assert!(matches!(
        records.lookup(ADDRESS, ProgramOperation::Build, |_| Some(build_p)),
        Lookup::Miss { .. }
    ));
}

/// Bug: a losing publisher unpinning the winner, or both committing.
#[test]
fn concurrent_publishers_commit_once_and_the_loser_drops_only_its_pin() {
    let domain = Domain_::new();
    let records = domain.records();
    let expected = generation(&records);
    let barrier = std::sync::Barrier::new(8);
    let outcomes: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8u8)
            .map(|i| {
                let records = &records;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    let executable = [i; 64];
                    records
                        .publish(&record(&executable, b"d"), expected)
                        .unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, Published::Committed { .. }))
            .count(),
        1
    );
    let found = hit(&records).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let bytes = executable_bytes(&found, directory.path());
    drop(found);
    records.collect().unwrap();
    let found = hit(&records).unwrap();
    assert_eq!(executable_bytes(&found, directory.path()), bytes);
    assert_eq!(fs::read_dir(records.root.join("pins")).unwrap().count(), 1);
    assert_eq!(fs::read_dir(records.root.join("blobs")).unwrap().count(), 2);
}

/// Bug: collection freeing a generation a reader selected before a replace.
#[test]
fn a_reader_keeps_its_generation_across_replace_and_collection() {
    let domain = Domain_::new();
    let records = domain.records();
    records.publish(&record(b"old", b"d"), 0).unwrap();
    let reader = hit(&records).unwrap();
    assert_eq!(
        records.publish(&record(b"new", b"d"), 1).unwrap(),
        Published::Committed { generation: 2 }
    );
    records.collect().unwrap();
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(executable_bytes(&reader, directory.path()), b"old");
    drop(reader);
    let collected = records.collect().unwrap();
    assert_eq!(collected, Collected { pins: 1, blobs: 2 });
    assert_eq!(
        executable_bytes(&hit(&records).unwrap(), directory.path()),
        b"new"
    );
}

/// Law: a committed association's closure is present, after a crash at any
/// boundary and after the collection that follows it; nothing unpinned stays.
#[test]
fn a_crash_at_any_boundary_leaves_a_committed_closure_and_no_leak() {
    for boundary in [
        Boundary::AfterPin,
        Boundary::AfterContent,
        Boundary::AfterJournalWrite,
        Boundary::AfterCommit,
    ] {
        let domain = Domain_::new();
        let records = domain.records();
        records.publish(&record(b"old", b"d"), 0).unwrap();
        CRASH.with(|c| c.set(Some(boundary)));
        assert!(records.publish(&record(b"new", b"d"), 1).is_err());
        CRASH.with(|c| c.set(None));
        let directory = tempfile::tempdir().unwrap();
        for collected in [false, true] {
            if collected {
                records.collect().unwrap();
            }
            let found =
                hit(&records).unwrap_or_else(|| panic!("{boundary:?}: committed record lost"));
            let expected: &[u8] = if boundary == Boundary::AfterCommit {
                b"new"
            } else {
                b"old"
            };
            assert_eq!(
                executable_bytes(&found, directory.path()),
                expected,
                "{boundary:?}"
            );
        }
        let pins = fs::read_dir(records.root.join("pins")).unwrap().count();
        let blobs = fs::read_dir(records.root.join("blobs")).unwrap().count();
        assert_eq!(
            (pins, blobs),
            (1, 2),
            "{boundary:?}: leaked or lost content"
        );
        // The next publication proceeds from the committed generation.
        let next = generation(&records);
        assert!(matches!(
            records.publish(&record(b"next", b"d"), next).unwrap(),
            Published::Committed { .. }
        ));
    }
}

/// Bug: a stale, modified or non-executable output counted as built; or an
/// unchanged output rewritten.
#[test]
fn materialization_restores_the_recorded_executable() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let domain = Domain_::new();
    let records = domain.records();
    records
        .publish(&record(b"\x7fELF program", b""), 0)
        .unwrap();
    let found = hit(&records).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("bin/app");
    found.materialize(&out).unwrap();
    let inode = fs::metadata(&out).unwrap().ino();
    found.materialize(&out).unwrap();
    assert_eq!(
        fs::metadata(&out).unwrap().ino(),
        inode,
        "unchanged output was rewritten"
    );
    fs::write(&out, b"\x7fELF progran").unwrap();
    found.materialize(&out).unwrap();
    assert_eq!(fs::read(&out).unwrap(), b"\x7fELF program");
    fs::set_permissions(&out, fs::Permissions::from_mode(0o644)).unwrap();
    found.materialize(&out).unwrap();
    assert_ne!(fs::metadata(&out).unwrap().permissions().mode() & 0o111, 0);
    fs::remove_file(&out).unwrap();
    found.materialize(&out).unwrap();
    assert_eq!(fs::read(&out).unwrap(), b"\x7fELF program");
    // A corrupted stored executable fails its digest and replaces nothing.
    let (digest, _) = found.header.executable.unwrap();
    fs::write(records.blob(digest), b"\x7fELF corrupt").unwrap();
    fs::remove_file(&out).unwrap();
    assert!(found.materialize(&out).is_err());
    assert!(!out.exists());
}
