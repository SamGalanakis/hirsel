//! The recorded form of the seam's outcomes (determinism.md: a store's requests of its
//! authority are capability requests). A tape carries each outcome a program observed,
//! so replay rebuilds it without the provider. Integers are little-endian; a string or
//! byte string is a u64 length and its bytes.
//!
//! A [`Request`] is what a program asked of one authority's provider, without the
//! authority and claim it named; [`serve`] performs it and [`Request::admits`] is the
//! check every engine applies to an outcome, live or recorded.

use crate::{
    Advanced, Authority, AuthorityError, Claim, Error, Level, ObjectRef, Reclaimed, Record,
};

/// A provider request. A level is its index among the authority's declared levels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Request {
    Claim,
    Submit {
        at: u8,
        payload: Vec<u8>,
        created: Vec<ObjectRef>,
        retired: Vec<ObjectRef>,
    },
    Wait {
        position: u64,
    },
    Durable {
        at: u8,
    },
    Reclaim {
        budget: u64,
    },
    Read {
        from: u64,
        at: u8,
        max: u64,
    },
    Get {
        r: ObjectRef,
    },
    Put {
        r: ObjectRef,
        data: Vec<u8>,
    },
}

impl Request {
    /// The core operation's name.
    pub fn operation(&self) -> &'static str {
        match self {
            Request::Claim => "authority.claim",
            Request::Submit { .. } => "authority.submit",
            Request::Wait { .. } => "authority.wait",
            Request::Durable { .. } => "authority.durable",
            Request::Reclaim { .. } => "authority.reclaim",
            Request::Read { .. } => "authority.read",
            Request::Get { .. } => "authority.get",
            Request::Put { .. } => "authority.put",
        }
    }

    /// Whether the request is made with a claim.
    pub fn claimed(&self) -> bool {
        matches!(
            self,
            Request::Submit { .. }
                | Request::Wait { .. }
                | Request::Durable { .. }
                | Request::Reclaim { .. }
                | Request::Put { .. }
        )
    }

    /// The level the request names, if any.
    pub fn level(&self) -> Option<u8> {
        match self {
            Request::Submit { at, .. } | Request::Durable { at } | Request::Read { at, .. } => {
                Some(*at)
            }
            _ => None,
        }
    }

    /// Whether `o` can answer this request: its kind, a wait answered for its own
    /// position and a read within its bound. A tape or provider that breaks this is refused.
    pub fn admits(&self, o: &Outcome) -> bool {
        match (self, o) {
            (
                Request::Claim | Request::Submit { .. } | Request::Durable { .. },
                Outcome::Position(_),
            ) => true,
            (Request::Wait { position }, Outcome::Advanced(a)) => match a {
                Advanced::Advanced(p) => p == position,
                _ => true,
            },
            (Request::Read { max, .. }, Outcome::Read(r)) => {
                r.as_ref().map_or(true, |all| all.len() as u64 <= *max)
            }
            (Request::Get { .. }, Outcome::Get(_)) => true,
            (Request::Reclaim { .. }, Outcome::Reclaimed(_)) => true,
            (Request::Put { .. }, Outcome::Unit(_)) => true,
            _ => false,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::new();
        match self {
            Request::Claim => w.push(0),
            Request::Submit {
                at,
                payload,
                created,
                retired,
            } => {
                w.push(12);
                w.push(*at);
                bytes(&mut w, payload);
                refs(&mut w, created);
                refs(&mut w, retired);
            }
            Request::Wait { position } => {
                w.push(8);
                u64(&mut w, *position);
            }
            Request::Durable { at } => w.extend_from_slice(&[2, *at]),
            Request::Reclaim { budget } => {
                w.push(13);
                u64(&mut w, *budget);
            }
            Request::Read { from, at, max } => {
                w.push(4);
                u64(&mut w, *from);
                w.push(*at);
                u64(&mut w, *max);
            }
            Request::Get { r } => {
                w.push(10);
                object_ref(&mut w, r);
            }
            Request::Put { r, data } => {
                w.push(11);
                object_ref(&mut w, r);
                bytes(&mut w, data);
            }
        }
        w
    }

    pub fn decode(b: &[u8]) -> Option<Request> {
        let mut r = Reader(b);
        let q = match r.u8()? {
            0 => Request::Claim,
            // 1 was an advance, before appends were split into submit and wait;
            // 7, 5 and 6 were submit, get and put before objects had references;
            // 9 and 3 were submit and reclaim before retirement.
            12 => Request::Submit {
                at: r.u8()?,
                payload: r.bytes()?.to_vec(),
                created: r.refs()?,
                retired: r.refs()?,
            },
            8 => Request::Wait { position: r.u64()? },
            2 => Request::Durable { at: r.u8()? },
            13 => Request::Reclaim { budget: r.u64()? },
            4 => Request::Read {
                from: r.u64()?,
                at: r.u8()?,
                max: r.u64()?,
            },
            10 => Request::Get { r: r.object_ref()? },
            11 => Request::Put {
                r: r.object_ref()?,
                data: r.bytes()?.to_vec(),
            },
            _ => return None,
        };
        r.0.is_empty().then_some(q)
    }
}

/// The `n`th claim's attempt identity and the identity it mints for a new store, drawn
/// from `seed`: a simulated run's instance, so a world and its replay agree.
pub fn draw(seed: &[u8], n: u64) -> ([u8; 16], [u8; 32]) {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"hirsel.authority.claim");
    h.update(seed);
    h.update(n.to_le_bytes());
    let attempt = h.clone().finalize();
    h.update(b"store");
    (attempt[..16].try_into().unwrap(), h.finalize().into())
}

/// Perform `r` on provider `p`. `c` is the claim a claimed request names; `draw` gives a
/// claim's attempt identity and the store identity it mints. A claim's outcome comes with
/// the provider's new claim.
pub fn serve(
    p: &mut dyn Authority,
    c: Option<&mut Claim>,
    r: Request,
    draw: impl FnOnce() -> ([u8; 16], [u8; 32]),
) -> Result<(Outcome, Option<Claim>), &'static str> {
    let mut held = None;
    let claim = || c.ok_or("a claimed request without its claim");
    let o = match r {
        Request::Claim => {
            let (attempt, fresh) = draw();
            Outcome::Position(p.claim(attempt, fresh).map(|c| {
                held = Some(c.claim);
                c.end
            }))
        }
        Request::Submit {
            at,
            payload,
            created,
            retired,
        } => Outcome::Position(p.submit(claim()?, payload, &created, &retired, Level(at))),
        Request::Wait { position } => Outcome::Advanced(p.wait(claim()?, position)),
        Request::Durable { at } => Outcome::Position(p.durable(claim()?, Level(at))),
        Request::Reclaim { budget } => Outcome::Reclaimed(p.reclaim(claim()?, budget)),
        Request::Read { from, at, max } => Outcome::Read(p.read(from, Level(at), max)),
        Request::Get { r } => Outcome::Get(p.get(r)),
        Request::Put { r, data } => Outcome::Unit(p.put(claim()?, r, data)),
    };
    Ok((o, held))
}

/// What one seam operation returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// `claim`: the claim's end; `submit`: the queued position; `durable`: the durable position.
    Position(Result<u64, Error>),
    Advanced(Advanced),
    Read(Result<Vec<Record>, Error>),
    Get(Result<Option<Vec<u8>>, Error>),
    /// `put`.
    Unit(Result<(), Error>),
    Reclaimed(Result<Reclaimed, Error>),
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
                    record(w, e);
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
        Outcome::Reclaimed(r) => {
            w.push(5);
            result(&mut w, r, |w, done| {
                w.push(match done {
                    Reclaimed::More => 0,
                    Reclaimed::Done => 1,
                })
            });
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
                all.push(r.record()?);
            }
            Some(all)
        })?),
        3 => Outcome::Get(r.result(|r| match r.u8()? {
            0 => Some(None),
            1 => Some(Some(r.bytes()?.to_vec())),
            _ => None,
        })?),
        4 => Outcome::Unit(r.result(|_| Some(()))?),
        5 => Outcome::Reclaimed(r.result(|r| match r.u8()? {
            0 => Some(Reclaimed::More),
            1 => Some(Reclaimed::Done),
            _ => None,
        })?),
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

fn record(w: &mut Vec<u8>, e: &Record) {
    match e {
        Record::Claimed {
            epoch,
            attempt,
            identity,
        } => {
            w.push(0);
            u64(w, *epoch);
            w.extend_from_slice(attempt);
            w.extend_from_slice(identity);
        }
        // 1 and 2 were an appended record before it had an inventory and
        // before it had retirements.
        Record::Appended {
            payload,
            created,
            retired,
        } => {
            w.push(3);
            bytes(w, payload);
            refs(w, created);
            refs(w, retired);
        }
    }
}

fn object_ref(w: &mut Vec<u8>, r: &ObjectRef) {
    u64(w, r.lifetime);
    u64(w, r.epoch);
    w.extend_from_slice(&r.digest);
}

fn refs(w: &mut Vec<u8>, all: &[ObjectRef]) {
    u64(w, all.len() as u64);
    for r in all {
        object_ref(w, r);
    }
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
    fn record(&mut self) -> Option<Record> {
        Some(match self.u8()? {
            0 => Record::Claimed {
                epoch: self.u64()?,
                attempt: self.take(16)?.try_into().ok()?,
                identity: self.take(32)?.try_into().ok()?,
            },
            3 => Record::Appended {
                payload: self.bytes()?.to_vec(),
                created: self.refs()?,
                retired: self.refs()?,
            },
            _ => return None,
        })
    }
    fn object_ref(&mut self) -> Option<ObjectRef> {
        Some(ObjectRef {
            lifetime: self.u64()?,
            epoch: self.u64()?,
            digest: self.take(32)?.try_into().ok()?,
        })
    }
    fn refs(&mut self) -> Option<Vec<ObjectRef>> {
        let n = self.u64()?;
        // Each reference is 48 bytes: a count past what remains is no list.
        if n > (self.0.len() / 48) as u64 {
            return None;
        }
        (0..n).map(|_| self.object_ref()).collect()
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
