//! V mutation controls for `Recorder` (MODE-6): each forges one accepted
//! program's TIR into the wrong program the front end must never emit, and V
//! must reject it on its own.
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_tir::{Callee, Definition, Terminator};
use hirsel_types::UseKind;

fn prepared(source: &str) -> (QueryEngine, NativeInput) {
    let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
    let file = engine.sources.add("recorder-v.hir", source.into()).unwrap();
    let output = source_check_request(&mut engine, file, &default_context()).unwrap();
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let input = engine.export_native_input().unwrap();
    engine.import_native_input(input.clone()).unwrap();
    (engine, input)
}

fn is_append(callee: &Callee) -> bool {
    matches!(callee, Callee::Definition { path, .. }
        if path.0.package.name_is("core") && path.0.modules == ["recorder"] && path.0.name == "append")
}

/// The violations V reports for `forged`; the unforged definition must have none.
fn violations(
    engine: &QueryEngine,
    input: &NativeInput,
    definition: &Definition,
    forged: &mut Definition,
) -> Vec<hirsel_validate::Violation> {
    let mut signatures = hirsel_validate::SignatureMap::new(input.types.clone());
    signatures.extend_signatures(input.signatures.values().map(|s| s.as_ref().clone()));
    hirsel_validate::assert_valid(definition, engine.types(), &signatures);
    forged.types = hirsel_tir::definition_types(engine.types(), forged);
    hirsel_validate::validate(forged, engine.types(), &signatures, &mut vec![])
        .unwrap()
        .violations
}

/// An append that consumes the recorder leaves `finish(take r)` reading a moved
/// recorder: the use after finish the borrow in `append` exists to rule out.
#[test]
fn recorder_v_686_append_that_consumes_the_recorder_is_refused() {
    with_compiler_stack(|| {
        let (engine, input) = prepared(
            "pub fn main(){let r:Recorder[u64]=recorder.new();recorder.append(r,1u64);let xs:List[u64]=recorder.finish(take r);}",
        );
        let definition = input.definitions[&entry_path(&default_context())].clone();
        let mut forged = (*definition).clone();
        let mut changed = 0;
        for body in &mut forged.bodies {
            for block in &mut body.blocks {
                if let Terminator::Call {
                    callee, arguments, ..
                } = &mut block.terminator
                    && is_append(callee)
                {
                    let r = arguments.iter_mut().find(|a| a.parameter.0 == 0).unwrap();
                    assert_eq!(r.value.usage, UseKind::Borrow);
                    r.value.usage = UseKind::Take;
                    changed += 1;
                }
            }
        }
        assert_eq!(changed, 1);
        let found = violations(&engine, &input, &definition, &mut forged);
        assert!(
            found.iter().any(|v| matches!(
                v.kind,
                hirsel_validate::ViolationKind::TypeMismatch {
                    entity: "argument use"
                }
            )),
            "V accepted an append that consumes the recorder: {found:?}"
        );
    });
}

/// Recording the caller's arena key itself instead of an owned copy retains a
/// local payload past the arena: V must refuse the event operand.
#[test]
fn recorder_v_686_arena_event_without_its_copy_is_refused() {
    with_compiler_stack(|| {
        let (engine, input) = prepared(
            "fn read(r:Recorder[Bytes],key:Bytes)->(){recorder.append(r,copy_to(heap,key))}\n\
             pub fn main(){let r:Recorder[Bytes]=recorder.new();arena.with(fn(a:Arena){let k:Bytes in a=copy_to(a,b\"absent\");read(r,k)});let xs:List[Bytes]=recorder.finish(take r);}",
        );
        let (path, definition) = input
            .definitions
            .iter()
            .find(|(path, _)| path.0.name == "read")
            .map(|(p, d)| (p.clone(), d.clone()))
            .unwrap();
        let mut forged = (*definition).clone();
        let body = &mut forged.bodies[0];
        let key = body.parameters[1];
        let key_place = body
            .places
            .iter()
            .position(|p| p.root == key && p.projection.is_empty())
            .expect("a place of the key parameter");
        let mut changed = 0;
        for block in &mut body.blocks {
            if let Terminator::Call {
                callee, arguments, ..
            } = &mut block.terminator
                && is_append(callee)
            {
                let event = arguments.iter_mut().find(|a| a.parameter.0 == 1).unwrap();
                event.value.place = hirsel_tir::PlaceId(key_place as u32);
                event.value.usage = UseKind::Take;
                changed += 1;
            }
        }
        assert_eq!(changed, 1, "{path:?}");
        let found = violations(&engine, &input, &definition, &mut forged);
        assert!(
            found
                .iter()
                .any(|v| matches!(v.kind, hirsel_validate::ViolationKind::BorrowedMove(_))),
            "V accepted a borrowed key recorded as the event: {found:?}"
        );
    });
}
