//! `authority.log@1` on real files: fencing between handles, and the line
//! between a torn tail (settled) and corruption (refused, nothing removed).

hirsel_guard::install!();
use hirsel_store_authority::group::{self, Tip};
use hirsel_store_authority::log::Log;
use hirsel_store_authority::{Advanced, Authority, Error, Level};
use std::fs;

const HOST: Level = Level(1);

/// A non-claim group following `tip`.
fn entry(tip: &Tip, tag: u8) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"HSHIST01");
    out.extend_from_slice(&tip.digest);
    out.extend_from_slice(&(tip.last + 1).to_le_bytes());
    out.extend_from_slice(&tip.epoch.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&[2, 7]);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.push(tag);
    out.extend_from_slice(&[0; 8]);
    out
}

fn tip_of(log: &mut Log) -> Tip {
    let all = log.read(0, Level(0), u64::MAX).unwrap();
    all.iter()
        .fold(Tip::origin(), |t, e| group::follow(&t, e).unwrap())
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
    let t = tip_of(&mut a);
    assert_eq!(
        a.advance(&mut ca, 1, entry(&t, 1), HOST),
        Advanced::Advanced(2)
    );
    let cb = b.claim([2; 16], [8; 32]).unwrap();
    assert_eq!(cb.end, 3);
    let t = tip_of(&mut a);
    // The superseded writer appends nothing and stays dead.
    assert_eq!(a.advance(&mut ca, 2, entry(&t, 2), HOST), Advanced::Fenced);
    assert_eq!(a.durable(&mut ca, HOST), Err(Error::Fenced));
    assert_eq!(b.read(0, HOST, u64::MAX).unwrap().len(), 3);
    // The second claim repeats the identity the first minted.
    assert_eq!(tip_of(&mut b).identity, Some([9; 32]));
}

#[test]
fn a_cut_short_tail_is_settled_and_a_damaged_one_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = Log::open(dir.path(), HOST).unwrap();
    let mut c = a.claim([1; 16], [9; 32]).unwrap().claim;
    let t = tip_of(&mut a);
    assert_eq!(
        a.advance(&mut c, 1, entry(&t, 1), HOST),
        Advanced::Advanced(2)
    );
    let whole = fs::read(segment(dir.path())).unwrap();

    // A frame cut short at the end is an append a crash interrupted.
    let t = tip_of(&mut a);
    let mut torn = whole.clone();
    torn.extend_from_slice(&(entry(&t, 2).len() as u32).to_le_bytes());
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
