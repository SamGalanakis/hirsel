//! Release composition consumes the existing reachable/materialized products.
//! The release unit is the whole program: every reached body, the program's and
//! each library's alike, is an internal definition of one LLVM module that the
//! whole-program pipeline optimises. After cleanup, each definition's release
//! tail runs alone and is held in the shared parts store under the key of its
//! part at that cut (`module_key`, a walk of the part's IR in memory, with the
//! linkage facts the part does not keep), so an edit reruns only the tails whose
//! input changed. Each out-of-line definition is then a part keyed by its own
//! IR and the code generator's settings; one code
//! generation covers every part not already held, and its object is cut per
//! part. An unchanged definition is generated once, whichever program and
//! build reached it.
use super::*;
use hirsel_core_ir::{Callee, RtOp};
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
/// the part's key (`ir`: what its bitcode records), which is all a part's
/// object is a function of.
fn part_key(
    generator: &impl serde::Serialize,
    machine: &str,
    ir: &[u8; 32],
) -> io::Result<ContentHash> {
    crate::native::key(
        "hirsel.core-release-part.v3",
        &(generator, machine, ContentHash(*ir)),
    )
}

/// A release tail's key: the compiler, libLLVM, target and debug lines
/// (`generator`), the machine text and the linkage the definition's part
/// does not keep (`key`), and the key of the part at the cut (`ir`), which is
/// all the tail's output is a function of.
fn tail_key(
    generator: &impl serde::Serialize,
    key: &[u8],
    ir: &[u8; 32],
) -> io::Result<ContentHash> {
    crate::native::key(
        "hirsel.core-release-tail.v3",
        &(generator, ContentHash::of_bytes(key), ContentHash(*ir)),
    )
}

/// What the shared parts cache holds for a release program: each part's
/// object and each definition's release tail. A store error stops the
/// program after generation; until then nothing more is loaded or kept.
struct SharedParts<'a, G> {
    generator: &'a G,
    shared: &'a Store,
    parts: &'a mut Vec<ContentHash>,
    held: &'a mut Vec<Held>,
    failure: &'a mut Option<io::Error>,
}

#[cfg(test)]
pub(super) struct TailFixture {
    pub key: Vec<u8>,
    pub ir: [u8; 32],
    pub input: Vec<u8>,
    pub cached: Vec<u8>,
}

#[cfg(test)]
std::thread_local! {
    pub(super) static TAIL_FIXTURES: std::cell::RefCell<Option<Vec<TailFixture>>> = const { std::cell::RefCell::new(None) };
}

impl<G: serde::Serialize> hirsel_llvm_lazy::Held for SharedParts<'_, G> {
    fn part(&mut self, _: &str, machine: &str, ir: &[u8; 32]) -> bool {
        let loaded = part_key(self.generator, machine, ir).and_then(|key| {
            self.parts.push(key);
            crate::native_object::load(self.shared, key)
        });
        match loaded {
            Ok(Some((bare, object, file))) => {
                self.held.push((bare, object, file, false));
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.failure.get_or_insert(error);
                true
            }
        }
    }

    fn tail(&mut self, key: &[u8], ir: &[u8; 32]) -> Option<Vec<u8>> {
        if self.failure.is_some() {
            return None;
        }
        tail_key(self.generator, key, ir)
            .and_then(|key| self.shared.get(key))
            .unwrap_or_else(|error| {
                self.failure.get_or_insert(error);
                None
            })
    }

    fn keep_tail(&mut self, key: &[u8], ir: &[u8; 32], input: &[u8], tail: &[u8]) {
        if self.failure.is_some() {
            return;
        }
        #[cfg(test)]
        TAIL_FIXTURES.with(|fixtures| {
            if let Some(fixtures) = fixtures.borrow_mut().as_mut() {
                fixtures.push(TailFixture {
                    key: key.to_vec(),
                    ir: *ir,
                    input: input.to_vec(),
                    cached: tail.to_vec(),
                });
            }
        });
        if let Err(error) =
            tail_key(self.generator, key, ir).and_then(|key| self.shared.put_keyed(key, tail))
        {
            self.failure.get_or_insert(error);
        }
    }

    /// A test that collects tail fixtures replays each tail from its input.
    #[cfg(test)]
    fn witness(&self) -> bool {
        TAIL_FIXTURES.with(|fixtures| fixtures.borrow().is_some())
    }
}

/// The release unit's record: its parts' keys in link order. A program whose
/// module cannot be cut is one part.
#[derive(Serialize, Deserialize)]
struct Parts {
    schema: u32,
    parts: Vec<ContentHash>,
    sites: Vec<hirsel_core_ir::source::ReleaseSite>,
}
const PARTS_SCHEMA: u32 = 3;

type Held = (
    crate::store::Bare,
    hirsel_core_ir::source::NativeObject,
    std::fs::File,
    bool,
);

fn load_parts(
    store: &Store,
    shared: &Store,
    key: ContentHash,
) -> io::Result<Option<(Parts, Vec<Held>)>> {
    let Some(record) = store.get(key)?.and_then(|bytes| decode::<Parts>(&bytes)) else {
        return Ok(None);
    };
    if record.schema != PARTS_SCHEMA || record.parts.is_empty() {
        return Ok(None);
    }
    let mut held = Vec::with_capacity(record.parts.len());
    for &key in &record.parts {
        let Some((bare, object, file)) = crate::native_object::load(shared, key)? else {
            return Ok(None);
        };
        held.push((bare, object, file, false));
    }
    Ok(Some((record, held)))
}
/// Whether a stored payload is a release unit's record.
#[cfg(test)]
pub(crate) fn is_unit_record(payload: &[u8]) -> bool {
    decode::<Parts>(payload).is_some_and(|r| r.schema == PARTS_SCHEMA)
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
        let (mut joined, entry_function) = hirsel_core_materialize::join(&units, entry_identity)
            .map_err(|message| NativeStop::Emit {
                definition: entry.clone(),
                message,
            })?;
        // The unit holds every body the program can run, so a blob's `W` that every
        // root reaching its type writes alike is a constant; the unit key below
        // reads the module after this.
        hirsel_core_ir::widths::resolve(&mut joined);
        // In a launched executable every code pointer comes from a function or
        // descriptor reference in the image: the launcher supplies only
        // capabilities, and spawn, flow, cleanup and the task entry adapters
        // call back into the same image. Drop glue calls finalizers by type.
        let contained = closed_executable && hirsel_core_ir::step_contexts::self_contained(&joined);
        if contained {
            hirsel_core_ir::step_contexts::specialize(&mut joined, entry_function);
        }
        // The separate result cleanup is considered when selecting the entry plan.
        let launch_digest_unused = contained
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
        let facts_key =
            crate::native_facts::key(compiler, entry, &body_identities, reached, &[], contained)?;
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
        let mut held = Vec::new();
        let mut sites = Vec::new();
        let mut lowered = None;
        let mut optimized_key = None;
        if let Some((record, objects)) = load_parts(store, &shared, unit)? {
            sites = record.sites;
            held = objects;
        } else if closed_executable
            && !options.debug_lines
            && target == hirsel_core_layout::Target::X86_64Linux
            && std::env::var_os("HIRSEL_LLVM_PRINTED_ORACLE").is_none()
        {
            let mut release = hirsel_llvm_lazy::ReleaseUnit::lower(
                &modules[0],
                &facts[0],
                options.logical_charge_report.is_some(),
            )
            .map_err(emit)?;
            sites = std::mem::take(&mut release.sites);
            let key = crate::native::key(
                "hirsel.core-release-ir.v2",
                &(&generator, runtime, instance, release.ir_key()),
            )?;
            if let Some((mut record, objects)) = load_parts(store, &shared, key)? {
                record.sites.clone_from(&sites);
                store.put_keyed(unit, &encode(&record)?)?;
                held = objects;
            } else {
                optimized_key = Some(key);
                lowered = Some(release);
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
            let mut reuse = SharedParts {
                generator: &generator,
                shared: &shared,
                parts: &mut parts,
                held: &mut held,
                failure: &mut failure,
            };
            let exports = [instance.symbol()];
            let program = match lowered {
                Some(release) => release.emit(&exports, jobs, &mut reuse),
                None => {
                    hirsel_llvm_lazy::emit_program_parts(&units, &exports, target, jobs, &mut reuse)
                }
            }
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
            let mut record = Parts {
                schema: PARTS_SCHEMA,
                parts,
                sites: Vec::new(),
            };
            if let Some(key) = optimized_key {
                store.put_keyed(key, &encode(&record)?)?;
            }
            record.sites.clone_from(&sites);
            store.put_keyed(unit, &encode(&record)?)?;
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
            closed_image: contained,
            launch_digest_unused,
            release_sites: sites,
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
