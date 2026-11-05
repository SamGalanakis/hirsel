//! A List lent `mut` to `list.pop`, which allocates nothing and so cannot
//! detach, is made writable by the caller: an arena List is never written in
//! place, even through a `unique` parameter.
use crate::rc_support as support;
use hirsel_lir::ir::{Callee, Inst, Terminator};
use hirsel_rc::{insert::Proofs, placement::Options};

const SOURCE: &str = "fn pop(mut xs: unique List[String]) -> Option[String] uses {} {
  list.pop(mut xs)
}

fn length(text: Option[String]) -> u64 uses {} {
  match text {
    Some(t) => string.byte_len(t),
    None => 0,
  }
}

fn inside(a: Arena) -> u64 uses {} {
  let mut xs: unique List[String] in a = [\"arena\", \"text\"]
  length(list.pop(mut xs)) + length(pop(mut xs))
}

pub fn main() -> Result[(), u64] uses {} {
  Error(arena.with(fn(a: Arena) -> u64 { inside(a) }))
}
";

/// Each storage primitive call in `name` follows a writability test.
/// An ordinary wrapper detaches at its own primitive call, checked separately.

#[test]
fn direct_and_wrapper_calls_preserve_arena_and_shared_owners() {
    use hirsel_driver::native;
    use hirsel_engine::{
        QueryEngine, RunConfiguration, default_context, default_environment, entry_path,
        run_observed_snapshot, source_check_request,
    };
    use hirsel_front_phases::compiler_adapters;
    use hirsel_intern::jobs::with_compiler_stack;
    let shared = SOURCE
        .replace(" in a = ", " = ")
        .replace(
            "  length(list.pop(mut xs)) + length(pop(mut xs))",
            "  let saved = copy xs\n  xs = unique xs\n  let removed = length(list.pop(mut xs)) + length(pop(mut xs))\n  failure.check(list.len(saved) == 2 && saved[0] == \"arena\" && saved[1] == \"text\", TextInvariant()); removed",
        );
    for source in [SOURCE, shared.as_str()] {
        with_compiler_stack(|| {
            let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
            let file = engine.sources.add("test.hir", source.into()).unwrap();
            let checked = source_check_request(&mut engine, file, &default_context()).unwrap();
            assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
            let entry = entry_path(&default_context());
            let run = run_observed_snapshot(
                &mut engine,
                &RunConfiguration {
                    entry: entry.clone(),
                    hash_seed: 0,
                    inputs: vec![],
                },
                Default::default(),
            )
            .unwrap();
            let reference =
                hirsel_native::Observation::of_reference(run.observation.as_ref().unwrap());
            let input = engine.export_native_input().unwrap();
            let validated = engine.import_native_input(input).unwrap();
            let sites = hirsel_native::Sites::new(validated.snapshot());
            let ready = hirsel_native_batch::build(
                &validated,
                &hirsel_native::Options::new(support::target()),
            )
            .unwrap();
            let heap = hirsel_native_testing::heap_campaign(&ready, &entry, &sites).unwrap();
            assert_eq!(heap.observation, reference, "{source}");
        });
    }
}

#[test]
fn storage_primitive_detaches_only_its_active_loan() {
    for action in [
        "let _ = list.pop(mut xs)",
        "let _ = list.remove(mut xs, 0)",
        "list.swap(mut xs, 0, 1)",
    ] {
        let source = format!(
            "pub fn main() -> Result[(), u64] uses {{}} {{
  let mut xs: List[u64] = []
  list.reserve(mut xs, 4)
  list.push(mut xs, 1)
  list.push(mut xs, 2)
  {action}
  Error(list.len(xs))
}}"
        );
        let (_, lowered) = support::lower_valid(&source);
        let ids = support::instances_of(&lowered, "main");
        assert_eq!(ids.len(), 1);
        let id = ids[0];
        let placed = hirsel_rc::placement::place(
            lowered.units[id.index()].clone(),
            &lowered.table,
            &Options::for_instance(true, &lowered.instances, id),
        )
        .unwrap();
        let rc = hirsel_rc::insert::insert(
            placed,
            &lowered.table,
            &Proofs::for_instance(&lowered.instances, id),
        )
        .unwrap();
        let tests = rc
            .instructions
            .iter()
            .filter(|i| matches!(i, Inst::IsWritable { .. }))
            .count();
        assert_eq!(
            tests,
            1,
            "{action}: only the storage primitive's loan detaches\n{}",
            support::dump(&rc)
        );
    }
}
