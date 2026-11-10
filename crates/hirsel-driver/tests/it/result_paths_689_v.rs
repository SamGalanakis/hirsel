//! V mutation controls for result-part relations (MODE-1): each forges one
//! accepted program's TIR into the wrong program the front end must never
//! emit, and V must reject it on its own.
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_tir::{CleanupAction, Constructor, Definition, InstructionKind, Rvalue, Terminator};

fn prepared(source: &str) -> (QueryEngine, NativeInput) {
    let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
    let file = engine
        .sources
        .add("result-paths-v.hir", source.into())
        .unwrap();
    let output = source_check_request(&mut engine, file, &default_context()).unwrap();
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let input = engine.export_native_input().unwrap();
    engine.import_native_input(input.clone()).unwrap();
    (engine, input)
}

fn named(input: &NativeInput, name: &str) -> std::sync::Arc<Definition> {
    input
        .definitions
        .iter()
        .find(|(path, _)| path.0.name == name)
        .map(|(_, d)| d.clone())
        .unwrap()
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

/// Retarget the one `Ok(..)` construction of `definition` to `Error(..)`.
fn ok_to_error(definition: &Definition) -> Definition {
    let mut forged = definition.clone();
    let mut changed = 0;
    for body in &mut forged.bodies {
        for value in &mut body.rvalues {
            if let Rvalue::Construct {
                constructor:
                    Constructor::Variant {
                        definition,
                        variant,
                    },
                ..
            } = value
                && definition.0.name == "Result"
                && *variant == 0
            {
                *variant = 1;
                changed += 1;
            }
        }
    }
    assert_eq!(changed, 1);
    forged
}

/// A borrowed parameter returned as the global Error payload: Bytes being
/// inert does not make a borrowed view of the caller's storage owned. A
/// generic part's relation is flow's too, whatever `T` is.
#[test]
fn result_paths_689_borrowed_error_payload_is_refused() {
    with_compiler_stack(|| {
        let mut accepted = vec![];
        for source in [
            "fn good(xs: Bytes) -> Result[Bytes in xs, Bytes] { Ok(copy xs) }\npub fn main(){}",
            "fn good[T: copyable](xs: T) -> Result[T in xs, T] { Ok(copy xs) }\npub fn main(){}",
        ] {
            let (engine, input) = prepared(source);
            let definition = named(&input, "good");
            let mut forged = ok_to_error(&definition);
            if violations(&engine, &input, &definition, &mut forged).is_empty() {
                accepted.push(source);
            }
        }
        assert!(
            accepted.is_empty(),
            "V accepted a borrowed Error payload under a global Error part: {accepted:?}"
        );
    });
}

/// An arena-placed value returned as the global Error payload outlives its
/// arena; V's typing refuses the field before flow sees it.
#[test]
fn result_paths_689_arena_error_payload_is_refused() {
    with_compiler_stack(|| {
        let (engine, input) = prepared(
            "fn mk(a: Arena) -> Bytes in a { b\"x\" }\n\
             fn good(a: Arena) -> Result[Bytes in a, Bytes] { Ok(mk(a)) }\npub fn main(){}",
        );
        let definition = named(&input, "good");
        let mut forged = ok_to_error(&definition);
        let found = violations(&engine, &input, &definition, &mut forged);
        assert!(
            !found.is_empty(),
            "V accepted an arena-placed Error payload under a global Error part"
        );
    });
}

/// The Ok payload of a call keeps its loan on the argument through `?`:
/// forged TIR that ends that loan right after the call, as if the result held
/// nothing, leaves the later use of `v` reading an expired view.
#[test]
fn result_paths_689_ok_payload_dependency_cannot_be_dropped() {
    with_compiler_stack(|| {
        let (engine, input) = prepared(
            "type Failed = Failed()\n\
             type Holder { data: Bytes }\n\
             fn get(h: Holder, k: u64) -> Result[Option[Bytes] in h, Failed] {\n\
               if k > 9 { return Error(Failed()) }\n\
               Ok(Some(copy h.data))\n\
             }\n\
             fn put(mut h: Holder, take b: Bytes) -> () { h.data = b }\n\
             fn size(o: Option[Bytes]) -> u64 { match o { Some(b) => bytes.len(b), None() => 0 } }\n\
             fn user(mut h: Holder) -> Result[u64, Failed] {\n\
               let v = get(h, 1)?\n\
               let n = size(v)\n\
               put(mut h, b\"y\")\n\
               Ok(n)\n\
             }\npub fn main(){}",
        );
        let definition = named(&input, "user");
        let mut forged = (*definition).clone();
        let body = &mut forged.bodies[0];
        let (place, next) = body
            .blocks
            .iter()
            .find_map(|block| match &block.terminator {
                Terminator::Call {
                    callee: hirsel_tir::Callee::Definition { path, .. },
                    arguments,
                    returns: Some(returns),
                    ..
                } if path.0.name == "get" => Some((arguments[0].value.place, returns.next)),
                _ => None,
            })
            .unwrap();
        let loan = body
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction.kind {
                InstructionKind::BeginLoan {
                    id, place: begun, ..
                } if begun == place || body.places[place.0 as usize].loan == Some(id) => Some(id),
                _ => None,
            })
            .unwrap();
        // Every later end of the loan becomes conditional, as for a loan the
        // call did not retain; the strict end moves to just after the call.
        let mut end = None;
        for block in &mut body.blocks {
            for instruction in &mut block.instructions {
                if let InstructionKind::EndLoan {
                    loan: id,
                    if_active,
                } = &mut instruction.kind
                    && *id == loan
                {
                    *if_active = true;
                    end.get_or_insert_with(|| instruction.clone());
                }
            }
        }
        for node in &mut body.cleanup_nodes {
            if let CleanupAction::EndLoan {
                loan: id,
                if_active,
            } = &mut node.action
                && *id == loan
            {
                *if_active = true;
            }
        }
        let mut end = end.expect("the argument loan of `get` ends somewhere");
        end.kind = InstructionKind::EndLoan {
            loan,
            if_active: false,
        };
        body.blocks[next.0 as usize].instructions.insert(0, end);
        let found = violations(&engine, &input, &definition, &mut forged);
        assert!(
            !found.is_empty(),
            "V accepted the Ok view of `h` used after its loan ended"
        );
    });
}

/// Forge every place of `definition` that projects the payload of Result
/// variant `variant` with a non-global relation to its global row; returns
/// the forged places.
fn globalize_payloads(
    engine: &QueryEngine,
    definition: &Definition,
    variant: u32,
) -> (Definition, Vec<hirsel_tir::PlaceId>) {
    let types = engine.types();
    let mut forged = definition.clone();
    let mut changed = vec![];
    for body in &mut forged.bodies {
        for (index, place) in body.places.iter_mut().enumerate() {
            if place.projection.last()
                != Some(&hirsel_tir::Projection::VariantField { variant, field: 0 })
            {
                continue;
            }
            let mut row = types.get(place.ty).unwrap().clone();
            if row.modes.relation.is_global() {
                continue;
            }
            row.modes.relation = hirsel_types::Relation::global();
            place.ty = types.intern(row);
            changed.push(hirsel_tir::PlaceId(index as u32));
        }
    }
    assert!(!changed.is_empty(), "no related payload place to forge");
    (forged, changed)
}

fn refuses_projection(
    found: &[hirsel_validate::Violation],
    places: &[hirsel_tir::PlaceId],
) -> bool {
    found.iter().any(|v| {
        matches!(v.kind, hirsel_validate::ViolationKind::InvalidProjection(p) if places.contains(&p))
    })
}

const HOLDER: &str = "type Failed = Failed(Bytes)\n\
     type Holder { data: Bytes }\n\
     fn get(h: Holder, k: u64) -> Result[Option[Bytes] in h, Failed] {\n\
       if k > 9 { return Error(Failed(b\"no\")) }\n\
       Ok(Some(copy h.data))\n\
     }\n\
     fn whole(h: Holder) -> Result[Option[Bytes], Failed] in h { Ok(Some(copy h.data)) }\n\
     fn size(o: Option[Bytes]) -> u64 { match o { Some(b) => bytes.len(b), None() => 0 } }\n\
     fn esize(e: Failed) -> u64 { match e { Failed(b) => bytes.len(b) } }\n";

/// MODE-1: only a result's unselected parts are global in storage that
/// receives it; its selected Ok part forged global must still be refused.
#[test]
fn result_paths_689_selected_part_forged_global_is_refused() {
    with_compiler_stack(|| {
        let source = format!(
            "{HOLDER}fn user(h: Holder) -> Result[u64, Failed] {{ let v = get(h, 1)?\n Ok(size(v)) }}\npub fn main(){{}}"
        );
        let (engine, input) = prepared(&source);
        let definition = named(&input, "user");
        let (mut forged, places) = globalize_payloads(&engine, &definition, 0);
        let found = violations(&engine, &input, &definition, &mut forged);
        assert!(
            refuses_projection(&found, &places),
            "V accepted the selected Ok part of `get` typed global: {found:?}"
        );
    });
}

/// MODE-1: storage that may also hold a result with no part selection
/// (after a join, a reassignment or a whole move from such storage) keeps
/// its Error part as related as its root; forged global, it is refused.
#[test]
fn result_paths_689_mixed_storage_keeps_error_relation() {
    with_compiler_stack(|| {
        let mut accepted = vec![];
        for (name, params, body) in [
            ("joined", "", "let r = if c { get(h, 1) } else { whole(h) }"),
            (
                "reassigned",
                "",
                "let mut r = get(h, 1)\n if c { r = whole(h) }",
            ),
            (
                "moved",
                "",
                "let w = whole(h)\n let mut r = get(h, 1)\n if c { r = w }",
            ),
            // Review 704 (c): writes the inventory must also see: an entry
            // value, a `mut` call's handback.
            (
                "entry",
                ", take w: Result[Option[Bytes], Failed] in h",
                "let r = if c { get(h, 1) } else { take w }",
            ),
            (
                "handback",
                "",
                "let mut r = get(h, 1)\n if c { refill(h, mut r) }",
            ),
        ] {
            let source = format!(
                "{HOLDER}fn refill(h: Holder, mut r: Result[Option[Bytes], Failed] in h) {{ }}\n\
                 fn {name}(h: Holder, c: bool{params}) -> u64 {{ {body}\n match r {{ Ok(v) => size(v), Error(e) => esize(e) }} }}\npub fn main(){{}}"
            );
            let (engine, input) = prepared(&source);
            let definition = named(&input, name);
            let (mut forged, places) = globalize_payloads(&engine, &definition, 1);
            let found = violations(&engine, &input, &definition, &mut forged);
            if !refuses_projection(&found, &places) {
                accepted.push(name);
            }
        }
        assert!(
            accepted.is_empty(),
            "V accepted an Error part related to `h` typed global: {accepted:?}"
        );
    });
}

fn path_of(input: &NativeInput, name: &str) -> hirsel_syntax::DefPath {
    input
        .definitions
        .keys()
        .find(|path| path.0.name == name)
        .cloned()
        .unwrap()
}

/// Forge every call of, and reference to, `from` in `definition` to name `to`
/// instead: a valid caller of a same-shaped function becomes a caller of
/// `to` that keeps every destination, type and cleanup the front end gave
/// the original.
fn retarget(definition: &Definition, from: &str, to: &hirsel_syntax::DefPath) -> Definition {
    let mut forged = definition.clone();
    let mut changed = 0;
    for body in &mut forged.bodies {
        for block in &mut body.blocks {
            if let Terminator::Call {
                callee: hirsel_tir::Callee::Definition { path, .. },
                ..
            } = &mut block.terminator
                && path.0.name == from
            {
                *path = to.clone();
                changed += 1;
            }
        }
        for value in &mut body.rvalues {
            if let Rvalue::FunctionReference { definition, .. } = value
                && definition.0.name == from
            {
                *definition = to.clone();
                changed += 1;
            }
        }
    }
    assert!(changed > 0, "nothing names `{from}`");
    forged
}

/// Review 704 (a): an arena-placed selected part does not become global at
/// a call. `escape` calls `plain` with a global destination; forged to call
/// `good`, whose Ok payload lives in `a`, it would return arena data as heap.
#[test]
fn result_paths_689_arena_part_kept_at_call() {
    with_compiler_stack(|| {
        let (engine, input) = prepared(
            "fn mk(a: Arena) -> Bytes in a { b\"x\" }\n\
             fn good(a: Arena) -> Result[Bytes in a, Bytes] { Ok(mk(a)) }\n\
             fn plain(a: Arena) -> Result[Bytes, Bytes] { Ok(b\"x\") }\n\
             fn escape(a: Arena) -> Result[Bytes, Bytes] { plain(a) }\n\
             fn bound(a: Arena) -> Result[Bytes, Bytes] { let r = plain(a)\n r }\n\
             pub fn main(){}",
        );
        let good = path_of(&input, "good");
        let mut accepted = vec![];
        for name in ["escape", "bound"] {
            let definition = named(&input, name);
            let mut forged = retarget(&definition, "plain", &good);
            if violations(&engine, &input, &definition, &mut forged).is_empty() {
                accepted.push(name);
            }
        }
        assert!(
            accepted.is_empty(),
            "V accepted arena data of `good` received as global: {accepted:?}"
        );
    });
}

/// Review 704 (d): an entry's declared function type is its contract. A
/// reference typed `Entry[fn(..) -> Result[Bytes, Bytes]]` to `plain`,
/// forged to name `good`, whose Ok part borrows `xs`, claims a global result
/// the definition does not give; so does a whole-result `in xs` one.
#[test]
fn result_paths_689_entry_contract_is_checked() {
    with_compiler_stack(|| {
        let (engine, input) = prepared(
            "fn good(xs: Bytes, ys: Bytes) -> Result[Bytes in xs, Bytes] { Ok(copy xs) }\n\
             fn plain(xs: Bytes, ys: Bytes) -> Result[Bytes, Bytes] { Ok(b\"x\") }\n\
             fn whole(xs: Bytes, ys: Bytes) -> Bytes in xs { copy xs }\n\
             fn flatten(xs: Bytes, ys: Bytes) -> Bytes { b\"x\" }\n\
             fn parts() -> u64 { let g: Entry[fn(xs: Bytes, ys: Bytes) -> Result[Bytes, Bytes]] = plain\n 0 }\n\
             fn wholes() -> u64 { let g: Entry[fn(xs: Bytes, ys: Bytes) -> Bytes] = flatten\n 0 }\n\
             fn value() -> u64 { let g: fn(xs: Bytes, ys: Bytes) -> Result[Bytes, Bytes] = plain\n 0 }\n\
             pub fn main(){}",
        );
        let mut accepted = vec![];
        for (name, from, to) in [
            ("parts", "plain", "good"),
            ("wholes", "flatten", "whole"),
            ("value", "plain", "good"),
        ] {
            let definition = named(&input, name);
            let mut forged = retarget(&definition, from, &path_of(&input, to));
            if violations(&engine, &input, &definition, &mut forged).is_empty() {
                accepted.push(name);
            }
        }
        assert!(
            accepted.is_empty(),
            "V accepted a function reference whose type misstates its result: {accepted:?}"
        );
    });
}
