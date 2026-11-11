//! Serial db@1 calls, using the interpreter's places.rs request protocol.
//! A transcript reconstructs the head's suspended prefix in a separate H heap.
//! At the next empty inbox, inject a failure and audit its entire cleanup. The
//! conceptual head stays suspended; no production allocator or drop helper runs.
use super::{HeapWorld, Io, IoGrant, grant_not_granted, io_ok};
use hirsel_heapcheck::{Arg, Config, INJECTED_RULE, OpOutcome, OpValue, Program};
use hirsel_lir::stage::Ready;
use hirsel_run_schema::{CallForm, CalleeFacts, RunnerEntry};
use hirsel_syntax::DefPath;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Default)]
pub(super) struct Runners {
    pub(super) entries: Vec<RunnerEntry>,
    pub(super) facts: Arc<BTreeMap<String, CalleeFacts>>,
    pub(super) program: Option<Arc<Program<Ready>>>,
    pub(super) config: Option<Arc<Config>>,
    places: BTreeMap<String, Place>,
    applied: BTreeMap<String, u64>,
}
#[derive(Clone, Default)]
struct Place {
    requests: Vec<OpValue>,
    ends: Vec<OpValue>,
}
#[derive(Clone, Default)]
pub(super) struct Head {
    requests: Vec<OpValue>,
    next: usize,
    taken: BTreeMap<u64, usize>,
    ends: Vec<Option<OpValue>>,
    inbox: bool,
    exhausted: bool,
}
fn v(tag: u32, value: OpValue) -> OpValue {
    OpValue::Variant {
        tag,
        value: Box::new(value),
    }
}
fn word(n: u64) -> OpValue {
    OpValue::Bits {
        scalar: hirsel_lir::layout_types::ScalarKind::U64,
        bits: n.into(),
    }
}
fn bytes(b: impl Into<Vec<u8>>) -> OpValue {
    OpValue::ByteSequence(b.into())
}
fn unsupported(why: impl std::fmt::Display) -> OpOutcome {
    OpOutcome::Refused(format!("runner heap model: {why}"))
}
fn fresh(value: &OpValue) -> OpValue {
    match value {
        OpValue::Bytes(b) => bytes(b.clone()),
        OpValue::Variant { tag, value } => v(*tag, fresh(value)),
        OpValue::Aggregate(fields) => OpValue::Aggregate(fields.iter().map(fresh).collect()),
        OpValue::Sequence(items) => OpValue::Sequence(items.iter().map(fresh).collect()),
        other => other.clone(),
    }
}
fn admitted(runner: &RunnerEntry, tag: u32, path: &DefPath) -> bool {
    runner.provider == "sim@1"
        && runner.models.as_deref() == Some("db@1")
        && matches!(tag, 2 | 3)
        && runner
            .definitions
            .as_ref()
            .is_none_or(|ds| ds.contains(&path.to_string()))
        && runner.head.is_some()
}

fn entry_id(path: &str, facts: &BTreeMap<String, CalleeFacts>) -> Option<OpValue> {
    let f = facts.get(path)?;
    Some(OpValue::Aggregate(vec![
        bytes(path.as_bytes()),
        bytes(f.fingerprint),
        bytes(f.code),
    ]))
}

impl Io {
    pub(super) fn runner_canceled(&mut self, args: &[OpValue], cause: u32) -> Option<OpOutcome> {
        let [
            OpValue::Handle(handle),
            OpValue::Variant { tag, .. },
            OpValue::Function(symbol),
            OpValue::Bytes(_),
        ] = args
        else {
            return None;
        };
        let Some(IoGrant::Runner { index }) = self.grants.get(handle) else {
            return Some(OpOutcome::Fail("CAP-2".into()));
        };
        let runner = self.runners.entries.get(*index)?;
        self.yielded = true;
        Some(OpOutcome::Value(
            if admitted(runner, *tag, &symbol.definition) {
                v(5, v(cause, OpValue::Unit))
            } else {
                v(3, v(0, OpValue::Unit))
            },
        ))
    }

    pub(super) fn runner(
        &mut self,
        module: Option<&str>,
        name: &str,
        args: &[OpValue],
    ) -> Option<OpOutcome> {
        if !matches!(module, Some("runner" | "inbox" | "arrival"))
            && !(module == Some("io") && matches!(name, "runner" | "inbox"))
        {
            return None;
        }
        Some(
            self.runner_call(module?, name, args)
                .unwrap_or_else(|| unsupported(format!("unmodeled {module:?}.{name} operands"))),
        )
    }
    fn runner_call(&mut self, module: &str, name: &str, args: &[OpValue]) -> Option<OpOutcome> {
        let value = match (module, name, args) {
            ("io", "runner", [OpValue::Handle(io), OpValue::Bytes(name)])
                if matches!(self.grants.get(io), Some(IoGrant::Root)) =>
            {
                let name = std::str::from_utf8(name).ok()?;
                match self.runners.entries.iter().position(|e| e.name == name) {
                    Some(index) => io_ok(OpValue::Handle(self.grant(IoGrant::Runner { index }))),
                    None => grant_not_granted(),
                }
            }
            ("runner", "entry_id", [OpValue::Function(symbol)]) => {
                entry_id(&symbol.definition.to_string(), &self.runners.facts)?
            }
            (
                "runner",
                "send",
                [
                    OpValue::Handle(r),
                    OpValue::Variant { tag, .. },
                    OpValue::Function(symbol),
                    OpValue::Bytes(argument),
                ],
            ) => {
                let Some(IoGrant::Runner { index }) = self.grants.get(r) else {
                    return Some(OpOutcome::Fail("CAP-2".into()));
                };
                let runner = self.runners.entries.get(*index)?.clone();
                return Some(self.place_send(&runner, *tag, &symbol.definition, argument));
            }
            ("io", "inbox", [OpValue::Handle(io)])
                if matches!(self.grants.get(io), Some(IoGrant::Root)) =>
            {
                if let Some(head) = self.head.as_mut().filter(|h| !h.inbox) {
                    head.inbox = true;
                    io_ok(OpValue::OpenHandle(self.grant(IoGrant::Inbox)))
                } else {
                    grant_not_granted()
                }
            }
            ("inbox", "take", [OpValue::Handle(inbox), OpValue::Bits { bits: wait, .. }])
                if matches!(self.grants.get(inbox), Some(IoGrant::Inbox)) =>
            {
                let head = self.head.as_mut()?;
                if head.next == head.requests.len() {
                    if *wait == 0 {
                        v(1, OpValue::Sequence(vec![]))
                    } else {
                        head.exhausted = true;
                        return Some(OpOutcome::Fail(INJECTED_RULE.into()));
                    }
                } else {
                    let ordinal = head.next;
                    head.next += 1;
                    let handle = self.grant(IoGrant::Arrival);
                    self.head.as_mut()?.taken.insert(handle, ordinal);
                    v(1, OpValue::Sequence(vec![OpValue::OpenHandle(handle)]))
                }
            }
            ("arrival", "request", [OpValue::Handle(arrival)]) => {
                let head = self.head.as_ref()?;
                head.requests.get(*head.taken.get(arrival)?)?.clone()
            }
            ("arrival", "end", [OpValue::Handle(arrival), end]) => {
                let head = self.head.as_mut()?;
                let ordinal = head.taken.remove(arrival)?;
                if head.ends.get(ordinal)?.is_some() {
                    return Some(unsupported("arrival ended twice"));
                }
                head.ends[ordinal] = Some(fresh(end));
                self.grants.remove(arrival)?;
                return Some(OpOutcome::Complete {
                    result: Ok(OpValue::Unit),
                    consumed: vec![0],
                });
            }
            _ => return None,
        };
        Some(OpOutcome::Value(value))
    }

    fn place_send(
        &mut self,
        runner: &RunnerEntry,
        tag: u32,
        path: &DefPath,
        argument: &[u8],
    ) -> OpOutcome {
        let full_path = path.to_string();
        if !admitted(runner, tag, path) {
            return OpOutcome::Value(v(3, v(0, OpValue::Unit)));
        }
        let grants = runner.grants.clone().unwrap_or_default();
        // Prefix reconstruction requires a deterministic, isolated head. Other
        // providers and concurrent/cross-run effects remain refused, never guessed.
        if grants
            != (hirsel_run_schema::IoGrants {
                authorities: grants.authorities.clone(),
                ..Default::default()
            })
            || !runner.faults.is_empty()
        {
            return unsupported(
                "db@1 head reconstruction supports authority-only grants without runner faults",
            );
        }
        let Some(program) = self.runners.program.clone() else {
            return unsupported("no checked head program");
        };
        let Some(facts) = self.runners.facts.get(&full_path) else {
            return unsupported("missing entry identity");
        };
        let applied = self.runners.applied.get(&runner.name).copied().unwrap_or(0);
        let form = if tag == 2 {
            CallForm::Transact
        } else {
            CallForm::Query
        };
        let digest = hirsel_run_schema::request_digest(
            form,
            &full_path,
            &facts.fingerprint,
            &facts.code,
            &[],
            &[],
            argument,
        );
        let request = OpValue::Aggregate(vec![
            v(tag, OpValue::Unit),
            entry_id(&full_path, &self.runners.facts).unwrap(),
            bytes(
                self.instance
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
                    .into_bytes(),
            ),
            OpValue::Aggregate(vec![
                bytes(hirsel_run_schema::hash_parts(
                    "hirsel.stream.v1",
                    [runner.name.as_bytes(), &self.instance],
                )),
                word(1),
                word(if tag == 2 { applied + 1 } else { 0 }),
                bytes(digest),
                word(applied),
            ]),
            runner
                .level
                .as_ref()
                .map_or_else(|| v(0, OpValue::Unit), |l| v(1, bytes(l.as_bytes()))),
            bytes(argument),
        ]);
        let key = runner.node.as_ref().unwrap_or(&runner.name).clone();
        let mut prefix = self.runners.places.get(&key).cloned().unwrap_or_default();
        prefix.requests.push(request);
        let world = HeapWorld {
            instance: hirsel_run_schema::hash_parts("hirsel.place.v1", [key.as_bytes()])[..16]
                .try_into()
                .unwrap(),
            authorities: grants.authorities,
            deny_output: true,
            mounts: BTreeMap::new(),
            ..Default::default()
        };
        let kind = hirsel_native::link::LaunchKind::Main(vec![]);
        let mut head_io = match Io::for_launch(&kind, 0, 0, &world) {
            Ok(io) => io,
            Err(e) => return unsupported(e),
        };
        head_io.positions = self.positions.clone();
        head_io.runners.facts = self.runners.facts.clone();
        head_io.head = Some(Head {
            requests: prefix.requests.clone(),
            ends: vec![None; prefix.requests.len()],
            ..Default::default()
        });
        let head = DefPath::from_full(runner.head.as_ref().unwrap()).unwrap();
        let Some(configuration) = self.runners.config.as_ref() else {
            return unsupported("no checked head configuration");
        };
        let mut config = (**configuration).clone();
        config.inject = None;
        // Every admitted root request must audit the entire nested head campaign.
        config.campaign_partition = None;
        config.fire = None;
        config.context_handles = vec![(hirsel_rt::launch::IO_HANDLE, "Io".into())];
        let args = [Arg::Handle(hirsel_rt::launch::IO_HANDLE), Arg::Unit];
        let initial = head_io.clone();
        let clean = hirsel_heapcheck::run_ready(&program, &head, &args, &mut head_io, &config);
        if let Err(e) = clean.result {
            return unsupported(format!("head: {e:?}"));
        }
        let state = head_io.head.as_ref().unwrap();
        if !state.exhausted || state.ends.iter().any(Option::is_none) {
            return unsupported("head ended or failed before reaching the next inbox wait");
        }
        let ends: Vec<_> = state.ends.iter().map(|e| e.clone().unwrap()).collect();
        if !ends.starts_with(&prefix.ends) {
            return unsupported("head prefix changed a previous completion");
        }
        // Every prefix's synthetic inbox failure and every ordinary injected
        // failure in that prefix must discharge the head's separate ledger.
        let campaign = hirsel_heapcheck::audit_campaign_ready(
            &program,
            &head,
            &args,
            &mut || Box::new(initial.clone()),
            &config,
        );
        if let Some((at, e)) = campaign.first_violation() {
            return unsupported(format!("head {at:?}: {e:?}"));
        }
        let end = ends.last().unwrap().clone();
        prefix.ends = ends;
        self.runners.places.insert(key, prefix);
        if tag == 2 && matches!(&end, OpValue::Variant { tag: 0 | 1 | 4, .. }) {
            self.runners
                .applied
                .insert(runner.name.clone(), applied + 1);
        }
        OpOutcome::Value(end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_contract::OperationId;
    use hirsel_heapcheck::Operations;
    use hirsel_lir::ir::{Symbol, SymbolKind};

    #[test]
    fn canceled_send_checks_admission_without_executing_the_head() {
        let mut io = Io::new();
        io.runners.entries.push(
            serde_json::from_value(serde_json::json!({
                "name": "main", "provider": "sim@1", "models": "db@1", "head": "app@1/head"
            }))
            .unwrap(),
        );
        let handle = io.grant(IoGrant::Runner { index: 0 });
        let mut args = vec![
            OpValue::Handle(handle),
            v(2, OpValue::Unit),
            OpValue::Function(Symbol {
                kind: SymbolKind::Function,
                definition: DefPath::from_full("app@1/entry").unwrap(),
                instance: None,
                charged: false,
            }),
            OpValue::Bytes(b"request".to_vec()),
        ];
        let op = OperationId {
            definition: DefPath::core("runner.send"),
            declaration_digest: Default::default(),
        };
        let Some(OpOutcome::Value(answer)) = io.canceled(&op, &args, 1, &mut vec![]) else {
            panic!("canceled send must complete");
        };
        assert_eq!(answer, v(5, v(1, OpValue::Unit)));
        // Unserved forms are denied before checking cancellation, as in places.rs.
        args[1] = v(0, OpValue::Unit);
        let Some(OpOutcome::Value(answer)) = io.canceled(&op, &args, 1, &mut vec![]) else {
            panic!("unserved send must be denied");
        };
        assert_eq!(answer, v(3, v(0, OpValue::Unit)));
        assert!(io.runners.places.is_empty());
        assert!(io.runners.applied.is_empty());
        assert!(io.head.is_none());
    }
}
