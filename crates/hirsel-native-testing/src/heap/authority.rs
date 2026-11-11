//! Semantic authority transport, following interp/library/authority.rs and the
//! store-authority wire protocol. H alone allocates, scans and releases values.
use super::{Io, IoGrant, grant_not_granted, io_ok};
use hirsel_heapcheck::{OpOutcome, OpValue};
use hirsel_lir::layout_types::ScalarKind;
use hirsel_store_authority::{
    Authority, AuthorityError, Claim, Error, Reclaimed, Record,
    wire::{self, Outcome},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Request {
    index: usize,
    claim: Option<u64>,
    request: wire::Request,
    minted: Option<u64>,
    outcome: Outcome,
}

#[derive(Default)]
struct Live {
    providers: BTreeMap<usize, Box<dyn Authority>>,
    claims: BTreeMap<u64, (usize, Claim)>,
    attempts: u64,
}

#[derive(Default)]
pub(super) struct Authorities {
    instance: [u8; 16],
    grants: Vec<(String, hirsel_run_schema::AuthorityGrant)>,
    history: Vec<Request>,
    closed: BTreeSet<u64>,
    live: Option<Live>,
}

// A campaign fork must never share mutable providers or Claim state. Rebuild
// deterministic providers from their requests only if the fork reaches the seam.
impl Clone for Authorities {
    fn clone(&self) -> Self {
        Self {
            instance: self.instance,
            grants: self.grants.clone(),
            history: self.history.clone(),
            closed: self.closed.clone(),
            live: None,
        }
    }
}

impl Authorities {
    pub(super) fn new(
        grants: &BTreeMap<String, hirsel_run_schema::AuthorityGrant>,
        instance: [u8; 16],
    ) -> Self {
        Self {
            instance,
            grants: grants.iter().map(|(n, g)| (n.clone(), g.clone())).collect(),
            ..Self::default()
        }
    }
    fn execute(
        grants: &[(String, hirsel_run_schema::AuthorityGrant)],
        instance: &[u8; 16],
        live: &mut Live,
        r: &Request,
    ) -> Option<(Outcome, Option<Claim>)> {
        let grant = &grants.get(r.index)?.1;
        // A heap simulation has no host directory or live object service.
        if grant.provider == "authority.log@1"
            || (grant.provider == "authority.objects@1"
                && hirsel_store_authority::objects_provider(grant) != Some("objects.sim@1"))
        {
            return None;
        }
        if !live.providers.contains_key(&r.index) {
            live.providers
                .insert(r.index, hirsel_store_authority::open(grant, None).ok()?);
        }
        let claim = match r.claim {
            Some(id) => {
                let (index, claim) = live.claims.get_mut(&id)?;
                if *index != r.index {
                    return None;
                }
                Some(claim)
            }
            None => None,
        };
        let n = live.attempts;
        let answer = wire::serve(
            &mut **live.providers.get_mut(&r.index)?,
            claim,
            r.request.clone(),
            || wire::draw(instance, n),
        )
        .ok()?;
        if matches!(r.request, wire::Request::Claim) {
            live.attempts += 1;
        }
        Some(answer)
    }
    fn live(&mut self) -> Option<&mut Live> {
        if self.live.is_none() {
            let mut live = Live::default();
            for r in &self.history {
                let (outcome, claim) = Self::execute(&self.grants, &self.instance, &mut live, r)?;
                if outcome != r.outcome {
                    return None;
                }
                if let (Some(id), Some(claim)) = (r.minted, claim) {
                    live.claims.insert(id, (r.index, claim));
                }
            }
            for id in &self.closed {
                live.claims.remove(id);
            }
            self.live = Some(live);
        }
        self.live.as_mut()
    }
    /// A fork that has not rebuilt its providers closes from the history: the
    /// claims open are those its requests minted and it has not closed. Cleanup
    /// then costs no replay of every request the base served.
    pub(super) fn close(&mut self, id: u64) -> Result<(), String> {
        let open = match &mut self.live {
            Some(live) => live.claims.remove(&id).is_some(),
            None => !self.closed.contains(&id) && self.history.iter().any(|r| r.minted == Some(id)),
        };
        if !open {
            return Err(format!("model close of unknown Claim {id}"));
        }
        self.closed.insert(id);
        Ok(())
    }
    fn request(
        &mut self,
        index: usize,
        claim: Option<u64>,
        request: wire::Request,
        minted: u64,
    ) -> Option<(Outcome, bool)> {
        self.live()?;
        let mut r = Request {
            index,
            claim,
            request,
            minted: None,
            outcome: Outcome::Unit(Ok(())),
        };
        let (outcome, held) = Self::execute(&self.grants, &self.instance, self.live.as_mut()?, &r)?;
        if !r.request.admits(&outcome) {
            return None;
        }
        let opened = held.is_some();
        if let Some(held) = held {
            r.minted = Some(minted);
            self.live.as_mut()?.claims.insert(minted, (index, held));
        }
        r.outcome = outcome.clone();
        self.history.push(r);
        Some((outcome, opened))
    }
}

fn v(tag: u32, value: OpValue) -> OpValue {
    OpValue::Variant {
        tag,
        value: Box::new(value),
    }
}
fn word(n: u64) -> OpValue {
    OpValue::Bits {
        scalar: ScalarKind::U64,
        bits: n.into(),
    }
}
fn medium(e: &AuthorityError) -> OpValue {
    match e {
        AuthorityError::Io(k) => v(
            0,
            if *k == 0 {
                v(
                    0,
                    OpValue::Bits {
                        scalar: ScalarKind::U32,
                        bits: 1,
                    },
                )
            } else {
                v((*k).into(), OpValue::Unit)
            },
        ),
        AuthorityError::Status(s) => v(
            1,
            OpValue::Bits {
                scalar: ScalarKind::U16,
                bits: (*s).into(),
            },
        ),
        AuthorityError::Unreachable => v(2, OpValue::Unit),
    }
}
fn error(e: &Error) -> OpValue {
    match e {
        Error::Storage(m) => v(0, medium(m)),
        Error::Refused(m) => v(1, medium(m)),
        Error::Corrupt(o) => v(2, OpValue::ByteSequence(o.as_bytes().to_vec())),
        Error::Fenced => v(3, OpValue::Unit),
        Error::Format { found, object } => v(
            4,
            OpValue::Aggregate(vec![
                OpValue::ByteSequence(found.to_vec()),
                OpValue::ByteSequence(object.as_bytes().to_vec()),
            ]),
        ),
    }
}
fn result<T>(r: Result<T, Error>, f: impl FnOnce(T) -> OpValue) -> OpValue {
    match r {
        Ok(x) => v(0, f(x)),
        Err(e) => v(1, error(&e)),
    }
}
fn text(v: &OpValue) -> Option<&str> {
    match v {
        OpValue::Bytes(b) => std::str::from_utf8(b).ok(),
        _ => None,
    }
}
fn bits(v: &OpValue) -> Option<u64> {
    match v {
        OpValue::Bits { bits, .. } => u64::try_from(*bits).ok(),
        _ => None,
    }
}
/// An `ObjectRef` operand; `Some(None)` when its digest is not 32 bytes and it names no
/// object, which the runtime refuses before any request.
fn object_ref(v: &OpValue) -> Option<Option<hirsel_store_authority::ObjectRef>> {
    let OpValue::Aggregate(fields) = v else {
        return None;
    };
    let [lifetime, epoch, OpValue::Bytes(digest)] = fields.as_slice() else {
        return None;
    };
    let (lifetime, epoch) = (bits(lifetime)?, bits(epoch)?);
    Some(
        digest
            .as_slice()
            .try_into()
            .ok()
            .map(|digest| hirsel_store_authority::ObjectRef {
                lifetime,
                epoch,
                digest,
            }),
    )
}
fn object_refs(v: &OpValue) -> Option<Option<Vec<hirsel_store_authority::ObjectRef>>> {
    let OpValue::Sequence(all) = v else {
        return None;
    };
    all.iter()
        .map(object_ref)
        .collect::<Option<Vec<_>>>()
        .map(|all| all.into_iter().collect())
}
/// A `List[ObjectRef]` value.
fn refs(all: Vec<hirsel_store_authority::ObjectRef>) -> OpValue {
    OpValue::Sequence(
        all.into_iter()
            .map(|r| {
                OpValue::Aggregate(vec![
                    word(r.lifetime),
                    word(r.epoch),
                    OpValue::ByteSequence(r.digest.to_vec()),
                ])
            })
            .collect(),
    )
}
fn handle(v: &OpValue) -> Option<u64> {
    match v {
        OpValue::Handle(h) => Some(*h),
        _ => None,
    }
}

impl Io {
    pub(super) fn authority(
        &mut self,
        module: Option<&str>,
        name: &str,
        args: &[OpValue],
    ) -> Option<OpOutcome> {
        if !matches!(module, Some("authority" | "claim" | "level"))
            && !(module == Some("io") && name == "authority")
        {
            return None;
        }
        Some(
            self.authority_call(module?, name, args)
                .unwrap_or(OpOutcome::Unsupported),
        )
    }
    fn authority_call(&mut self, module: &str, name: &str, args: &[OpValue]) -> Option<OpOutcome> {
        let value = if module == "io" {
            let [OpValue::Handle(io), n] = args else {
                return None;
            };
            if !matches!(self.grants.get(io), Some(IoGrant::Root)) {
                return Some(OpOutcome::Fail("CAP-2".into()));
            }
            match self
                .authorities
                .grants
                .iter()
                .position(|(s, _)| Some(s.as_str()) == text(n))
            {
                Some(index) => io_ok(OpValue::Handle(self.grant(IoGrant::Authority { index }))),
                None => grant_not_granted(),
            }
        } else if module == "level" && name == "name" {
            let [l] = args else {
                return None;
            };
            let l = handle(l)?;
            let (_, g) = self.authorities.grants.get((l / 256) as usize)?;
            let (levels, _) = hirsel_store_authority::check(g).ok()?;
            OpValue::ByteSequence(levels.get((l % 256) as usize)?.as_bytes().to_vec())
        } else if module == "claim" && name == "end" {
            let [c] = args else {
                return None;
            };
            word(self.authorities.live()?.claims.get(&handle(c)?)?.1.end())
        } else {
            let (a, rest) = args.split_first()?;
            let Some(IoGrant::Authority { index }) = self.grants.get(&handle(a)?) else {
                return Some(OpOutcome::Fail("CAP-2".into()));
            };
            let index = *index;
            let (_, g) = self.authorities.grants.get(index)?;
            let (levels, floor) = hirsel_store_authority::check(g).ok()?;
            let level = |v: &OpValue| {
                let l = handle(v)?;
                ((l / 256) as usize == index && (l % 256) < levels.len() as u64)
                    .then_some((l % 256) as u8)
            };
            match (module, name, rest) {
                ("authority", "levels", []) => OpValue::Sequence(
                    (0..levels.len())
                        .map(|l| OpValue::Handle(index as u64 * 256 + l as u64))
                        .collect(),
                ),
                ("authority", "floor", []) => OpValue::Handle(index as u64 * 256 + floor.0 as u64),
                ("authority", "level", [n]) => {
                    match levels.iter().position(|s| Some(s.as_str()) == text(n)) {
                        Some(l) => v(1, OpValue::Handle(index as u64 * 256 + l as u64)),
                        None => v(0, OpValue::Unit),
                    }
                }
                ("authority", "max_entry", []) => word(hirsel_store_authority::MAX_ENTRY),
                ("level", "narrow", [l]) => {
                    if level(l).is_some() {
                        v(1, l.clone())
                    } else {
                        v(0, OpValue::Unit)
                    }
                }
                ("claim", "narrow", [c]) => {
                    let c = handle(c)?;
                    let of = self.authorities.live()?.claims.get(&c)?.0;
                    if index == of {
                        v(1, OpValue::TakenHandle { argument: 1 })
                    } else {
                        self.authorities.close(c).ok()?;
                        self.grants.remove(&c);
                        return Some(OpOutcome::Complete {
                            result: Ok(v(0, OpValue::Unit)),
                            consumed: vec![1],
                        });
                    }
                }
                ("authority", op, rest) => {
                    let no_object = || {
                        v(
                            1,
                            error(&Error::Refused(AuthorityError::io(
                                std::io::ErrorKind::InvalidInput,
                            ))),
                        )
                    };
                    let (claim, request) = match (op, rest) {
                        ("claim", []) => (None, wire::Request::Claim),
                        ("submit", [c, OpValue::Bytes(payload), created, retired, at]) => {
                            let (Some(created), Some(retired)) =
                                (object_refs(created)?, object_refs(retired)?)
                            else {
                                return Some(OpOutcome::Value(no_object()));
                            };
                            let request = wire::Request::Submit {
                                at: level(at)?,
                                payload: payload.clone(),
                                created,
                                retired,
                            };
                            (Some(handle(c)?), request)
                        }
                        ("wait", [c, p]) => {
                            (Some(handle(c)?), wire::Request::Wait { position: bits(p)? })
                        }
                        ("durable", [c, at]) => {
                            (Some(handle(c)?), wire::Request::Durable { at: level(at)? })
                        }
                        ("read", [from, at, max]) => (
                            None,
                            wire::Request::Read {
                                from: bits(from)?,
                                at: level(at)?,
                                max: bits(max)?,
                            },
                        ),
                        ("get", [r]) => match object_ref(r)? {
                            Some(r) => (None, wire::Request::Get { r }),
                            None => return Some(OpOutcome::Value(no_object())),
                        },
                        ("put", [c, r, OpValue::Bytes(data)]) => match object_ref(r)? {
                            Some(r) => (
                                Some(handle(c)?),
                                wire::Request::Put {
                                    r,
                                    data: data.clone(),
                                },
                            ),
                            None => return Some(OpOutcome::Value(no_object())),
                        },
                        ("reclaim", [c, budget]) => (
                            Some(handle(c)?),
                            wire::Request::Reclaim {
                                budget: bits(budget)?,
                            },
                        ),
                        _ => return None,
                    };
                    let minted = self.next;
                    let (outcome, opened) =
                        self.authorities.request(index, claim, request, minted)?;
                    if opened {
                        assert_eq!(self.grant(IoGrant::Claim), minted);
                    }
                    match outcome {
                        Outcome::Position(r) => result(r, |n| {
                            if opened {
                                OpValue::OpenHandle(minted)
                            } else {
                                word(n)
                            }
                        }),
                        Outcome::Read(r) => result(r, |all| {
                            OpValue::Sequence(
                                all.into_iter()
                                    .map(|r| match r {
                                        Record::Claimed {
                                            epoch,
                                            attempt,
                                            identity,
                                        } => v(
                                            0,
                                            OpValue::Aggregate(vec![
                                                word(epoch),
                                                OpValue::ByteSequence(attempt.to_vec()),
                                                OpValue::ByteSequence(identity.to_vec()),
                                            ]),
                                        ),
                                        Record::Appended {
                                            payload,
                                            created,
                                            retired,
                                        } => v(
                                            1,
                                            OpValue::Aggregate(vec![
                                                OpValue::ByteSequence(payload),
                                                refs(created),
                                                refs(retired),
                                            ]),
                                        ),
                                    })
                                    .collect(),
                            )
                        }),
                        Outcome::Get(r) => result(r, |b| match b {
                            Some(b) => v(1, OpValue::ByteSequence(b)),
                            None => v(0, OpValue::Unit),
                        }),
                        Outcome::Unit(r) => result(r, |_| OpValue::Unit),
                        Outcome::Reclaimed(r) => result(r, |r| match r {
                            Reclaimed::More => v(0, OpValue::Unit),
                            Reclaimed::Done => v(1, OpValue::Unit),
                        }),
                        Outcome::Advanced(a) => match a {
                            hirsel_store_authority::Advanced::Advanced(n) => v(0, word(n)),
                            hirsel_store_authority::Advanced::Fenced => v(1, OpValue::Unit),
                            hirsel_store_authority::Advanced::Refused(e) => v(2, medium(&e)),
                            hirsel_store_authority::Advanced::Undecided(e) => v(3, medium(&e)),
                        },
                    }
                }
                _ => return None,
            }
        };
        Some(OpOutcome::Value(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_contract::{OperationId, Passing};
    use hirsel_heapcheck::build::{CLEANUP, Layouts, fixture_unit, path};
    use hirsel_heapcheck::{Arg, Config, Program, ViolationKind};
    use hirsel_lir::{ir::*, layout_types::*, stage::Ready, testing::UnitBuilder};

    fn memory() -> Authorities {
        Authorities::new(
            &BTreeMap::from([(
                "main".into(),
                serde_json::from_value(
                    serde_json::json!({"provider":"authority.memory@1", "floor":"memory"}),
                )
                .unwrap(),
            )]),
            [0; 16],
        )
    }

    #[test]
    fn provider_forks_keep_claims_fencing_and_bytes_independent() {
        let mut a = memory();
        assert_eq!(
            a.request(0, None, wire::Request::Claim, 40),
            Some((Outcome::Position(Ok(1)), true))
        );
        let mut b = a.clone();
        let submit = wire::Request::Submit {
            at: 0,
            payload: b"second".to_vec(),
            created: vec![],
            retired: vec![],
        };
        assert_eq!(
            b.request(0, Some(40), submit, 41),
            Some((Outcome::Position(Ok(2)), false))
        );
        assert_eq!(
            b.request(0, Some(40), wire::Request::Wait { position: 2 }, 41),
            Some((
                Outcome::Advanced(hirsel_store_authority::Advanced::Advanced(2)),
                false
            ))
        );
        assert_eq!(
            a.request(0, None, wire::Request::Claim, 41),
            Some((Outcome::Position(Ok(2)), true))
        );
        assert_eq!(
            a.request(0, Some(40), wire::Request::Durable { at: 0 }, 42),
            Some((Outcome::Position(Err(Error::Fenced)), false))
        );
        assert_eq!(
            b.request(0, Some(40), wire::Request::Durable { at: 0 }, 42),
            Some((Outcome::Position(Ok(2)), false))
        );
        b.close(40).unwrap();
        assert!(b.clone().live().unwrap().claims.is_empty());
        // A fork closes without rebuilding providers; each minted Claim closes
        // once, and stays closed when a later request rebuilds them.
        let mut c = a.clone();
        c.close(40).unwrap();
        assert!(c.close(40).is_err());
        assert!(c.close(99).is_err());
        assert_eq!(
            c.request(0, Some(40), wire::Request::Durable { at: 0 }, 43),
            None
        );
        c.close(41).unwrap();
    }

    // An actual authority.read returns a claim record and an append record that names one
    // created object, whose four byte buffers and created list are materialized under H.
    // Removing its release leaks that result; duplicating it releases twice.
    #[test]
    fn authority_read_buffers_detect_missing_and_duplicate_release() {
        let mut io = Io::new();
        io.authorities = memory();
        io.authorities
            .request(0, None, wire::Request::Claim, 40)
            .unwrap();
        io.authorities
            .request(
                0,
                Some(40),
                wire::Request::Submit {
                    at: 0,
                    payload: b"payload".to_vec(),
                    created: vec![hirsel_store_authority::ObjectRef {
                        lifetime: 2,
                        epoch: 1,
                        digest: [7; 32],
                    }],
                    retired: vec![],
                },
                41,
            )
            .unwrap();
        io.authorities
            .request(0, Some(40), wire::Request::Wait { position: 2 }, 41)
            .unwrap();
        let authority = io.grant(IoGrant::Authority { index: 0 });
        for releases in [1, 0, 2] {
            let mut l = Layouts::new(TargetSpec::x86_64_unknown_linux_gnu());
            let word_layout = l.scalar(ScalarKind::U64);
            let handle_layout = l.scalar(ScalarKind::U64);
            l.get_mut(handle_layout).kind = LayoutKind::Handle;
            let byte = l.scalar(ScalarKind::U8);
            let pointer = l.scalar(ScalarKind::Pointer);
            let byte_sequence = l.record(&[word_layout]);
            let r = l.get_mut(byte_sequence);
            r.kind = LayoutKind::Sequence { element: byte };
            r.size = SizeSpec::Dynamic(vec![
                SizeTerm::Constant(16),
                SizeTerm::ReadLength {
                    offset: 0,
                    scalar: ScalarKind::U64,
                },
                SizeTerm::Add,
            ]);
            r.copy = CopyRecipe::NotCopyable;
            let bytes = l.record(&[pointer, word_layout, word_layout]);
            let r = l.get_mut(bytes);
            r.kind = LayoutKind::Slice { element: byte };
            r.scan = vec![ScanStep::SliceOwner { offset: 0 }];
            r.drop = vec![DropStep::Release { offset: 0 }];
            r.copy = CopyRecipe::Dup;
            // ObjectRef { lifetime, epoch, digest }, 40 bytes, and its List.
            let object_ref = l.record(&[word_layout, word_layout, bytes]);
            let refs = l.record(&[word_layout]);
            let r = l.get_mut(refs);
            r.kind = LayoutKind::Sequence {
                element: object_ref,
            };
            r.size = SizeSpec::Dynamic(vec![
                SizeTerm::Constant(16),
                SizeTerm::ReadLength {
                    offset: 0,
                    scalar: ScalarKind::U64,
                },
                SizeTerm::Constant(40),
                SizeTerm::Multiply,
                SizeTerm::Add,
            ]);
            r.scan = vec![ScanStep::Sequence {
                count_offset: 0,
                offset: 16,
                stride: 40,
                element: object_ref,
            }];
            r.drop = vec![DropStep::ElementsReverse {
                count_offset: 0,
                offset: 16,
                stride: 40,
                element: object_ref,
            }];
            r.copy = CopyRecipe::NotCopyable;
            let refs = l.block(refs);
            // HistoryRecord: Claimed(u64, Bytes, Bytes) |
            // Appended(Bytes, List[ObjectRef], List[ObjectRef]), 64 bytes.
            let record = l.variant(&[&[word_layout, bytes, bytes], &[bytes, refs, refs]]);
            let sequence = l.record(&[word_layout]);
            let r = l.get_mut(sequence);
            r.kind = LayoutKind::Sequence { element: record };
            r.size = SizeSpec::Dynamic(vec![
                SizeTerm::Constant(16),
                SizeTerm::ReadLength {
                    offset: 0,
                    scalar: ScalarKind::U64,
                },
                SizeTerm::Constant(64),
                SizeTerm::Multiply,
                SizeTerm::Add,
            ]);
            r.scan = vec![ScanStep::Sequence {
                count_offset: 0,
                offset: 16,
                stride: 64,
                element: record,
            }];
            r.drop = vec![DropStep::ElementsReverse {
                count_offset: 0,
                offset: 16,
                stride: 64,
                element: record,
            }];
            r.copy = CopyRecipe::NotCopyable;
            let list = l.block(sequence);
            let result_layout = l.variant(&[&[list], &[]]);
            let (target, layouts) = l.finish();
            let mut b = UnitBuilder::<Ready>::new(fixture_unit(path("main")));
            let first = b.block(BlockKind::Body, &[]);
            let failed = b.block(BlockKind::FailureCleanup, &[]);
            b.term(failed, Terminator::Propagate);
            let edge = b.edge(failed, &[]);
            let failure = b.failure(edge, None, &[]);
            let a = b.plain(ValueRepr::Handle, handle_layout);
            b.parameter(a);
            let level = b.plain(ValueRepr::Handle, handle_layout);
            b.parameter(level);
            let mut number = |n| {
                let k = b.constant(
                    ValueRepr::Scalar(ScalarKind::U64),
                    word_layout,
                    ConstantValue::Integer(n),
                );
                let v = b.plain(ValueRepr::Scalar(ScalarKind::U64), word_layout);
                b.push(
                    first,
                    Inst::Const {
                        result: v,
                        constant: k,
                    },
                );
                v
            };
            let from = number(0);
            let max = number(10);
            let result = b.owned(ValueRepr::Aggregate, result_layout);
            let op = b.operation(OperationId {
                definition: hirsel_syntax::DefPath::core("authority.read"),
                declaration_digest: Default::default(),
            });
            let call = b.call(
                op,
                &[a, from, level, max].map(|v| (Passing::Borrow, ArgumentValue::Value(v))),
                Some(result),
            );
            let next = b.block(BlockKind::Body, &[]);
            let normal = b.edge(next, &[]);
            b.term(
                first,
                Terminator::Call {
                    call,
                    normal: Some(normal),
                    failure,
                },
            );
            for _ in 0..releases {
                b.push(
                    next,
                    Inst::Release {
                        value: result,
                        layout: result_layout,
                        live: MaskRef::All,
                        anchor: CLEANUP,
                        failure,
                    },
                );
            }
            b.term(next, Terminator::Return(None));
            let program = Program {
                target,
                layouts: layouts.clone(),
                units: vec![b.finish_with_layouts(&layouts)],
                statics: vec![],
                invocations: Default::default(),
                invocation_bodies: vec![],
                pretransfer_adapters: vec![],
            };
            let config = Config {
                context_handles: vec![(authority, "authority".into()), (0, "level".into())],
                ..Config::default()
            };
            let run = hirsel_heapcheck::run_ready(
                &program,
                &path("main"),
                &[Arg::Handle(authority), Arg::Handle(0)],
                &mut io.clone(),
                &config,
            );
            match releases {
                1 => {
                    assert!(run.result.is_ok(), "{run:?}");
                    assert!(run.counters.allocations >= 3);
                }
                0 => assert_eq!(run.result.unwrap_err().kind, ViolationKind::LeakedCredit),
                2 => assert_eq!(run.result.unwrap_err().kind, ViolationKind::CreditUnderflow),
                _ => unreachable!(),
            }
        }
    }
}
