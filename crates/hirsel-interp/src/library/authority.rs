//! Store authorities (storage.md STORE-11; determinism.md, Authority grants). `io.authority`
//! projects a granted authority, whose provider the run makes at its first projection and
//! every later projection of the name shares. The simulation serves `authority.memory@1`
//! and `authority.sim@1`; `authority.log@1` keeps a host directory, which a simulated run
//! has none of.
//!
//! What a provider decides (each claim, advance, barrier, read and object request) is a
//! recorded capability input of `Authority`: the request is the grant name and the
//! operation's arguments, the completion `hirsel_store_authority::wire`'s outcome. Replay
//! rebuilds every outcome from the tape and never makes a provider. The levels, floor and
//! entry bound are the grant's, part of the run's identity, and are not recorded.
//!
//! A `Level of a` is a `Level` handle whose id is the authority's index among the run's
//! granted names times 256 plus the level's index among the levels it declares; so a
//! level narrows to every projection of the authority that declared it, and to no other.
use super::Library;
use super::dispatch::{arg, bytes, error, none, ok, some, text, u32_value, u64_value, variant};
use crate::{Error, OperationCall, OperationContext, OperationResult, Value, invalid};
use hirsel_store_authority::wire::{self, Outcome};
use hirsel_store_authority::{Advanced, Authority, AuthorityError, Claim, Error as SeamError};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The run's authorities: providers by grant name, projections and claims by handle.
#[derive(Clone, Default)]
pub(super) struct Authorities {
    providers: BTreeMap<String, Arc<Mutex<Box<dyn Authority>>>>,
    /// Authority handle to grant name.
    handles: BTreeMap<u64, String>,
    claims: BTreeMap<u64, Claimed>,
    /// Claims made so far, which number each attempt's identity draw.
    attempts: u64,
}

/// A claim the program holds: its authority, its end and, outside replay, the provider's
/// claim. A claim a provider fenced, or one replay rebuilt, keeps its end.
#[derive(Clone)]
struct Claimed {
    name: String,
    end: u64,
    claim: Option<Arc<Mutex<Claim>>>,
}

fn u64_arg(v: &Value, what: &str) -> Result<u64, Error> {
    match v {
        Value::Integer(n) if n.kind() == hirsel_types::Primitive::U64 => Ok(n.bits() as u64),
        _ => Err(invalid(format!("{what} is a u64"))),
    }
}

/// The `FileError` value a portable file condition names.
fn file_error(k: u8) -> Value {
    if k == 0 {
        variant(0, vec![u32_value(1)])
    } else {
        variant(k as u32, vec![])
    }
}

fn medium(e: &AuthorityError) -> Value {
    match e {
        AuthorityError::Io(k) => variant(0, vec![file_error(*k)]),
        AuthorityError::Status(s) => variant(
            1,
            vec![Value::Integer(
                crate::Integer::new(hirsel_types::Primitive::U16, *s as u128).unwrap(),
            )],
        ),
        AuthorityError::Unreachable => variant(2, vec![]),
    }
}

/// The `SeamError` value of a provider error, in its declared variant order.
fn seam_error(e: &SeamError) -> Value {
    match e {
        SeamError::Storage(m) => variant(0, vec![medium(m)]),
        SeamError::Refused(m) => variant(1, vec![medium(m)]),
        SeamError::Corrupt(o) => variant(2, vec![Value::String(o.clone())]),
        SeamError::Fenced => variant(3, vec![]),
        SeamError::Format { found, object } => variant(
            4,
            vec![Value::Bytes(found.to_vec()), Value::String(object.clone())],
        ),
    }
}

fn result<T>(r: &Result<T, SeamError>, value: impl FnOnce(&T) -> Value) -> Value {
    match r {
        Ok(v) => ok(value(v)),
        Err(e) => error(seam_error(e)),
    }
}

fn list(elements: Vec<Value>) -> Value {
    Value::Sequence {
        kind: hirsel_types::BuiltinType::List,
        elements: elements.into_iter().map(Some).collect(),
        arena: None,
    }
}

impl Library {
    fn grant(&self, name: &str) -> Option<(usize, &hirsel_run_schema::AuthorityGrant)> {
        let grants = &self.simulation.config.authorities;
        grants
            .iter()
            .enumerate()
            .find(|(_, (n, _))| *n == name)
            .map(|(i, (_, g))| (i, g))
    }

    /// The grant name an `Authority` argument projects.
    fn authority_name(&self, v: &Value) -> Result<String, Error> {
        let id = self.simulation.id(v, "Authority")?;
        self.authorities
            .handles
            .get(&id)
            .cloned()
            .ok_or_else(|| invalid("unknown Authority"))
    }

    /// The declared levels and floor of a granted name.
    fn levels(&self, name: &str) -> Result<(usize, Vec<String>, u8), Error> {
        let (index, grant) = self
            .grant(name)
            .ok_or_else(|| invalid("unknown authority grant"))?;
        let (levels, floor) = hirsel_store_authority::check(grant)
            .map_err(|e| invalid(format!("authority grant: {}", e.0)))?;
        Ok((index, levels, floor.0))
    }

    fn level_value(index: usize, level: u8) -> Value {
        Value::Handle {
            kind: "Level".into(),
            id: index as u64 * 256 + level as u64,
        }
    }

    /// A `Level` argument's authority index and level.
    fn level_arg(v: &Value) -> Result<(usize, u8), Error> {
        match v {
            Value::Handle { kind, id } if kind == "Level" => {
                Ok(((id / 256) as usize, (id % 256) as u8))
            }
            _ => Err(invalid("invalid Level representation")),
        }
    }

    /// A level of the authority `name`; the brand makes any other a checker bug.
    fn level_of(&self, name: &str, v: &Value) -> Result<u8, Error> {
        let (index, levels, _) = self.levels(name)?;
        let (i, l) = Self::level_arg(v)?;
        if i != index || l as usize >= levels.len() {
            return Err(invalid("a Level of another authority"));
        }
        Ok(l)
    }

    fn provider(&mut self, name: &str) -> Result<Arc<Mutex<Box<dyn Authority>>>, Error> {
        if let Some(p) = self.authorities.providers.get(name) {
            return Ok(p.clone());
        }
        let (_, grant) = self
            .grant(name)
            .ok_or_else(|| invalid("unknown authority grant"))?;
        if grant.provider == "authority.log@1" {
            return Err(Error::Unavailable(
                "authority.log@1 keeps a host directory; a simulated run is served by authority.memory@1 or authority.sim@1"
                    .into(),
            ));
        }
        let p = hirsel_store_authority::open(grant, None)
            .map_err(|e| invalid(format!("authority grant: {}", e.0)))?;
        let p = Arc::new(Mutex::new(p));
        self.authorities
            .providers
            .insert(name.to_string(), p.clone());
        Ok(p)
    }

    /// The claim a `Claim` argument holds, which must be of the authority `name`.
    fn claim_arg(&self, name: &str, v: &Value) -> Result<(u64, Claimed), Error> {
        let Value::Handle { kind, id } = v else {
            return Err(invalid("invalid Claim representation"));
        };
        let claimed = self
            .authorities
            .claims
            .get(id)
            .filter(|c| kind == "Claim" && c.name == name)
            .ok_or_else(|| invalid("a Claim of another authority"))?;
        Ok((*id, claimed.clone()))
    }

    /// A provider request: the recorded outcome in replay, else the provider's, recorded.
    fn request(
        &mut self,
        operation: &str,
        input: Vec<u8>,
        context: &mut OperationContext<'_>,
        live: impl FnOnce(&mut Self) -> Result<Outcome, Error>,
    ) -> Result<Outcome, Error> {
        let outcome = match self
            .simulation
            .replay_output("Authority", operation, &input)?
        {
            Some(recorded) => wire::decode(recorded)
                .ok_or_else(|| invalid("invalid authority tape completion"))?,
            None => live(self)?,
        };
        self.simulation.record(
            "Authority",
            operation,
            input,
            wire::encode(&outcome),
            context,
        )?;
        Ok(outcome)
    }

    pub(super) fn authority_call(
        &mut self,
        op: &str,
        call: &OperationCall<'_>,
        context: &mut OperationContext<'_>,
    ) -> Result<OperationResult, Error> {
        let a = &*call.arguments;
        let value = match op {
            "io.authority" => {
                self.simulation.id(arg(a, 0)?, "Io")?;
                let name = text(arg(a, 1)?)?.to_string();
                if self.grant(&name).is_none() {
                    error(variant(0, vec![]))
                } else {
                    let handle = self.simulation.handle(
                        "Authority",
                        super::simulation::Grant::Root("Authority".into()),
                    );
                    let Value::Handle { id, .. } = &handle else {
                        unreachable!()
                    };
                    self.authorities.handles.insert(*id, name);
                    ok(handle)
                }
            }
            "authority.levels" => {
                let (_, levels, _) = self.levels(&self.authority_name(arg(a, 0)?)?)?;
                list(levels.into_iter().map(Value::String).collect())
            }
            "authority.level" => {
                let (index, levels, _) = self.levels(&self.authority_name(arg(a, 0)?)?)?;
                let wanted = text(arg(a, 1)?)?;
                match levels.iter().position(|l| l == wanted) {
                    Some(l) => some(Self::level_value(index, l as u8)),
                    None => none(),
                }
            }
            "authority.floor" => {
                let (index, _, floor) = self.levels(&self.authority_name(arg(a, 0)?)?)?;
                Self::level_value(index, floor)
            }
            "level.name" => {
                let (index, level) = Self::level_arg(arg(a, 0)?)?;
                let name = self
                    .simulation
                    .config
                    .authorities
                    .keys()
                    .nth(index)
                    .cloned()
                    .ok_or_else(|| invalid("a Level of no authority"))?;
                let (_, levels, _) = self.levels(&name)?;
                Value::String(
                    levels
                        .get(level as usize)
                        .cloned()
                        .ok_or_else(|| invalid("invalid Level"))?,
                )
            }
            "level.narrow" => {
                let (index, _, _) = self.levels(&self.authority_name(arg(a, 0)?)?)?;
                let l = arg(a, 1)?;
                if Self::level_arg(l)?.0 == index {
                    some(l.clone())
                } else {
                    none()
                }
            }
            "authority.max_entry" => {
                self.authority_name(arg(a, 0)?)?;
                u64_value(hirsel_store_authority::MAX_ENTRY)
            }
            "claim.end" => {
                let Value::Handle { id, .. } = arg(a, 0)? else {
                    return Err(invalid("invalid Claim representation"));
                };
                u64_value(
                    self.authorities
                        .claims
                        .get(id)
                        .ok_or_else(|| invalid("unknown Claim"))?
                        .end,
                )
            }
            "claim.narrow" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let c = arg(a, 1)?;
                let Value::Handle { id, .. } = c else {
                    return Err(invalid("invalid Claim representation"));
                };
                let of = self
                    .authorities
                    .claims
                    .get(id)
                    .ok_or_else(|| invalid("unknown Claim"))?;
                if of.name == name {
                    some(c.clone())
                } else {
                    // The claim was taken: refused, it ends here.
                    self.authorities.claims.remove(id);
                    none()
                }
            }
            "authority.claim" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let n = self.authorities.attempts;
                self.authorities.attempts += 1;
                let mut held = None;
                let outcome = self.request(
                    "authority.claim",
                    name.as_bytes().to_vec(),
                    context,
                    |lib| {
                        let p = lib.provider(&name)?;
                        let (attempt, fresh) = lib.draw(n);
                        let r = p.lock().unwrap().claim(attempt, fresh);
                        Ok(Outcome::Position(r.map(|c| {
                            held = Some(c.claim);
                            c.end
                        })))
                    },
                )?;
                let Outcome::Position(r) = outcome else {
                    return Err(invalid("authority tape completion of another operation"));
                };
                match r {
                    Ok(end) => {
                        let handle = self
                            .simulation
                            .handle("Claim", super::simulation::Grant::Root("Claim".into()));
                        let Value::Handle { id, .. } = &handle else {
                            unreachable!()
                        };
                        let claim = held.map(|c| Arc::new(Mutex::new(c)));
                        self.authorities
                            .claims
                            .insert(*id, Claimed { name, end, claim });
                        ok(handle)
                    }
                    Err(e) => error(seam_error(&e)),
                }
            }
            "authority.advance" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let (id, held) = self.claim_arg(&name, arg(a, 1)?)?;
                let from = u64_arg(arg(a, 2)?, "from")?;
                let entry = bytes(arg(a, 3)?)?.to_vec();
                let at = self.level_of(&name, arg(a, 4)?)?;
                let mut input = name.as_bytes().to_vec();
                input.push(0);
                input.extend_from_slice(&u64::to_le_bytes(from));
                input.push(at);
                input.extend_from_slice(&entry);
                let outcome = self.request("authority.advance", input, context, |lib| {
                    let p = lib.provider(&name)?;
                    let c = held
                        .claim
                        .as_ref()
                        .ok_or_else(|| invalid("a replayed claim outside replay"))?;
                    let r = p.lock().unwrap().advance(
                        &mut c.lock().unwrap(),
                        from,
                        entry,
                        hirsel_store_authority::Level(at),
                    );
                    Ok(Outcome::Advanced(r))
                })?;
                let Outcome::Advanced(r) = outcome else {
                    return Err(invalid("authority tape completion of another operation"));
                };
                match r {
                    Advanced::Advanced(end) => {
                        if end != from + 1 {
                            return Err(invalid(
                                "authority tape completion advances past one entry",
                            ));
                        }
                        self.authorities.claims.get_mut(&id).unwrap().end = end;
                        variant(0, vec![u64_value(end)])
                    }
                    Advanced::Fenced => variant(1, vec![]),
                    Advanced::Refused(e) => variant(2, vec![medium(&e)]),
                    Advanced::Undecided(e) => variant(3, vec![medium(&e)]),
                }
            }
            "authority.durable" | "authority.reclaim" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let (_, held) = self.claim_arg(&name, arg(a, 1)?)?;
                let at = if op == "authority.durable" {
                    Some(self.level_of(&name, arg(a, 2)?)?)
                } else {
                    None
                };
                let mut input = name.as_bytes().to_vec();
                input.extend(at.map(|l| [0, l]).into_iter().flatten());
                let outcome = self.request(op, input, context, |lib| {
                    let p = lib.provider(&name)?;
                    let c = held
                        .claim
                        .as_ref()
                        .ok_or_else(|| invalid("a replayed claim outside replay"))?;
                    let mut c = c.lock().unwrap();
                    Ok(match at {
                        Some(l) => Outcome::Position(
                            p.lock()
                                .unwrap()
                                .durable(&mut c, hirsel_store_authority::Level(l)),
                        ),
                        None => Outcome::Unit(p.lock().unwrap().reclaim(&mut c)),
                    })
                })?;
                match outcome {
                    Outcome::Position(r) if at.is_some() => result(&r, |p| u64_value(*p)),
                    Outcome::Unit(r) if at.is_none() => result(&r, |()| Value::Unit),
                    _ => return Err(invalid("authority tape completion of another operation")),
                }
            }
            "authority.read" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let from: u64 = u64_arg(arg(a, 1)?, "from")?;
                let at = self.level_of(&name, arg(a, 2)?)?;
                let max: u64 = u64_arg(arg(a, 3)?, "max")?;
                let mut input = name.as_bytes().to_vec();
                input.push(0);
                input.extend_from_slice(&from.to_le_bytes());
                input.push(at);
                input.extend_from_slice(&max.to_le_bytes());
                let outcome = self.request(op, input, context, |lib| {
                    let p = lib.provider(&name)?;
                    let r = p
                        .lock()
                        .unwrap()
                        .read(from, hirsel_store_authority::Level(at), max);
                    Ok(Outcome::Read(r))
                })?;
                let Outcome::Read(r) = outcome else {
                    return Err(invalid("authority tape completion of another operation"));
                };
                if r.as_ref().is_ok_and(|all| all.len() as u64 > max) {
                    return Err(invalid("authority tape completion reads past its bound"));
                }
                result(&r, |all| {
                    list(all.iter().map(|e| Value::Bytes(e.clone())).collect())
                })
            }
            "authority.get" | "authority.put" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let object = text(arg(a, 1)?)?.to_string();
                let data = if op == "authority.put" {
                    Some(bytes(arg(a, 2)?)?.to_vec())
                } else {
                    None
                };
                let mut input = name.as_bytes().to_vec();
                input.push(0);
                input.extend_from_slice(object.as_bytes());
                if let Some(d) = &data {
                    input.push(0);
                    input.extend_from_slice(d);
                }
                let outcome = self.request(op, input, context, |lib| {
                    let p = lib.provider(&name)?;
                    let mut p = p.lock().unwrap();
                    Ok(match data {
                        Some(d) => Outcome::Unit(p.put(&object, d)),
                        None => Outcome::Get(p.get(&object)),
                    })
                })?;
                match outcome {
                    Outcome::Get(r) if op == "authority.get" => result(&r, |v| {
                        v.as_ref()
                            .map_or_else(none, |b| some(Value::Bytes(b.clone())))
                    }),
                    Outcome::Unit(r) if op == "authority.put" => result(&r, |()| Value::Unit),
                    _ => return Err(invalid("authority tape completion of another operation")),
                }
            }
            _ => return Err(invalid(format!("unknown authority operation {op}"))),
        };
        Ok(OperationResult::returned(value))
    }

    /// The `n`th claim's attempt identity and the identity it mints for a new store:
    /// draws from the run's instance, so a world and its replay agree.
    fn draw(&self, n: u64) -> ([u8; 16], [u8; 32]) {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"hirsel.authority.claim");
        h.update(self.simulation.config.instance);
        h.update(n.to_le_bytes());
        let attempt = h.clone().finalize();
        h.update(b"store");
        (attempt[..16].try_into().unwrap(), h.finalize().into())
    }

    /// A dropped claim ends.
    pub(super) fn drop_claim(&mut self, id: u64) {
        self.authorities.claims.remove(&id);
    }
}
