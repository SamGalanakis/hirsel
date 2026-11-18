//! Release composition consumes the existing reachable/materialized products.
//! The release unit is the whole program: every reached body, the program's and
//! each library's alike, is an internal definition of one LLVM module that the
//! whole-program pipeline optimises. Each out-of-line definition is then a
//! part keyed by its own bitcode and the code generator's settings; one code
//! generation covers every part not already held, and its object is cut per
//! part. An unchanged definition is generated once, whichever program and
//! build reached it.
use super::*;
use hirsel_core_ir::{Callee, FuncId, HostOp, Inst, RtOp};
use hirsel_core_layout::flow::Op as FlowOp;
use serde::{Deserialize, Serialize};

/// The release unit's key: its inputs and the role facts each unit consumed.
fn unit_key(
    inputs: &impl serde::Serialize,
    consumed: &[&[hirsel_core_ir::roles::BodyFacts]],
) -> io::Result<ContentHash> {
    crate::native::key(
        "hirsel.core-release-unit.v6",
        &(hirsel_core_ir::roles::FACTS_SCHEMA, consumed, inputs),
    )
}

/// A part's key: the compiler, libLLVM, target and debug lines
/// (`generator`), the target machine's settings as libLLVM states them
/// (`machine`: CPU, features, relocation and code model, codegen options), and
/// the part's bitcode, which is all a part's object is a function of.
fn part_key(
    generator: &impl serde::Serialize,
    machine: &str,
    bitcode: &[u8],
) -> io::Result<ContentHash> {
    crate::native::key(
        "hirsel.core-release-part.v2",
        &(generator, machine, ContentHash::of_bytes(bitcode)),
    )
}

/// The release unit's record: its parts' keys in link order. A program whose
/// module cannot be cut is one part.
#[derive(Serialize, Deserialize)]
struct Parts {
    schema: u32,
    parts: Vec<ContentHash>,
}
const PARTS_SCHEMA: u32 = 2;
/// Whether a stored payload is a release unit's record.
#[cfg(test)]
pub(crate) fn is_unit_record(payload: &[u8]) -> bool {
    decode::<Parts>(payload).is_some_and(|r| r.schema == PARTS_SCHEMA)
}

/// In a launched executable, every code pointer comes from a function or
/// descriptor reference in the image. The launcher supplies only capabilities,
/// spawn copies a parent's bound, and cleanup and the task entry adapters call
/// back into the same image. Drop glue calls finalizers by type. Clone and
/// codec code is called or named explicitly in the materialized bodies.
fn cannot_install_step_bound(module: &Module) -> bool {
    let defined = |f: FuncId| module.functions[f.index()].body.is_some();
    module
        .types
        .iter()
        .filter_map(|shape| shape.finalizer)
        .all(|f| defined(FuncId(f.0)))
        && module
            .functions
            .iter()
            .filter_map(|f| f.body.as_ref())
            .all(|body| {
                body.insts.iter().all(|inst| match *inst {
                    Inst::FunctionRef { function, .. } | Inst::DescriptorRef { function, .. } => {
                        defined(function)
                    }
                    Inst::Call { call } => match body.calls[call.index()].callee {
                        Callee::Direct(function) => defined(function),
                        Callee::Indirect(_) => true,
                        Callee::Runtime(op) => step_elision_compatible(op),
                    },
                    Inst::Const { .. }
                    | Inst::LayoutConst { .. }
                    | Inst::ReadStatic { .. }
                    | Inst::Unary { .. }
                    | Inst::Binary { .. }
                    | Inst::Compare { .. }
                    | Inst::Convert { .. }
                    | Inst::IntArith { .. }
                    | Inst::MulAdd { .. }
                    | Inst::ScalarUnary { .. }
                    | Inst::Address { .. }
                    | Inst::Read { .. }
                    | Inst::Store { .. }
                    | Inst::StorageLive { .. }
                    | Inst::StorageDead { .. }
                    | Inst::UpdateMask { .. }
                    | Inst::Pointer { .. }
                    | Inst::Alloc { .. }
                    | Inst::Free { .. }
                    | Inst::FrameRefund { .. }
                    | Inst::Copy { .. }
                    | Inst::Grow { .. }
                    | Inst::Case { .. }
                    | Inst::SetCase { .. }
                    | Inst::RegionOpen { .. }
                    | Inst::RegionClose { .. }
                    | Inst::Trim { .. }
                    | Inst::LimitEnter { .. }
                    | Inst::LimitExit
                    | Inst::Drop { .. }
                    | Inst::Share { .. }
                    | Inst::Release { .. }
                    | Inst::TryUnwrap { .. }
                    | Inst::Mark { .. }
                    | Inst::Unmark { .. }
                    | Inst::Step { .. }
                    | Inst::PromoteFrame { .. } => true,
                })
            })
}

fn step_elision_compatible(op: RtOp) -> bool {
    match op {
        RtOp::StepEnter | RtOp::StepLeave | RtOp::StepCatch | RtOp::Flow(FlowOp::Stage) => false,
        // The native receive-in-loop entry can retain the sequencer grant.
        // A Step refill is one of the release boundaries for that grant.
        RtOp::Receive { .. } => false,
        RtOp::Spawn { .. }
        | RtOp::Join { .. }
        | RtOp::Cancel
        | RtOp::CheckCancel
        | RtOp::ChannelNew { .. }
        | RtOp::ChannelSender
        | RtOp::Send { .. }
        | RtOp::TrySend { .. }
        | RtOp::TryReceive { .. }
        | RtOp::LatestNew { .. }
        | RtOp::LatestHandle
        | RtOp::LatestPublish
        | RtOp::LatestRead
        | RtOp::DestId
        | RtOp::CompareBytes
        | RtOp::HashBytes
        | RtOp::CauseNext
        | RtOp::CauseValue
        | RtOp::ArgsLen
        | RtOp::ArgsGet
        | RtOp::TapeOpen
        | RtOp::TapeEnd
        | RtOp::TapeGrant
        | RtOp::TapeAcross
        | RtOp::IsolateEnter
        | RtOp::IsolateLeave
        | RtOp::IsolateCatch
        | RtOp::LaunchWord
        | RtOp::LaunchTarget { .. }
        | RtOp::DescribeEmit
        | RtOp::Flow(
            FlowOp::New
            | FlowOp::Sink
            | FlowOp::Port
            | FlowOp::Push
            | FlowOp::Advance
            | FlowOp::End
            | FlowOp::Next
            | FlowOp::Finish,
        )
        | RtOp::Host(
            HostOp::Open
            | HostOp::Read
            | HostOp::Write
            | HostOp::Close
            | HostOp::TryLock
            | HostOp::Unlock
            | HostOp::Mkdir
            | HostOp::Mkdtemp
            | HostOp::Remove
            | HostOp::RemoveTree
            | HostOp::WallMs
            | HostOp::MonoNs
            | HostOp::Stdout
            | HostOp::Stderr
            | HostOp::Rename
            | HostOp::SleepUntil
            | HostOp::Sync
            | HostOp::SyncDir
            | HostOp::Random
            | HostOp::Connect
            | HostOp::Listen
            | HostOp::Accept
            | HostOp::RecvBounded
            | HostOp::SendPair
            | HostOp::NetNarrow
            | HostOp::NetSplit,
        ) => true,
    }
}

impl CheckProduct {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_release(
        &self,
        entry: &DefPath,
        reached: &[Identity],
        materialized: &crate::instances::InstanceSet,
        options: &NativeOptions,
        runtime: ContentHash,
        store: &Store,
        closed_executable: bool,
    ) -> Result<Emitted, NativeStop> {
        let compiler = compiler_identity()?;
        // Release is LLVM's alone: every object here is libLLVM's output.
        let llvm = crate::tool_inputs::llvm_library()?;
        let mut modules: Vec<_> = reached
            .iter()
            .map(|instance| materialized.get(instance).ir().map(Cow::Borrowed))
            .collect::<io::Result<_>>()?;
        let body_identities = crate::callable_variants::prepare(
            &mut modules,
            reached,
            materialized,
            entry,
            options.instance_limit,
        )?;
        // Exact Step planning needs the same helper bodies LLVM will optimize.
        // Resolve the existing unit tables before lowering, as the evaluator does.
        let entry_identity = reached
            .iter()
            .find(|i| &i.definition == entry)
            .expect("reached entry");
        let roots = modules
            .iter()
            .enumerate()
            .map(|(unit, m)| {
                if let Some(identity) = reached.get(unit) {
                    return (identity.clone(), materialized.get(identity).function);
                }
                let (i, f) = m
                    .functions
                    .iter()
                    .enumerate()
                    .find(|(_, f)| f.body.is_some())
                    .expect("reached definition");
                (m.identity(f.instance), hirsel_core_ir::FuncId(i as u32))
            })
            .collect::<Vec<_>>();
        let units = modules
            .iter()
            .zip(&roots)
            .map(
                |(module, (identity, function))| hirsel_core_materialize::Unit {
                    identity,
                    module,
                    function: *function,
                },
            )
            .collect::<Vec<_>>();
        let (joined, _) =
            hirsel_core_materialize::join(&units, entry_identity).map_err(|message| {
                NativeStop::Emit {
                    definition: entry.clone(),
                    message,
                }
            })?;
        let elide_steps = closed_executable && cannot_install_step_bound(&joined);
        // The closed-unit walk above proves calls, function references and
        // finalizers stay in the image. The separate result cleanup is
        // considered when selecting the entry plan.
        let launch_digest_unused = elide_steps
            && !joined
                .functions
                .iter()
                .filter_map(|f| f.body.as_ref())
                .any(|body| {
                    body.calls
                        .iter()
                        .any(|call| call.callee == Callee::Runtime(RtOp::LaunchWord))
                });
        modules = vec![Cow::Owned(joined)];
        let refs = modules.iter().map(|m| &**m).collect::<Vec<_>>();
        // The checked body keys identify the source-neutral published modules.
        // All shape-owner modules used by generic materialization are in this
        // ordered closure. Entry and compiler identify the deterministic callable
        // preparation above, including appended private units.
        let facts_key = crate::native_facts::key(compiler, entry, &body_identities, reached, &[])?;
        let crate::native_facts::ProgramFacts { facts, consumed } =
            crate::native_facts::get(store, facts_key, &refs, &[])?;
        let instance = reached
            .iter()
            .find(|i| &i.definition == entry)
            .expect("reached entry");
        let target = hirsel_core_layout::Target::from_triple(&options.target)
            .expect("supported native target");
        let generator = (compiler, llvm, &options.target, options.debug_lines);
        let unit = unit_key(
            &(
                &generator,
                options.logical_charge_report.is_some(),
                !hirsel_llvm_lazy::core::facts_enabled(),
                runtime,
                instance,
                closed_executable,
                elide_steps,
                &refs,
            ),
            &consumed.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )?;
        // Every part, whichever program reached it, is in the shared parts cache.
        let shared = Store::new(&options.parts)?;
        let emit = |message: String| NativeStop::Emit {
            definition: entry.clone(),
            message,
        };
        // An unchanged rebuild reads the unit's record and every part it names.
        let mut held = Vec::new();
        if let Some(record) = store.get(unit)?.and_then(|b| decode::<Parts>(&b))
            && record.schema == PARTS_SCHEMA
        {
            for &key in &record.parts {
                match crate::native_object::load(&shared, key)? {
                    Some((bare, object, file)) => held.push((bare, object, file, false)),
                    None => break,
                }
            }
            if held.len() != record.parts.len() {
                held.clear();
            }
        }
        if held.is_empty() {
            let units = modules
                .iter()
                .zip(&facts)
                .map(|(module, facts)| hirsel_llvm_lazy::Unit::Core {
                    module,
                    facts,
                    observe_charge: options.logical_charge_report.is_some(),
                    debug_lines: options.debug_lines,
                })
                .collect::<Vec<_>>();
            let mut parts = Vec::new();
            let mut failure = None;
            // One code generator thread per CPU this process may run on.
            let jobs = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
            let program = hirsel_llvm_lazy::emit_program_parts(
                &units,
                &[instance.symbol()],
                target,
                jobs,
                elide_steps,
                &mut |_, machine, bitcode| {
                    let loaded = part_key(&generator, machine, bitcode).and_then(|key| {
                        parts.push(key);
                        crate::native_object::load(&shared, key)
                    });
                    match loaded {
                        Ok(Some((bare, object, file))) => {
                            held.push((bare, object, file, false));
                            true
                        }
                        Ok(None) => false,
                        Err(error) => {
                            failure.get_or_insert(error);
                            true
                        }
                    }
                },
            )
            .map_err(emit)?;
            if let Some(error) = failure {
                return Err(error.into());
            }
            // Each missed part's object, or the one object of a module
            // generated whole, is cut to the part's finished object; those
            // parts join the hits in part order.
            let missed = program
                .parts
                .iter()
                .zip(&parts)
                .filter(|(part, _)| !part.reused)
                .map(|(part, &key)| (part.name.as_str(), key))
                .collect::<Vec<_>>();
            let cut = |object: &[u8], parts: &[(&str, ContentHash)]| {
                hirsel_llvm_lazy::core::split_native_object(object, parts, options.debug_lines)
                    .map_err(emit)
            };
            let mut made = match program.objects.as_slice() {
                [object] => cut(object, &missed)?,
                objects => {
                    let mut made = Vec::with_capacity(missed.len());
                    for (object, part) in objects.iter().zip(&missed) {
                        made.extend(cut(object, std::slice::from_ref(part))?);
                    }
                    made
                }
            }
            .into_iter();
            let mut hits = std::mem::take(&mut held).into_iter();
            for (part, &key) in program.parts.iter().zip(&parts) {
                held.push(match part.reused {
                    true => hits.next().expect("a reused part was loaded"),
                    false => {
                        let object = made.next().expect("a missed part was generated");
                        let (bare, file) = crate::native_object::publish(&shared, key, &object)?;
                        (bare, object, file, true)
                    }
                });
            }
            store.put_keyed(
                unit,
                &encode(&Parts {
                    schema: PARTS_SCHEMA,
                    parts,
                })?,
            )?;
        }
        let mut private_variants = modules
            .iter()
            .map(|m| {
                m.code_variants
                    .keys()
                    .filter(|f| m.functions[f.index()].body.is_some())
                    .count()
            })
            .sum();
        let objects = held
            .into_iter()
            .map(|(bare, object, file, compiled)| ObjectProduct {
                private_variants: std::mem::take(&mut private_variants),
                instance: instance.clone(),
                path: crate::native_object::path(&shared, bare.address),
                identity: bare.address,
                check: bare.check,
                held: Some(file),
                debug: object.debug,
                debug_record: None,
                compiled,
            })
            .collect();
        Ok(Emitted {
            objects,
            table: None,
            step_charging_elided: elide_steps,
            launch_digest_unused,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consumed_role_facts_invalidate_the_release_unit_key() {
        use hirsel_core_ir::roles::{BodyFacts, ParamFacts};
        let original = vec![BodyFacts {
            code: ContentHash::of_bytes(b"callee code identity"),
            params: vec![ParamFacts {
                sealed: true,
                place_blind: true,
            }],
        }];
        let before = unit_key(&"unchanged body and signature", &[&original]).unwrap();
        assert_eq!(
            before,
            unit_key(&"unchanged body and signature", &[&original.clone()]).unwrap()
        );
        for field in 0..2 {
            let mut changed = original.clone();
            if field == 0 {
                changed[0].params[0].sealed = false;
            } else {
                changed[0].params[0].place_blind = false;
            }
            assert_ne!(
                before,
                unit_key(&"unchanged body and signature", &[&changed]).unwrap()
            );
        }
    }
}
