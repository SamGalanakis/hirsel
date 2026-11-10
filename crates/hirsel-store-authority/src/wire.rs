//! The recorded form of the seam's outcomes (determinism.md: a store's requests of its
//! authority are capability requests). A tape carries each outcome a program observed,
//! so replay rebuilds it without the provider. Integers are little-endian; a string or
//! byte string is a u64 length and its bytes.

use crate::{Advanced, AuthorityError, Error};

/// What one seam operation returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// `claim`: the claim's end; `durable`: the durable position.
    Position(Result<u64, Error>),
    Advanced(Advanced),
    Read(Result<Vec<Vec<u8>>, Error>),
    Get(Result<Option<Vec<u8>>, Error>),
    /// `put` and `reclaim`.
    Unit(Result<(), Error>),
}

pub fn encode(o: &Outcome) -> Vec<u8> {
    let mut w = Vec::new();
    match o {
        Outcome::Position(r) => {
            w.push(0);
            result(&mut w, r, |w, p| u64(w, *p));
        }
        Outcome::Advanced(a) => {
            w.push(1);
            match a {
                Advanced::Advanced(p) => {
                    w.push(0);
                    u64(&mut w, *p);
                }
                Advanced::Fenced => w.push(1),
                Advanced::Refused(e) => {
                    w.push(2);
                    medium(&mut w, e);
                }
                Advanced::Undecided(e) => {
                    w.push(3);
                    medium(&mut w, e);
                }
            }
        }
        Outcome::Read(r) => {
            w.push(2);
            result(&mut w, r, |w, all| {
                u64(w, all.len() as u64);
                for e in all {
                    bytes(w, e);
                }
            });
        }
        Outcome::Get(r) => {
            w.push(3);
            result(&mut w, r, |w, v| match v {
                None => w.push(0),
                Some(b) => {
                    w.push(1);
                    bytes(w, b);
                }
            });
        }
        Outcome::Unit(r) => {
            w.push(4);
            result(&mut w, r, |_, ()| {});
        }
    }
    w
}

/// The outcome `b` records, or `None` when it is not one `encode` writes.
pub fn decode(b: &[u8]) -> Option<Outcome> {
    let mut r = Reader(b);
    let o = match r.u8()? {
        0 => Outcome::Position(r.result(|r| r.u64())?),
        1 => Outcome::Advanced(match r.u8()? {
            0 => Advanced::Advanced(r.u64()?),
            1 => Advanced::Fenced,
            2 => Advanced::Refused(r.medium()?),
            3 => Advanced::Undecided(r.medium()?),
            _ => return None,
        }),
        2 => Outcome::Read(r.result(|r| {
            let n = r.u64()?;
            let mut all = Vec::new();
            for _ in 0..n {
                all.push(r.bytes()?.to_vec());
            }
            Some(all)
        })?),
        3 => Outcome::Get(r.result(|r| match r.u8()? {
            0 => Some(None),
            1 => Some(Some(r.bytes()?.to_vec())),
            _ => None,
        })?),
        4 => Outcome::Unit(r.result(|_| Some(()))?),
        _ => return None,
    };
    r.0.is_empty().then_some(o)
}

fn u64(w: &mut Vec<u8>, n: u64) {
    w.extend_from_slice(&n.to_le_bytes());
}

fn bytes(w: &mut Vec<u8>, b: &[u8]) {
    u64(w, b.len() as u64);
    w.extend_from_slice(b);
}

fn medium(w: &mut Vec<u8>, e: &AuthorityError) {
    match e {
        AuthorityError::Io(k) => w.extend_from_slice(&[0, *k]),
        AuthorityError::Status(s) => {
            w.push(1);
            w.extend_from_slice(&s.to_le_bytes());
        }
        AuthorityError::Unreachable => w.push(2),
    }
}

fn result<T>(w: &mut Vec<u8>, r: &Result<T, Error>, ok: impl FnOnce(&mut Vec<u8>, &T)) {
    match r {
        Ok(v) => {
            w.push(0);
            ok(w, v);
        }
        Err(e) => {
            w.push(1);
            match e {
                Error::Storage(m) => {
                    w.push(0);
                    medium(w, m);
                }
                Error::Refused(m) => {
                    w.push(1);
                    medium(w, m);
                }
                Error::Corrupt(o) => {
                    w.push(2);
                    bytes(w, o.as_bytes());
                }
                Error::Fenced => w.push(3),
                Error::Format { found, object } => {
                    w.push(4);
                    w.extend_from_slice(found);
                    bytes(w, object.as_bytes());
                }
            }
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (h, t) = self.0.split_at(n);
        self.0 = t;
        Some(h)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.u64()?).ok()?;
        self.take(n)
    }
    fn text(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }
    fn medium(&mut self) -> Option<AuthorityError> {
        Some(match self.u8()? {
            0 => AuthorityError::Io(self.u8().filter(|k| matches!(k, 0 | 3..=9))?),
            1 => AuthorityError::Status(u16::from_le_bytes(self.take(2)?.try_into().ok()?)),
            2 => AuthorityError::Unreachable,
            _ => return None,
        })
    }
    fn result<T>(&mut self, ok: impl FnOnce(&mut Self) -> Option<T>) -> Option<Result<T, Error>> {
        Some(match self.u8()? {
            0 => Ok(ok(self)?),
            1 => Err(match self.u8()? {
                0 => Error::Storage(self.medium()?),
                1 => Error::Refused(self.medium()?),
                2 => Error::Corrupt(self.text()?),
                3 => Error::Fenced,
                4 => Error::Format {
                    found: self.take(8)?.try_into().ok()?,
                    object: self.text()?,
                },
                _ => return None,
            }),
            _ => return None,
        })
    }
}
