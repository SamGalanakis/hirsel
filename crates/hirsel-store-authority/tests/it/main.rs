//! `authority.log@1` on real files: fencing between handles, first claims
//! racing on threads, and the line between a torn tail (settled) and
//! corruption (refused, nothing removed).

hirsel_guard::install!();
use hirsel_store_authority::log::Log;
use hirsel_store_authority::{Advanced, envelope, Authority, Error, Level, Locator, Record};
use sha2::{Digest, Sha256};
use std::fs;
use std::sync::Barrier;

const HOST: Level = Level(1);

/// An engine payload unique by `tag`.
fn entry(tag: u8) -> Vec<u8> {
    vec![tag; 24]
}

fn segment(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(format!("{:020}.seg", 1))
}

#[test]
fn a_later_claim_fences_the_earlier_writer() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = Log::open(dir.path(), HOST).unwrap();
    let mut b = Log::open(dir.path(), HOST).unwrap();
    let mut ca = a.claim([1; 16], [9; 32]).unwrap().claim;
    assert_eq!(a.submit(&mut ca, entry(1), &[], &[], HOST), Ok(2));
    assert_eq!(a.wait(&mut ca, 2), Advanced::Advanced(2));
    let cb = b.claim([2; 16], [8; 32]).unwrap();
    assert_eq!(cb.end, 3);
    // The superseded writer appends nothing and stays dead.
    assert_eq!(a.submit(&mut ca, entry(2), &[], &[], HOST), Ok(3));
    assert_eq!(a.wait(&mut ca, 3), Advanced::Fenced);
    assert_eq!(
        a.submit(&mut ca, entry(3), &[], &[], HOST),
        Err(Error::Fenced)
    );
    assert_eq!(a.durable(&mut ca, HOST), Err(Error::Fenced));
    let all = b.read(0, HOST, u64::MAX).unwrap();
    // The second claim, one epoch up, repeats the identity the first minted
    // and names the append before it: position 2 and the digest of its entry,
    // which follows the claim's frame (a 4-byte length, a 4-byte check and the
    // 145-byte claim) and its own 8 header bytes, 89 bytes long (a 57-byte
    // header, two empty inventories, the payload).
    let file = fs::read(segment(dir.path())).unwrap();
    let appended = Locator {
        position: 2,
        digest: Sha256::digest(&file[161..161 + 89]).into(),
    };
    assert_eq!(b.last(HOST), Ok(Some(appended)));
    assert_eq!(
        all,
        vec![
            Record::Claimed {
                epoch: 1,
                attempt: [1; 16],
                identity: [9; 32],
                appended: None,
            },
            Record::Appended {
                payload: entry(1),
                created: vec![],
                retired: vec![]
            },
            Record::Claimed {
                epoch: 2,
                attempt: [2; 16],
                identity: [9; 32],
                appended: Some(appended),
            },
        ]
    );
}

#[test]
fn a_cut_short_tail_is_settled_and_a_damaged_one_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = Log::open(dir.path(), HOST).unwrap();
    let mut c = a.claim([1; 16], [9; 32]).unwrap().claim;
    assert_eq!(a.submit(&mut c, entry(1), &[], &[], HOST), Ok(2));
    assert_eq!(a.wait(&mut c, 2), Advanced::Advanced(2));
    let whole = fs::read(segment(dir.path())).unwrap();

    // A frame cut short at the end is an append a crash interrupted.
    let mut torn = whole.clone();
    torn.extend_from_slice(&100u32.to_le_bytes());
    fs::write(segment(dir.path()), &torn).unwrap();
    let mut b = Log::open(dir.path(), HOST).unwrap();
    assert_eq!(b.claim([2; 16], [8; 32]).unwrap().end, 3);

    // A damaged length in the final frame is corruption: refused, nothing removed.
    let mut damaged = fs::read(segment(dir.path())).unwrap();
    let last = whole.len();
    damaged[last] ^= 0x40;
    fs::write(segment(dir.path()), &damaged).unwrap();
    let mut c = Log::open(dir.path(), HOST).unwrap();
    assert!(matches!(c.claim([3; 16], [7; 32]), Err(Error::Corrupt(_))));
    assert_eq!(fs::read(segment(dir.path())).unwrap(), damaged);
}

/// The frame check: the first four bytes of the SHA-256 of the length and
/// the position (storage.md, "authority.log@1").
fn check(len: u32, position: u64) -> [u8; 4] {
    let mut h = Sha256::new();
    h.update(len.to_le_bytes());
    h.update(position.to_le_bytes());
    h.finalize()[..4].try_into().unwrap()
}

/// A crash may cut the newest frame anywhere: in its length or check, or in
/// its entry, here a claim's record, through the locator it holds. A fresh
/// handle's `last` then answers the append before it, found by a forward scan
/// of the segment from its start, and `at` from that locator reads that
/// append. A payload holding a whole frame of the next position, cut right
/// after it, is still one torn frame: the scan never reads inside an entry. A
/// locator the history cannot authenticate (another digest, a position past
/// the end, a segment that is gone) answers nothing, so the reader starts
/// again: none is corrupt.
#[test]
fn a_cut_in_the_newest_frame_answers_the_append_before_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = Log::open(dir.path(), HOST).unwrap();
    let mut c = a.claim([1; 16], [9; 32]).unwrap().claim;
    assert_eq!(a.submit(&mut c, entry(1), &[], &[], HOST), Ok(2));
    assert_eq!(a.wait(&mut c, 2), Advanced::Advanced(2));
    let two = fs::read(segment(dir.path())).unwrap();
    let appended = Locator {
        position: 2,
        digest: Sha256::digest(&two[161..161 + 89]).into(),
    };
    let append = Record::Appended {
        payload: entry(1),
        created: vec![],
        retired: vec![],
    };
    let reads = |cut: &[u8], what: &str| {
        fs::write(segment(dir.path()), cut).unwrap();
        let mut b = Log::open(dir.path(), HOST).unwrap();
        assert_eq!(b.last(HOST), Ok(Some(appended)), "{what}");
        let mut b = Log::open(dir.path(), HOST).unwrap();
        assert_eq!(b.at(&appended, 1, HOST, 1), Ok(vec![append.clone()]), "{what}");
    };

    // A claim after the append: its frame's 8 header bytes, then its 145-byte
    // record, whose last 40 bytes are the locator of the append.
    Log::open(dir.path(), HOST).unwrap().claim([2; 16], [8; 32]).unwrap();
    let whole = fs::read(segment(dir.path())).unwrap();
    assert_eq!(whole.len(), two.len() + 8 + 145);
    assert_eq!(&whole[whole.len() - 32..], &appended.digest);
    for cut in two.len()..=whole.len() {
        reads(&whole[..cut], &format!("cut at {cut}"));
    }

    // An outer frame at position 3 whose payload is a whole frame at
    // position 3 holding an append that follows the history, cut right after
    // the inner frame: a reader that took it would answer position 3.
    let (tip1, _) = envelope::follow(&envelope::Tip::origin(), &two[8..153]).unwrap();
    let (tip2, _) = envelope::follow(&tip1, &two[161..250]).unwrap();
    let inner_record = &envelope::appended(&tip2, &[], &[], b"forged")[..];
    let mut inner = (inner_record.len() as u32).to_le_bytes().to_vec();
    inner.extend(check(inner_record.len() as u32, 3));
    inner.extend(inner_record);
    let outer_len = inner.len() as u32 + 64;
    let mut forged = two.clone();
    forged.extend(outer_len.to_le_bytes());
    forged.extend(check(outer_len, 3));
    forged.extend(&inner);
    reads(&forged, "a whole frame inside a torn one");

    fs::write(segment(dir.path()), &whole).unwrap();
    let mut other = appended;
    other.digest[0] ^= 1;
    let mut future = appended;
    future.position = 9;
    for l in [other, future] {
        let mut b = Log::open(dir.path(), HOST).unwrap();
        assert_eq!(b.at(&l, 1, HOST, 1), Ok(vec![]), "{l:?}");
    }
    fs::rename(segment(dir.path()), dir.path().join("gone")).unwrap();
    let mut b = Log::open(dir.path(), HOST).unwrap();
    assert_eq!(b.at(&appended, 1, HOST, 1), Ok(vec![]), "a segment gone");
}

/// Handles on racing threads make the first claims on an empty directory at
/// once: each claim is acknowledged at its own position, holding its own
/// attempt, and the history they leave follows.
#[test]
fn racing_first_claims_each_take_their_own_position() {
    const N: usize = 8;
    for _ in 0..40 {
        let dir = tempfile::tempdir().unwrap();
        let start = Barrier::new(N);
        let ends: Vec<(u64, [u8; 16])> = std::thread::scope(|s| {
            let racers: Vec<_> = (0..N)
                .map(|i| {
                    let (dir, start) = (dir.path(), &start);
                    s.spawn(move || {
                        let mut log = Log::open(dir, HOST).unwrap();
                        let attempt = [i as u8 + 1; 16];
                        start.wait();
                        (log.claim(attempt, [i as u8; 32]).unwrap().end, attempt)
                    })
                })
                .collect();
            racers.into_iter().map(|r| r.join().unwrap()).collect()
        });
        let mut log = Log::open(dir.path(), HOST).unwrap();
        let all = log.read(0, HOST, u64::MAX).unwrap();
        assert_eq!(all.len(), N);
        for (end, attempt) in &ends {
            assert!(
                matches!(&all[*end as usize - 1], Record::Claimed { attempt: a, .. } if a == attempt)
            );
        }
    }
}
