//! `authority.log@1` on real files: fencing between handles, first claims
//! racing on threads, and the line between a torn tail (settled) and
//! corruption (refused, nothing removed).

hirsel_guard::install!();
use hirsel_store_authority::log::Log;
use hirsel_store_authority::{Advanced, Authority, Error, Level, Record};
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
    // The second claim, one epoch up, repeats the identity the first minted.
    assert_eq!(
        all,
        vec![
            Record::Claimed {
                epoch: 1,
                attempt: [1; 16],
                identity: [9; 32]
            },
            Record::Appended {
                payload: entry(1),
                created: vec![],
                retired: vec![]
            },
            Record::Claimed {
                epoch: 2,
                attempt: [2; 16],
                identity: [9; 32]
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
