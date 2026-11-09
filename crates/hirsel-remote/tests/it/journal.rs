//! The durable call journal keeps every acknowledged record, cuts only a torn tail, and
//! refuses earlier damage as corrupt.
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

/// Damage to a record before the last cannot be a torn write: a flipped byte or a lost
/// record (a sequence gap) is refused, and the file is left as it is, so the later synced
/// records are not lost.
#[test]
fn interior_damage_is_corrupt_and_leaves_the_file_unchanged() {
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
    // A 24-byte header, then records of a 44-byte head and a body; all three are equal in
    // length.
    let record = 44 + intact[24] as usize;
    let second = 24 + record;
    let mut flipped = intact.clone();
    flipped[second + 50] ^= 1;
    let mut lost = intact.clone();
    lost.drain(second..second + record);
    for damaged in [flipped, lost] {
        std::fs::write(&path, &damaged).unwrap();
        let error = DirJournal::open(dir.path(), [8; 16]).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), damaged);
    }
}
