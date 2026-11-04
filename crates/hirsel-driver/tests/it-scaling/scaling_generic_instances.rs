use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
/// A body selecting many flat instances of one size-generic declaration once
/// expanded each argument's crossing receipts into a clone of the body's
/// store, so the first expansion copied every body row: quadratic in the
/// number of calls. Expansions now go to proof-local scratch.
#[test]
fn generic_instance_calls_copy_no_body_rows_per_call() {
    with_compiler_stack(|| {
        let work = |count: usize| {
            let calls = (0..count)
                .map(|k| format!("let t{k}: Tag[{k}] = Tag(a: {k})\nlet _ = describe(t{k})\n"))
                .collect::<String>();
            let source = format!(
                "type Tag[n: size] {{ a: u64 }}\nfn describe[T](v: T) -> u64 {{ let _ = v\n1 }}\nfn f() -> u64 {{ {calls}0 }}\n"
            );
            let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
            let file = engine.sources.add("generic-instances.hir", source).unwrap();
            let parsed = engine.parse_file(file, &default_context()).unwrap();
            let before = hirsel_intern::work::read(hirsel_intern::work::Counter::InternedTypeRows);
            let output = engine
                .mode_definition(&parsed.value.items[2].item.definition)
                .unwrap();
            assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
            hirsel_intern::work::read(hirsel_intern::work::Counter::InternedTypeRows) - before
        };
        let (half, full) = (work(64), work(128));
        assert!(
            full <= 3 * half + 1024,
            "{half} new type rows for 64 calls, {full} for 128"
        );
    });
}

#[test]
fn summary_membership_and_sibling_frames_have_bounded_scaling() {
    with_compiler_stack(|| {
        use hirsel_intern::work::{self, Counter};
        let mut probe_path = entry_path(&default_context());
        probe_path.0.name = "probe".into();
        let source = "fn leaf(x: u8) -> u8 uses {} { x }\nfn probe(x: u8) -> u8 { let _ = (fn() -> u8 { leaf(x) })(); comptime leaf(1u8) }";
        let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
        let file = engine
            .sources
            .add("summary-nesting.hir", source.into())
            .unwrap();
        engine.parse_file(file, &default_context()).unwrap();
        let output = engine.mode_definition(&probe_path).unwrap();
        assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
        let (summary, _) = summarize(engine.types(), &output.value.typed);
        assert_eq!(
            summary
                .frames
                .iter()
                .map(|f| (f.kind, f.parent))
                .collect::<Vec<_>>(),
            [
                (FrameKind::Item, None),
                (FrameKind::Closure, Some(0)),
                (FrameKind::ComptimeRegion, Some(0)),
            ]
        );
        assert_eq!(
            summary
                .calls
                .iter()
                .map(|c| (c.owning_frame, c.target_frame, c.class))
                .collect::<Vec<_>>(),
            [
                (1, None, CallClass::Runtime),
                (0, Some(1), CallClass::Runtime),
                (2, None, CallClass::ComptimeExecuted),
            ]
        );
        let mut measurements = Vec::new();
        for family in ["calls", "growth", "frames"] {
            let mut units = Vec::new();
            for n in [128, 1024] {
                let source = match family {
                    "calls" => format!(
                        "fn leaf(x: u8) -> u8 {{ x }}\nfn probe(x: u8) -> u8 {{\n{}x\n}}",
                        "leaf(x)\n".repeat(n)
                    ),
                    "growth" => format!(
                        "fn leaf[T: copyable](x: T) {{ let _ = x }}\nfn probe[T: copyable](x: T) {{\n{}}}",
                        "leaf(x)\n".repeat(n)
                    ),
                    _ => format!(
                        "fn probe(x: u8) -> u8 {{\n{}x\n}}",
                        (0..n)
                            .map(|i| format!("let f{i} = fn() -> u8 {{ x }}\n"))
                            .collect::<String>()
                    ),
                };
                let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
                let file = engine.sources.add("summary-scale.hir", source).unwrap();
                let parsed = engine.parse_file(file, &default_context()).unwrap();
                assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
                let output = engine.mode_definition(&probe_path).unwrap();
                assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
                let before = work::read(Counter::SummaryComparisons);
                let (summary, growth) = summarize(engine.types(), &output.value.typed);
                units.push(work::read(Counter::SummaryComparisons) - before);
                // The source has exactly n independent written sites. Planned
                // duplicates retain the first written call and its runtime class.
                if family == "frames" {
                    assert_eq!(summary.frames.len(), n + 1);
                    assert!(
                        summary.frames[1..]
                            .iter()
                            .all(|f| f.parent == Some(0) && f.kind == FrameKind::Closure)
                    );
                } else {
                    assert_eq!(summary.calls.len(), n);
                    assert!(
                        summary
                            .calls
                            .iter()
                            .all(|c| c.owning_frame == 0 && c.class == CallClass::Runtime)
                    );
                    assert!(
                        summary
                            .calls
                            .windows(2)
                            .all(|c| c[0].site.start < c[1].site.start)
                    );
                    assert_eq!(growth.edges.len(), if family == "growth" { n } else { 0 });
                }
            }
            eprintln!(
                "{family}: {} / {} comparison/index units",
                units[0], units[1]
            );
            measurements.push((family, units[0], units[1]));
        }
        eprintln!("summary comparison work: {measurements:?}");
        // A nesting sweep is linear; ordered membership adds logarithmic
        // comparisons, whose growth for these widths is below another 2x.
        assert!(
            measurements
                .iter()
                .all(|(_, n, eight_n)| *n > 0 && *eight_n <= 16 * n),
            "{measurements:?}"
        );
    });
}
