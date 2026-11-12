//! The history envelope (storage.md STORE-11 "History format"): the record an
//! authority keeps at each position. The authority owns it whole; what an
//! `Appended` record carries is the engine's payload, which no authority reads.
//!
//! ```text
//! record   = "HSHIST06" prev:[32] position:u64 epoch:u64 kind:u8 body
//! Claimed  = kind 1, body attempt:[16] identity:[32] appended:locator
//! Appended = kind 2, body count:u32 digest:[32]* retired:u32 ref* payload
//! ref      = lifetime:u64 epoch:u64 digest:[32]
//! locator  = position:u64 digest:[32]
//! ```
//!
//! An `Appended` record's inventory lists the digests of the objects the entry
//! creates, whose lifetime and epoch are the record's own, then the objects it
//! retires, each created by an earlier record; the payload is the rest.
//!
//! `prev` is the SHA-256 of the record before, zeros at position 1. A
//! `Claimed` record raises the epoch by one and names the store identity, which
//! the first one mints; an `Appended` record keeps the epoch, and none comes
//! before the first claim. A claim also names the newest append before it
//! (all zeros when there is none), so the newest append is one read from the
//! last record whichever kind that is.

use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"HSHIST06";
/// The magic's family: other version digits are another format version.
const FAMILY: &[u8; 6] = b"HSHIST";
const HEADER: usize = 8 + 32 + 8 + 8 + 1;
const CLAIMED: u8 = 1;
const APPENDED: u8 = 2;
const CLAIMED_BYTES: usize = HEADER + 16 + 32 + crate::Locator::BYTES;
const DIGEST_BYTES: usize = 32;
/// A retired reference: lifetime, epoch and digest.
pub(crate) const REF_BYTES: usize = 48;

/// What a record says, without its chain fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Record {
    Claimed {
        epoch: u64,
        attempt: [u8; 16],
        identity: [u8; 32],
        /// The newest append before the claim, `None` when there is none.
        appended: Option<crate::Locator>,
    },
    Appended {
        payload: Vec<u8>,
        /// The objects the entry creates, each at the record's position and epoch.
        created: Vec<crate::ObjectRef>,
        /// The objects the entry retires, each created at an earlier position.
        retired: Vec<crate::ObjectRef>,
    },
}

/// The end of a followed history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tip {
    pub digest: [u8; 32],
    pub end: u64,
    pub epoch: u64,
    /// The store identity, none before the first claim.
    pub identity: Option<[u8; 32]>,
    /// The newest append, none before the first.
    pub appended: Option<crate::Locator>,
}

impl Tip {
    pub fn origin() -> Self {
        Tip {
            digest: [0; 32],
            end: 0,
            epoch: 0,
            identity: None,
            appended: None,
        }
    }
}

/// Why a record could not be followed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Bad {
    Corrupt,
    /// A record of the format family with other version digits.
    Unsupported([u8; 8]),
}

pub fn digest(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The record's magic, checked first so another version is never damage.
fn format(b: &[u8]) -> Result<(), Bad> {
    if b.len() >= 8 && &b[..8] != MAGIC && &b[..6] == FAMILY {
        return Err(Bad::Unsupported(b[..8].try_into().unwrap()));
    }
    if b.len() < HEADER || &b[..8] != MAGIC {
        return Err(Bad::Corrupt);
    }
    Ok(())
}

/// The digest of the record before `b`, from its header.
pub fn prev(b: &[u8]) -> Result<[u8; 32], Bad> {
    format(b)?;
    Ok(b[8..40].try_into().unwrap())
}

/// The position `b`'s header names.
pub fn position(b: &[u8]) -> Result<u64, Bad> {
    format(b)?;
    Ok(u64_at(b, 40))
}

/// The record `b` holds, every field checked except the chain.
pub fn decode(b: &[u8]) -> Result<Record, Bad> {
    format(b)?;
    let epoch = u64_at(b, 48);
    match b[56] {
        CLAIMED if b.len() == CLAIMED_BYTES && epoch > 0 => {
            let l = crate::Locator::decode(&b[HEADER + 48..]).unwrap();
            let appended = match l.position {
                0 if l == crate::Locator::default() => None,
                p if p > 0 && p < u64_at(b, 40) => Some(l),
                _ => return Err(Bad::Corrupt),
            };
            Ok(Record::Claimed {
                epoch,
                attempt: b[HEADER..HEADER + 16].try_into().unwrap(),
                identity: b[HEADER + 16..HEADER + 48].try_into().unwrap(),
                appended,
            })
        }
        APPENDED if epoch > 0 => appended_body(u64_at(b, 40), epoch, &b[HEADER..]),
        _ => Err(Bad::Corrupt),
    }
}

/// An appended record's body at `position` in `epoch`: an inventory of
/// digests, the references it retires, then the payload. A retired reference
/// names an object of an earlier position.
fn appended_body(position: u64, epoch: u64, body: &[u8]) -> Result<Record, Bad> {
    let mut at = 0;
    let count = |at: &mut usize, size: usize| -> Result<usize, Bad> {
        let n = body.get(*at..*at + 4).ok_or(Bad::Corrupt)?;
        let n = u32::from_le_bytes(n.try_into().unwrap()) as usize;
        *at += 4;
        let len = n
            .checked_mul(size)
            .filter(|len| *len <= body.len() - *at)
            .ok_or(Bad::Corrupt)?;
        *at += len;
        Ok(len)
    };
    let len = count(&mut at, DIGEST_BYTES)?;
    let created = body[at - len..at]
        .chunks_exact(DIGEST_BYTES)
        .map(|d| crate::ObjectRef {
            lifetime: position,
            epoch,
            digest: d.try_into().unwrap(),
        })
        .collect();
    let len = count(&mut at, REF_BYTES)?;
    let retired: Vec<crate::ObjectRef> = body[at - len..at]
        .chunks_exact(REF_BYTES)
        .map(object_ref)
        .collect();
    if retired.iter().any(|r| !retirable(r, position, epoch)) {
        return Err(Bad::Corrupt);
    }
    Ok(Record::Appended {
        payload: body[at..].to_vec(),
        created,
        retired,
    })
}

/// The reference a retirement's 48 bytes name.
fn object_ref(b: &[u8]) -> crate::ObjectRef {
    crate::ObjectRef {
        lifetime: u64_at(b, 0),
        epoch: u64_at(b, 8),
        digest: b[16..48].try_into().unwrap(),
    }
}

/// Whether a record at `position` in `epoch` can retire `r`: an object an
/// earlier record created, under this claim or an earlier one.
pub(crate) fn retirable(r: &crate::ObjectRef, position: u64, epoch: u64) -> bool {
    r.lifetime > 0 && r.lifetime < position && r.epoch > 0 && r.epoch <= epoch
}

/// The record `b` and the tip after it, which must follow `tip`: `prev` is the
/// tip's digest and the position one past its end, a claim raises the epoch
/// by one, names the identity the history has (or mints it) and the tip's
/// newest append, and an append keeps the epoch.
pub fn follow(tip: &Tip, b: &[u8]) -> Result<(Tip, Record), Bad> {
    let record = decode(b)?;
    if b[8..40] != tip.digest || Some(u64_at(b, 40)) != tip.end.checked_add(1) {
        return Err(Bad::Corrupt);
    }
    let epoch = u64_at(b, 48);
    let identity = match &record {
        Record::Claimed {
            identity, appended, ..
        } => {
            if Some(epoch) != tip.epoch.checked_add(1)
                || tip.identity.is_some_and(|known| known != *identity)
                || *appended != tip.appended
            {
                return Err(Bad::Corrupt);
            }
            Some(*identity)
        }
        Record::Appended { .. } => {
            if epoch != tip.epoch {
                return Err(Bad::Corrupt);
            }
            tip.identity
        }
    };
    let d = digest(b);
    let appended = match record {
        Record::Appended { .. } => Some(crate::Locator {
            position: tip.end + 1,
            digest: d,
        }),
        Record::Claimed { .. } => tip.appended,
    };
    let next = Tip {
        digest: d,
        end: tip.end + 1,
        epoch,
        identity,
        appended,
    };
    Ok((next, record))
}

fn header(tip: &Tip, epoch: u64, kind: u8, body: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + body);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&tip.digest);
    out.extend_from_slice(&(tip.end + 1).to_le_bytes());
    out.extend_from_slice(&epoch.to_le_bytes());
    out.push(kind);
    out
}

/// The claim record after `tip`, one epoch up, naming `attempt`, the
/// history's identity (`fresh` when it has none yet) and `appended`, the
/// newest append before it.
pub fn claimed(
    tip: &Tip,
    attempt: [u8; 16],
    fresh: [u8; 32],
    appended: Option<crate::Locator>,
) -> Vec<u8> {
    let mut out = header(tip, tip.epoch + 1, CLAIMED, 48 + crate::Locator::BYTES);
    out.extend_from_slice(&attempt);
    out.extend_from_slice(&tip.identity.unwrap_or(fresh));
    out.extend_from_slice(&appended.unwrap_or_default().encode());
    out
}

/// The append record of `payload` after `tip`, in its epoch, inventorying the
/// objects of digests `created` at the record's position and epoch and
/// retiring the objects `retired`.
pub fn appended(
    tip: &Tip,
    created: &[[u8; 32]],
    retired: &[crate::ObjectRef],
    payload: &[u8],
) -> Vec<u8> {
    let body = 8 + created.len() * DIGEST_BYTES + retired.len() * REF_BYTES + payload.len();
    let mut out = header(tip, tip.epoch, APPENDED, body);
    out.extend_from_slice(&(created.len() as u32).to_le_bytes());
    for d in created {
        out.extend_from_slice(d);
    }
    out.extend_from_slice(&(retired.len() as u32).to_le_bytes());
    for r in retired {
        out.extend_from_slice(&r.lifetime.to_le_bytes());
        out.extend_from_slice(&r.epoch.to_le_bytes());
        out.extend_from_slice(&r.digest);
    }
    out.extend_from_slice(payload);
    out
}

/// Whether `b` is a claim record naming `attempt`: how a provider that lost a
/// response recognises its own claim.
pub fn claims(b: &[u8], attempt: &[u8; 16]) -> bool {
    matches!(decode(b), Ok(Record::Claimed { attempt: a, .. }) if a == *attempt)
}

/// Whether `b` is any claim record.
pub fn is_claim(b: &[u8]) -> bool {
    matches!(decode(b), Ok(Record::Claimed { .. }))
}

/// The records a provider has followed: each one's digest by position - 1,
/// and the tip after the last. A record read again is checked against the
/// digest it was followed with, so a reader never sees one the chain does not
/// name.
#[derive(Clone, Debug)]
pub(crate) struct Chain {
    pub(crate) digests: Vec<[u8; 32]>,
    pub(crate) tip: Tip,
}

impl Default for Chain {
    fn default() -> Self {
        Chain {
            digests: Vec::new(),
            tip: Tip::origin(),
        }
    }
}

impl Chain {
    pub(crate) fn end(&self) -> u64 {
        self.digests.len() as u64
    }

    /// Follow the record `b` at the next position.
    pub(crate) fn follow(&mut self, b: &[u8]) -> Result<Record, Bad> {
        let (tip, record) = follow(&self.tip, b)?;
        self.digests.push(tip.digest);
        self.tip = tip;
        Ok(record)
    }

    /// The newest append followed.
    pub(crate) fn appended(&self) -> Option<crate::Locator> {
        self.tip.appended
    }

    /// The locator of a followed position.
    pub(crate) fn locator(&self, position: u64) -> Option<crate::Locator> {
        let digest = *self.digests.get((position as usize).checked_sub(1)?)?;
        Some(crate::Locator { position, digest })
    }

    /// The record `b` read again at `position`, which this chain has followed.
    pub(crate) fn check(&self, position: u64, b: &[u8]) -> Result<Record, Bad> {
        match self.digests.get((position as usize).wrapping_sub(1)) {
            Some(d) if *d == digest(b) => decode(b),
            _ => Err(Bad::Corrupt),
        }
    }
}

/// Laws of the envelope (storage.md STORE-11, History format), each oracle
/// the format as written there:
/// 1. records written by hand from the format are what `claimed` and
///    `appended` write, and decode to the values they were written from;
/// 2. the chain rules: `prev`, position, a claim one epoch up naming the one
///    identity, an append in its epoch, nothing before the first claim;
/// 3. every single-bit flip and truncation of a record is refused, by the
///    record itself or the next one's `prev` when followed, and by its digest
///    when read again; a change to the magic's version digits is another
///    format version, anything else corruption.
#[cfg(test)]
mod tests {
    use super::*;

    /// The first claim: no append before it, so its locator is 40 zeros.
    fn hand_claim() -> Vec<u8> {
        let mut b = b"HSHIST06".to_vec();
        b.extend([0; 32]);
        b.extend([1, 0, 0, 0, 0, 0, 0, 0]);
        b.extend([1, 0, 0, 0, 0, 0, 0, 0]);
        b.push(1);
        b.extend([0x91; 16]);
        b.extend([0x92; 32]);
        b.extend([0; 40]);
        b
    }

    /// The claim at position 3 in epoch 2 after the append `prev` at
    /// position 2, naming it.
    fn hand_reclaim(prev: &[u8; 32]) -> Vec<u8> {
        let mut b = b"HSHIST06".to_vec();
        b.extend(prev);
        b.extend([3, 0, 0, 0, 0, 0, 0, 0]);
        b.extend([2, 0, 0, 0, 0, 0, 0, 0]);
        b.push(1);
        b.extend([7; 16]);
        b.extend([0x92; 32]);
        b.extend([2, 0, 0, 0, 0, 0, 0, 0]);
        b.extend(prev);
        b
    }

    /// An append at position `at` in epoch 1 creating one object and retiring
    /// the object of digest 0x95 created at `lifetime` in epoch 1.
    fn hand_append(prev: &[u8; 32], at: u8, lifetime: u8) -> Vec<u8> {
        let mut b = b"HSHIST06".to_vec();
        b.extend(prev);
        b.extend([at, 0, 0, 0, 0, 0, 0, 0]);
        b.extend([1, 0, 0, 0, 0, 0, 0, 0]);
        b.push(2);
        b.extend([1, 0, 0, 0]);
        b.extend([0x93; 32]);
        b.extend([1, 0, 0, 0]);
        b.extend([lifetime, 0, 0, 0, 0, 0, 0, 0]);
        b.extend([1, 0, 0, 0, 0, 0, 0, 0]);
        b.extend([0x95; 32]);
        b.extend(b"pay");
        b
    }

    /// Follows `records` from the origin: the index of the first refused.
    fn refused_at(records: &[Vec<u8>]) -> Option<(usize, Bad)> {
        let mut tip = Tip::origin();
        for (i, r) in records.iter().enumerate() {
            match follow(&tip, r) {
                Ok((next, _)) => tip = next,
                Err(b) => return Some((i, b)),
            }
        }
        None
    }

    fn classified(bad: &Bad, changed: &[u8], byte: Option<usize>) -> bool {
        match bad {
            Bad::Unsupported(found) => matches!(byte, Some(6 | 7)) && found[..] == changed[..8],
            Bad::Corrupt => !matches!(byte, Some(6 | 7)),
        }
    }

    #[test]
    fn hand_written_records_are_what_the_writers_write() {
        let claim = hand_claim();
        assert_eq!(claimed(&Tip::origin(), [0x91; 16], [0x92; 32], None), claim);
        let (tip1, r1) = follow(&Tip::origin(), &claim).unwrap();
        assert_eq!(
            r1,
            Record::Claimed {
                epoch: 1,
                attempt: [0x91; 16],
                identity: [0x92; 32],
                appended: None,
            }
        );
        assert_eq!(tip1.digest, digest(&claim));
        let retired = crate::ObjectRef {
            lifetime: 1,
            epoch: 1,
            digest: [0x95; 32],
        };
        let append = hand_append(&digest(&claim), 2, 1);
        assert_eq!(
            appended(&tip1, &[[0x93; 32]], &[retired.clone()], b"pay"),
            append
        );
        let (tip2, r2) = follow(&tip1, &append).unwrap();
        assert_eq!(
            r2,
            Record::Appended {
                payload: b"pay".to_vec(),
                created: vec![crate::ObjectRef {
                    lifetime: 2,
                    epoch: 1,
                    digest: [0x93; 32]
                }],
                retired: vec![retired]
            }
        );
        // A retirement of an object no earlier record could have created, the
        // record's own position, is refused.
        assert_eq!(
            follow(&tip1, &hand_append(&digest(&claim), 2, 2)).map(|_| ()),
            Err(Bad::Corrupt)
        );
        assert_eq!(
            (tip2.end, tip2.epoch, tip2.identity),
            (2, 1, Some([0x92; 32]))
        );
        // A later claim keeps the identity the first one minted and names
        // the newest append.
        let again = claimed(&tip2, [7; 16], [8; 32], tip2.appended);
        assert_eq!(again, hand_reclaim(&digest(&append)));
        let (_, r3) = follow(&tip2, &again).unwrap();
        assert_eq!(
            r3,
            Record::Claimed {
                epoch: 2,
                attempt: [7; 16],
                identity: [0x92; 32],
                appended: Some(crate::Locator {
                    position: 2,
                    digest: digest(&append),
                }),
            }
        );
    }

    #[test]
    fn records_that_break_a_chain_rule_are_refused() {
        let origin = Tip::origin();
        let (tip1, _) = follow(&origin, &claimed(&origin, [1; 16], [2; 32], None)).unwrap();
        let (tip2, _) = follow(&tip1, &appended(&tip1, &[], &[], b"x")).unwrap();
        let doctored = |f: &dyn Fn(&mut Tip)| {
            let mut t = tip2.clone();
            f(&mut t);
            t
        };
        let bad = [
            // Nothing before the first claim.
            (origin.clone(), appended(&origin, &[], &[], b"x")),
            // Another prev, a skipped and a repeated position.
            (
                tip2.clone(),
                appended(&doctored(&|t| t.digest = [9; 32]), &[], &[], b"x"),
            ),
            (
                tip2.clone(),
                appended(&doctored(&|t| t.end = 3), &[], &[], b"x"),
            ),
            (tip2.clone(), appended(&tip1, &[], &[], b"x")),
            // A claim that skips an epoch or names another identity.
            (
                tip2.clone(),
                claimed(&doctored(&|t| t.epoch = 2), [3; 16], [2; 32], tip2.appended),
            ),
            (
                tip2.clone(),
                claimed(
                    &doctored(&|t| t.identity = None),
                    [3; 16],
                    [4; 32],
                    tip2.appended,
                ),
            ),
            // A claim naming no append, another one, or one at another position.
            (tip2.clone(), claimed(&tip2, [3; 16], [4; 32], None)),
            (
                tip2.clone(),
                claimed(
                    &tip2,
                    [3; 16],
                    [4; 32],
                    doctored(&|t| t.appended.as_mut().unwrap().digest = [9; 32]).appended,
                ),
            ),
            (
                tip2.clone(),
                claimed(
                    &tip2,
                    [3; 16],
                    [4; 32],
                    doctored(&|t| t.appended.as_mut().unwrap().position = 1).appended,
                ),
            ),
            // An append in another epoch.
            (
                tip2.clone(),
                appended(&doctored(&|t| t.epoch = 2), &[], &[], b"x"),
            ),
        ];
        for (i, (tip, record)) in bad.iter().enumerate() {
            assert_eq!(
                follow(tip, record).map(|_| ()),
                Err(Bad::Corrupt),
                "case {i}"
            );
        }
        // Each case's twin, built from the true tip, follows.
        assert!(follow(&tip2, &appended(&tip2, &[], &[], b"x")).is_ok());
        assert!(follow(&tip2, &claimed(&tip2, [3; 16], [4; 32], tip2.appended)).is_ok());
    }

    #[test]
    fn every_flip_and_truncation_of_a_record_is_refused() {
        let origin = Tip::origin();
        let c = claimed(&origin, [5; 16], [6; 32], None);
        let (t1, _) = follow(&origin, &c).unwrap();
        let gone = crate::ObjectRef {
            lifetime: 1,
            epoch: 1,
            digest: [0x96; 32],
        };
        let a = appended(&t1, &[[0x94; 32]], &[gone], b"payload bytes");
        let (t2, _) = follow(&t1, &a).unwrap();
        let last = claimed(&t2, [7; 16], [0; 32], t2.appended);
        let records = vec![c, a, last];
        let mut chain = Chain::default();
        for r in &records {
            chain.follow(r).unwrap();
        }
        for i in 0..records.len() {
            let n = records[i].len();
            let mut changes: Vec<(Vec<u8>, Option<usize>)> = (0..n * 8)
                .map(|bit| {
                    let mut b = records[i].clone();
                    b[bit / 8] ^= 1 << (bit % 8);
                    (b, Some(bit / 8))
                })
                .collect();
            changes.extend((0..n).map(|cut| (records[i][..cut].to_vec(), None)));
            for (changed, byte) in changes {
                // Read again, every change is refused by the digest.
                assert_eq!(chain.check(i as u64 + 1, &changed), Err(Bad::Corrupt));
                // Followed, the record or the next one refuses it, except a
                // change to the last record's attempt, which no later record
                // names: there the digest a provider kept is the check.
                let mut changed_chain = records.clone();
                changed_chain[i] = changed.clone();
                match refused_at(&changed_chain) {
                    Some((at, bad)) => {
                        assert!(
                            at == i || at == i + 1,
                            "record {i} byte {byte:?} refused at {at}"
                        );
                        if at == i {
                            assert!(classified(&bad, &changed, byte), "record {i} byte {byte:?}");
                        }
                    }
                    None => assert!(
                        i == 2 && matches!(byte, Some(b) if (HEADER..HEADER + 16).contains(&b)),
                        "record {i} byte {byte:?} followed"
                    ),
                }
            }
        }
    }
}
