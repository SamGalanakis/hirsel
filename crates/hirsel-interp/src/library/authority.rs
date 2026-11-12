//! Store authorities (storage.md STORE-11; determinism.md, Authority grants). `io.authority`
//! projects a granted authority, whose provider the run makes at its first projection and
//! every later projection of the name shares. The simulation serves `authority.memory@1`
//! and `authority.sim@1`; `authority.log@1` keeps a host directory, which a simulated run
//! has none of. On a host the adapter serves every provider, as it does a native run's,
//! and hands the library each outcome (`host_authority_outcome`).
//!
//! What a provider decides (each claim, submit, wait, barrier, read and object request) is a
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
use hirsel_store_authority::{
    Advanced, Authority, AuthorityError, Claim, Error as SeamError, Locator, ObjectRef, Reclaimed,
    Record,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The run's authorities: providers by grant name, projections and claims by handle.
#[derive(Clone, Default)]
pub(super) struct Authorities {
    providers: BTreeMap<String, Arc<Mutex<Box<dyn Authority>>>>,
    /// The run's named objects.sim@1 worlds, which its providers share.
    worlds: Arc<hirsel_store_authority::Worlds>,
    /// Authority handle to grant name.
    handles: BTreeMap<u64, String>,
    claims: BTreeMap<u64, Claimed>,
    /// Claims made so far, which number each attempt's identity draw.
    attempts: u64,
    /// Seam calls that reached a provider, for `SimulationDebug::cancel_at_seam`.
    pub(super) seam_calls: u64,
    /// The host's outcome of the next provider request, when a host adapter serves the
    /// run's authorities: the request it answered and its outcome.
    host: Option<(wire::Request, Outcome)>,
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

/// An `ObjectRef` argument: its lifetime, epoch and digest, `None` when the digest is
/// not 32 bytes, a reference that names no object.
fn object_ref(v: &Value) -> Result<Option<ObjectRef>, Error> {
    if let Value::Record { definition, fields } = v
        && definition.is_core("ObjectRef")
        && let [Some(lifetime), Some(epoch), Some(digest)] = fields.as_slice()
    {
        let lifetime = u64_arg(lifetime, "lifetime")?;
        let epoch = u64_arg(epoch, "epoch")?;
        return Ok(bytes(digest)?.try_into().ok().map(|digest| ObjectRef {
            lifetime,
            epoch,
            digest,
        }));
    }
    Err(invalid("ObjectRef representation"))
}

fn object_refs(v: &Value) -> Result<Option<Vec<ObjectRef>>, Error> {
    super::collection::elements(v, hirsel_types::BuiltinType::List)?
        .iter()
        .map(object_ref)
        .collect::<Result<Vec<_>, _>>()
        .map(|all| all.into_iter().collect())
}

/// The answer to a request naming a reference that names no object: refused before
/// any request, as the provider would refuse it.
fn no_object() -> Value {
    error(seam_error(&SeamError::Refused(AuthorityError::io(
        std::io::ErrorKind::InvalidInput,
    ))))
}

fn object_ref_value(r: &ObjectRef) -> Value {
    Value::Record {
        definition: (&hirsel_syntax::DefPath::core("ObjectRef")).into(),
        fields: vec![
            Some(u64_value(r.lifetime)),
            Some(u64_value(r.epoch)),
            Some(Value::Bytes(r.digest.to_vec())),
        ],
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
        if grant.provider == "authority.objects@1"
            && hirsel_store_authority::objects_provider(grant) != Some("objects.sim@1")
        {
            return Err(Error::Unavailable(
                "authority.objects@1 here keeps its objects in objects.sim@1".into(),
            ));
        }
        let p = hirsel_store_authority::open(grant, None, &self.authorities.worlds)
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

    /// A provider request of the authority `name`, with the claim `held` for a claimed
    /// one: the recorded outcome in replay, else the provider's, recorded. The recorded
    /// input is the grant name, a zero byte and the request's wire form.
    fn request(
        &mut self,
        name: &str,
        held: Option<&Claimed>,
        r: wire::Request,
        context: &mut OperationContext<'_>,
    ) -> Result<(Outcome, Option<Claim>), Error> {
        let operation = r.operation();
        let mut input = name.as_bytes().to_vec();
        input.push(0);
        input.extend_from_slice(&r.encode());
        let mut minted = None;
        let outcome = if let Some((answered, outcome)) = self.authorities.host.take() {
            if answered != r {
                return Err(invalid("host authority outcome answers another request"));
            }
            outcome
        } else {
            match self
                .simulation
                .replay_output("Authority", operation, &input)?
            {
                Some(recorded) => wire::decode(recorded)
                    .ok_or_else(|| invalid("invalid authority tape completion"))?,
                None => {
                    let p = self.provider(name)?;
                    let claim = match held {
                        Some(h) => Some(
                            h.claim
                                .clone()
                                .ok_or_else(|| invalid("a replayed claim outside replay"))?,
                        ),
                        None => None,
                    };
                    let mut guard = claim.as_ref().map(|c| c.lock().unwrap());
                    let n = self.authorities.attempts;
                    let instance = self.simulation.config.instance;
                    let (o, c) = wire::serve(
                        &mut **p.lock().unwrap(),
                        guard.as_deref_mut(),
                        r.clone(),
                        || wire::draw(&instance, n),
                    )
                    .map_err(invalid)?;
                    minted = c;
                    o
                }
            }
        };
        if !r.admits(&outcome) {
            return Err(invalid(
                "authority tape completion does not answer its request",
            ));
        }
        if matches!(r, wire::Request::Claim) {
            self.authorities.attempts += 1;
        }
        self.simulation.record(
            "Authority",
            operation,
            input,
            wire::encode(&outcome),
            context,
        )?;
        Ok((outcome, minted))
    }

    /// The provider request an `authority.*` operation makes with these arguments, for a
    /// host adapter that serves the run's authorities; `None` for the operations the grant
    /// alone answers.
    pub fn authority_request(&self, op: &str, a: &[Value]) -> Result<Option<wire::Request>, Error> {
        let name = || self.authority_name(arg(a, 0)?);
        Ok(Some(match op {
            "authority.claim" => wire::Request::Claim,
            // A reference that names no object is answered without the provider.
            "authority.submit" => match (object_refs(arg(a, 3)?)?, object_refs(arg(a, 4)?)?) {
                (Some(created), Some(retired)) => wire::Request::Submit {
                    at: self.level_of(&name()?, arg(a, 5)?)?,
                    payload: bytes(arg(a, 2)?)?.to_vec(),
                    created,
                    retired,
                },
                _ => return Ok(None),
            },
            "authority.wait" => wire::Request::Wait {
                position: u64_arg(arg(a, 2)?, "position")?,
            },
            "authority.durable" => wire::Request::Durable {
                at: self.level_of(&name()?, arg(a, 2)?)?,
            },
            "authority.reclaim" => wire::Request::Reclaim {
                budget: u64_arg(arg(a, 2)?, "budget")?,
            },
            "authority.read" => wire::Request::Read {
                from: u64_arg(arg(a, 1)?, "from")?,
                at: self.level_of(&name()?, arg(a, 2)?)?,
                max: u64_arg(arg(a, 3)?, "max")?,
            },
            "authority.last" => wire::Request::Last {
                at: self.level_of(&name()?, arg(a, 1)?)?,
            },
            "authority.locate" => wire::Request::Locate {
                position: u64_arg(arg(a, 2)?, "position")?,
            },
            "authority.at" => match Locator::decode(bytes(arg(a, 1)?)?) {
                Some(l) => wire::Request::At {
                    l,
                    from: u64_arg(arg(a, 2)?, "from")?,
                    at: self.level_of(&name()?, arg(a, 3)?)?,
                    max: u64_arg(arg(a, 4)?, "max")?,
                },
                None => return Ok(None),
            },
            "authority.get" => match object_ref(arg(a, 1)?)? {
                Some(r) => wire::Request::Get { r },
                None => return Ok(None),
            },
            "authority.put" => match object_ref(arg(a, 2)?)? {
                Some(r) => wire::Request::Put {
                    r,
                    data: bytes(arg(a, 3)?)?.to_vec(),
                },
                None => return Ok(None),
            },
            _ => return Ok(None),
        }))
    }

    /// The host's outcome of the provider request the next `authority.*` operation makes.
    pub fn host_authority_outcome(&mut self, request: wire::Request, outcome: Outcome) {
        self.authorities.host = Some((request, outcome));
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
                let (index, levels, _) = self.levels(&self.authority_name(arg(a, 0)?)?)?;
                list(
                    (0..levels.len())
                        .map(|l| Self::level_value(index, l as u8))
                        .collect(),
                )
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
                let (outcome, held) = self.request(&name, None, wire::Request::Claim, context)?;
                let Outcome::Position(r) = outcome else {
                    unreachable!("admitted")
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
            "authority.submit" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let (_, held) = self.claim_arg(&name, arg(a, 1)?)?;
                let payload = bytes(arg(a, 2)?)?.to_vec();
                let at = self.level_of(&name, arg(a, 5)?)?;
                match (object_refs(arg(a, 3)?)?, object_refs(arg(a, 4)?)?) {
                    (Some(created), Some(retired)) => {
                        let r = wire::Request::Submit {
                            at,
                            payload,
                            created,
                            retired,
                        };
                        let Outcome::Position(r) = self.request(&name, Some(&held), r, context)?.0
                        else {
                            unreachable!("admitted")
                        };
                        result(&r, |p| u64_value(*p))
                    }
                    _ => no_object(),
                }
            }
            "authority.wait" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let (id, held) = self.claim_arg(&name, arg(a, 1)?)?;
                let position = u64_arg(arg(a, 2)?, "position")?;
                let r = wire::Request::Wait { position };
                let (outcome, _) = self.request(&name, Some(&held), r, context)?;
                let Outcome::Advanced(r) = outcome else {
                    unreachable!("admitted")
                };
                match r {
                    Advanced::Advanced(p) => {
                        // Waits may be answered newest first: the end never moves back.
                        let claimed = self.authorities.claims.get_mut(&id).unwrap();
                        claimed.end = claimed.end.max(p);
                        variant(0, vec![u64_value(p)])
                    }
                    Advanced::Fenced => variant(1, vec![]),
                    Advanced::Refused(e) => variant(2, vec![medium(&e)]),
                    Advanced::Undecided(e) => variant(3, vec![medium(&e)]),
                }
            }
            "authority.durable" | "authority.reclaim" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let (_, held) = self.claim_arg(&name, arg(a, 1)?)?;
                let r = if op == "authority.durable" {
                    wire::Request::Durable {
                        at: self.level_of(&name, arg(a, 2)?)?,
                    }
                } else {
                    wire::Request::Reclaim {
                        budget: u64_arg(arg(a, 2)?, "budget")?,
                    }
                };
                match self.request(&name, Some(&held), r, context)?.0 {
                    Outcome::Position(r) => result(&r, |p| u64_value(*p)),
                    Outcome::Reclaimed(r) => result(&r, |done| match done {
                        Reclaimed::More => variant(0, vec![]),
                        Reclaimed::Done => variant(1, vec![]),
                    }),
                    _ => unreachable!("admitted"),
                }
            }
            "authority.last" | "authority.locate" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let (held, r) = if op == "authority.locate" {
                    let (_, held) = self.claim_arg(&name, arg(a, 1)?)?;
                    let position = u64_arg(arg(a, 2)?, "position")?;
                    (Some(held), wire::Request::Locate { position })
                } else {
                    let at = self.level_of(&name, arg(a, 1)?)?;
                    (None, wire::Request::Last { at })
                };
                let Outcome::Located(r) = self.request(&name, held.as_ref(), r, context)?.0 else {
                    unreachable!("admitted")
                };
                // No locator is 40 zero bytes, the encoding a record gives it.
                result(&r, |l| {
                    Value::Bytes(l.unwrap_or_default().encode().to_vec())
                })
            }
            "authority.read" | "authority.at" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let r = if op == "authority.at" {
                    let Some(l) = Locator::decode(bytes(arg(a, 1)?)?) else {
                        return Ok(OperationResult::returned(no_object()));
                    };
                    wire::Request::At {
                        l,
                        from: u64_arg(arg(a, 2)?, "from")?,
                        at: self.level_of(&name, arg(a, 3)?)?,
                        max: u64_arg(arg(a, 4)?, "max")?,
                    }
                } else {
                    wire::Request::Read {
                        from: u64_arg(arg(a, 1)?, "from")?,
                        at: self.level_of(&name, arg(a, 2)?)?,
                        max: u64_arg(arg(a, 3)?, "max")?,
                    }
                };
                let Outcome::Read(r) = self.request(&name, None, r, context)?.0 else {
                    unreachable!("admitted")
                };
                result(&r, |all| {
                    list(
                        all.iter()
                            .map(|e| match e {
                                Record::Claimed {
                                    epoch,
                                    attempt,
                                    identity,
                                    ..
                                } => variant(
                                    0,
                                    vec![
                                        u64_value(*epoch),
                                        Value::Bytes(attempt.to_vec()),
                                        Value::Bytes(identity.to_vec()),
                                    ],
                                ),
                                Record::Appended {
                                    payload,
                                    created,
                                    retired,
                                } => variant(
                                    1,
                                    vec![
                                        Value::Bytes(payload.clone()),
                                        list(created.iter().map(object_ref_value).collect()),
                                        list(retired.iter().map(object_ref_value).collect()),
                                    ],
                                ),
                            })
                            .collect(),
                    )
                })
            }
            "authority.get" | "authority.put" => {
                let name = self.authority_name(arg(a, 0)?)?;
                let (held, r) = if op == "authority.put" {
                    let (_, held) = self.claim_arg(&name, arg(a, 1)?)?;
                    let data = bytes(arg(a, 3)?)?.to_vec();
                    let r = object_ref(arg(a, 2)?)?.map(|r| wire::Request::Put { r, data });
                    (Some(held), r)
                } else {
                    let r = object_ref(arg(a, 1)?)?.map(|r| wire::Request::Get { r });
                    (None, r)
                };
                let Some(r) = r else {
                    return Ok(OperationResult::returned(no_object()));
                };
                match self.request(&name, held.as_ref(), r, context)?.0 {
                    Outcome::Get(r) => result(&r, |v| {
                        v.as_ref()
                            .map_or_else(none, |b| some(Value::Bytes(b.clone())))
                    }),
                    Outcome::Unit(r) => result(&r, |()| Value::Unit),
                    _ => unreachable!("admitted"),
                }
            }
            _ => return Err(invalid(format!("unknown authority operation {op}"))),
        };
        let result = OperationResult::returned(value);
        // A narrowed claim is the taken one: it moves into the result, never dropped.
        Ok(if op == "claim.narrow" {
            result.transferring([1])
        } else {
            result
        })
    }

    /// A dropped claim ends.
    pub(super) fn drop_claim(&mut self, id: u64) {
        self.authorities.claims.remove(&id);
    }
}
