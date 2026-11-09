//! The durable call journal keeps every acknowledged record, cuts only an incomplete final
//! frame, and refuses any other damage as corrupt.
use hirsel_remote::journal::{DirJournal, Entry, Journal, StreamKey};
use hirsel_run_schema::CallOutcome;

fn stream() -> StreamKey {
    StreamKey {
        caller: "app".into(),
        stream: [5; 32],
    }
}

#[test]
fn a_dir_journal_recovers_its_records_and_drops_a_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let entries = vec![
        Entry::Stream {
            stream: stream(),
            instance: [1; 16],
        },
        Entry::Admitted {
            stream: stream(),
            position: 1,
            key: [2; 32],
            digest: [3; 32],
        },
        Entry::Decided {
            stream: stream(),
            position: 1,
            key: [2; 32],
            digest: [3; 32],
            end: CallOutcome::Failed(b"no".to_vec()),
            callee: Some([4; 16]),
        },
        Entry::Acknowledged {
            stream: stream(),
            position: 1,
        },
    ];
    let incarnation = {
        let mut journal = DirJournal::open(dir.path(), [9; 16]).unwrap();
        for entry in &entries {
            journal.append(entry).unwrap();
        }
        journal.incarnation()
    };
    assert_eq!(incarnation, [9; 16]);
    // A torn record at the end: its length promises more than was written.
    let path = dir.path().join("calls.journal");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(&100u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 10]);
    std::fs::write(&path, &bytes).unwrap();
    let mut journal = DirJournal::open(dir.path(), [8; 16]).unwrap();
    assert_eq!(journal.incarnation(), [9; 16], "the incarnation survives");
    assert_eq!(journal.recover().unwrap(), entries);
    // Appends follow the last intact record.
    let more = Entry::Acknowledged {
        stream: stream(),
        position: 2,
    };
    journal.append(&more).unwrap();
    let mut reopened = DirJournal::open(dir.path(), [7; 16]).unwrap();
    let mut expected = entries.clone();
    expected.push(more);
    assert_eq!(reopened.recover().unwrap(), expected);
}

/// The shared damage table (determinism.md, Frames and recovery): only an incomplete final
/// frame is cut, every other damage is refused, and a refused file is left byte-identical,
/// so no later synced record is lost.
#[test]
fn the_shared_damage_table_holds_and_a_refusal_leaves_the_file_unchanged() {
    use hirsel_remote::journal::{FORMAT, known};
    use hirsel_run_schema::frame::damage::{Expect, cases};
    let dir = tempfile::tempdir().unwrap();
    {
        let mut journal = DirJournal::open(dir.path(), [9; 16]).unwrap();
        for position in 1..=3 {
            journal
                .append(&Entry::Acknowledged {
                    stream: stream(),
                    position,
                })
                .unwrap();
        }
    }
    let path = dir.path().join("calls.journal");
    let intact = std::fs::read(&path).unwrap();
    for case in cases(&intact, FORMAT.len(), known, 9) {
        std::fs::write(&path, &case.bytes).unwrap();
        match (DirJournal::open(dir.path(), [8; 16]), case.expect) {
            (Ok(mut journal), Expect::Valid(valid)) => {
                assert_eq!(journal.incarnation(), [9; 16], "{}", case.name);
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    case.bytes[..valid],
                    "{}",
                    case.name
                );
                let records = journal.recover().unwrap();
                assert_eq!(
                    records.len(),
                    if valid == intact.len() { 3 } else { 2 },
                    "{}",
                    case.name
                );
            }
            (Err(error), Expect::Corrupt) => {
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::InvalidData,
                    "{}: {error}",
                    case.name
                );
                assert_eq!(std::fs::read(&path).unwrap(), case.bytes, "{}", case.name);
            }
            (got, expect) => panic!("{}: {:?} where {expect:?}", case.name, got.map(|_| ())),
        }
    }
    // The file header protects the incarnation; a short one was never durable.
    let mut identity = intact.clone();
    identity[FORMAT.magic.len()] ^= 1;
    std::fs::write(&path, &identity).unwrap();
    assert!(DirJournal::open(dir.path(), [8; 16]).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), identity);
    std::fs::write(&path, &intact[..FORMAT.len() - 1]).unwrap();
    assert_eq!(
        DirJournal::open(dir.path(), [8; 16]).unwrap().incarnation(),
        [8; 16]
    );
}
