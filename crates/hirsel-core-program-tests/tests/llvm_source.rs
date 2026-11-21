//! Source LLVM agreement uses independently checked definition units.
#![allow(dead_code)]
use hirsel_core_ir::eval;
use hirsel_core_ir::*;
use hirsel_core_layout::Target;
use hirsel_llvm::core::symbol;
use hirsel_llvm::core::*;
use std::{
    fmt::Write as _,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
hirsel_guard::install!();
#[path = "../../hirsel-core-rt/testdata/host_memory.rs"]
mod host_memory;
include!("../../hirsel-llvm/tests/support/native.rs");
include!("support/flow.rs");

#[test]
fn scalar_record_calls_agree_with_evaluator() {
    let source = r#"
type Single { n: u64 }
type Mixed { tag: u8, value: f64 }
type Wide { left: u64, right: u64 }

fn single(n: Single) -> Single { Single(n: n.n + 1) }

fn recurse(x: Mixed, n: u64) -> Mixed {
    if n == 0 { return x }
    recurse(Mixed(tag: x.tag, value: x.value + 0.5), n - 1)
}

fn wide(x: Wide, divisor: u64) -> Wide {
    Wide(left: x.right / divisor, right: x.left)
}

fn apply(f: fn(Wide, u64) -> Wide, x: Wide, n: u64) -> Wide { f(x, n) }

fn revise(mut x: Mixed) { x.tag = 7 }

fn probe(s: u64, divisor: u64) -> u64 {
    let ran = bound steps s {
        let one = single(Single(n: 21u64))
        let mut mixed = recurse(Mixed(tag: 5u8, value: 1.5), 4)
        revise(mut mixed)
        let two = apply(wide, Wide(left: 10u64, right: 20u64), divisor)
        if mixed.tag == 7 && float.to_bits64(mixed.value) == 4615063718147915776 {
            one.n + two.left + two.right
        } else {
            0u64
        }
    }
    match ran { Ok(value) => value, Error(e) => 1000u64 + e.steps }
}
"#;
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let ir = emit_ir(&program).unwrap();
    let wide = symbol(&program, names["wide"]);
    let wide_header = format!("{{ i64, i64, i8 }} @{wide}(");
    assert!(
        ir.lines()
            .filter(|line| line.starts_with("define "))
            .any(|line| {
                let Some((_, args)) = line.split_once(&wide_header) else {
                    return false;
                };
                let types = args
                    .split(')')
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|param| param.split_whitespace().next().unwrap());
                types.eq(["ptr", "i64", "i64", "i64", "i64"])
            }),
        "two u64 leaves and failure status cross the call as immediate results"
    );
    let cases: Vec<_> = (0..=11)
        .flat_map(|steps| [0, 2].map(move |divisor| (steps, divisor)))
        .map(|(steps, divisor)| Case {
            function: names["probe"],
            args: vec![steps, divisor],
            limit: 1_000,
            injection: None,
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|case| {
            let (steps, divisor) = (case.args[0], case.args[1]);
            // single, recurse (entry and four self-calls), revise, apply, and
            // the indirect call to wide cost nine Steps. Only success calls to_bits64.
            let outcome = if steps < if divisor == 0 { 9 } else { 10 } {
                eval::Outcome::Returned(vec![1000 + steps])
            } else if divisor == 0 {
                eval::Outcome::Failed(vec!["Division".into()])
            } else {
                eval::Outcome::Returned(vec![42])
            };
            let evaluated = eval::run(&program, case.function, &case.args, case.limit);
            assert_eq!(evaluated.outcome, outcome, "{:?}", case.args);
            evaluated
        })
        .collect();
    let cranelift = program.cranelift_object("x86_64-unknown-linux-gnu", false, false, false);
    let llvm = program.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
    for (optimization, object) in [(Optimization::None, cranelift), (Optimization::Speed, llvm)] {
        let reports = execute_object(&program, &cases, "", "", optimization, Some(object));
        for ((case, evaluated), native) in cases.iter().zip(&expected).zip(reports) {
            assert_eq!(native.outcome, evaluated.outcome, "{:?}", case.args);
            assert_eq!(native.used, evaluated.used);
            let sites: Vec<_> = evaluated
                .sites
                .iter()
                .map(|anchor| {
                    anchor.map_or(0, |a| program.sites[a.item.index()] + u64::from(a.start))
                })
                .collect();
            assert_eq!(native.sites, sites, "{:?}", case.args);
        }
    }
}

/// A constant divisor drops a division's checks only where the division cannot
/// fail (`failure::constant_safe`): a signed quotient by -1 still raises
/// Overflow at its type's minimum, also at a narrow width, and a zero divisor
/// still raises Division. Both native engines agree with the evaluator.
#[test]
fn constant_divisors_keep_reachable_failures() {
    let source = r#"
fn by_minus_one(x: i64) -> i64 { x / -1i64 }
fn rem_minus_one(x: i64) -> i64 { x % -1i64 }
fn narrow_minus_one(x: u64) -> u64 {
    let n: i8 = int.truncate(x)
    let q: i8 = n / -1i8
    let w: u8 = int.truncate(q)
    int.widen(w)
}
fn by_zero(x: u64) -> u64 { x / 0u64 }
fn by_seven(x: i64) -> i64 { x / 7i64 + x % 7i64 }
"#;
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let min = i64::MIN as u64;
    let calls = [
        ("by_minus_one", min),
        ("by_minus_one", 5),
        ("by_minus_one", -5i64 as u64),
        ("rem_minus_one", min),
        ("narrow_minus_one", 0x80),
        ("narrow_minus_one", 5),
        ("by_zero", 1),
        ("by_seven", min),
        ("by_seven", -15i64 as u64),
        ("by_seven", 15),
    ];
    let cases: Vec<_> = calls
        .iter()
        .map(|&(name, x)| Case {
            function: names[name],
            args: vec![x],
            limit: 1_000,
            injection: None,
        })
        .collect();
    // Hand-derived: MIN / -1 and -128i8 / -1 overflow, x / 0 is a Division,
    // and x / 7 + x % 7 truncates toward zero (-15 gives -2 + -1).
    let overflow = || eval::Outcome::Failed(vec!["Overflow".into()]);
    let expected = [
        overflow(),
        eval::Outcome::Returned(vec![-5i64 as u64]),
        eval::Outcome::Returned(vec![5]),
        eval::Outcome::Returned(vec![0]),
        overflow(),
        eval::Outcome::Returned(vec![0xFB]),
        eval::Outcome::Failed(vec!["Division".into()]),
        eval::Outcome::Returned(vec![(i64::MIN / 7 + i64::MIN % 7) as u64]),
        eval::Outcome::Returned(vec![-3i64 as u64]),
        eval::Outcome::Returned(vec![3]),
    ];
    let evaluated: Vec<_> = cases
        .iter()
        .zip(&expected)
        .map(|(case, outcome)| {
            let run = eval::run(&program, case.function, &case.args, case.limit);
            assert_eq!(&run.outcome, outcome, "f{}{:?}", case.function.0, case.args);
            run
        })
        .collect();
    let exports = calls.map(|(name, _)| names[name]);
    let cranelift = program.cranelift_object("x86_64-unknown-linux-gnu", false, false, false);
    let llvm = program.llvm_program(Target::X86_64Linux, false, false, &exports);
    for (optimization, object) in [(Optimization::None, cranelift), (Optimization::Speed, llvm)] {
        let reports = execute_object(&program, &cases, "", "", optimization, Some(object));
        for ((case, evaluated), native) in cases.iter().zip(&evaluated).zip(reports) {
            assert_eq!(
                native.outcome, evaluated.outcome,
                "f{}{:?}",
                case.function.0, case.args
            );
        }
    }
}

/// Release LLVM drops a loop's failure check only when ScalarEvolution proves it
/// never fires. The Fannkuch flip (`j - i` for `i < (k + 1) >> 1`) agrees with
/// the evaluator; variants that reach the boundary (an extra trip, a trip count
/// only data decides, an overflow at the type's top bit) still fail with the
/// evaluator's text and site.
#[test]
fn proven_loop_checks_keep_reachable_failures() {
    let source = r#"
fn flip(kw: u64) -> u64 {
    let mut perm: Inline[i32, 32] = [0i32; 32]
    for i in 0u64 .. 32u64 {
        perm[i] = int.narrow(i)
    }
    let k: i32 = int.narrow(kw)
    let half: u64 = int.narrow((k + 1) >> 1i32)
    for i in 0 .. half {
        let j: u64 = int.narrow(k)
        let x = perm[i]
        perm[i] = perm[j - i]
        perm[j - i] = x
    }
    let mut total: u64 = 0
    for at in 0u64 .. 32u64 {
        let v: u32 = int.truncate(perm[at])
        total = int.wrapping_add(int.wrapping_mul(total, 31u64), int.widen(v))
    }
    total
}

fn flip_over(kw: u64, extra: u64) -> u64 {
    let mut perm: Inline[i32, 32] = [0i32; 32]
    for i in 0u64 .. 32u64 {
        perm[i] = int.narrow(i)
    }
    let k: i32 = int.narrow(kw)
    let half: u64 = int.narrow((k + 1) >> 1i32)
    for i in 0 .. half + extra {
        let j: u64 = int.narrow(k)
        let x = perm[i]
        perm[i] = perm[j - i]
        perm[j - i] = x
    }
    let mut total: u64 = 0
    for at in 0u64 .. 32u64 {
        let v: u32 = int.truncate(perm[at])
        total = int.wrapping_add(int.wrapping_mul(total, 31u64), int.widen(v))
    }
    total
}

fn reach(j: u64, extra: u64) -> u64 {
    let mut perm: Inline[i32, 32] = [0i32; 32]
    for i in 0u64 .. 32u64 {
        perm[i] = int.narrow(i)
    }
    for i in 0 .. j + extra {
        let x = perm[i]
        perm[i] = perm[j - i]
        perm[j - i] = x
    }
    let mut total: u64 = 0
    for at in 0u64 .. 32u64 {
        let v: u32 = int.truncate(perm[at])
        total = int.wrapping_add(int.wrapping_mul(total, 31u64), int.widen(v))
    }
    total
}

fn unknown(j: u64, n: u64) -> u64 {
    let mut perm: Inline[i32, 32] = [0i32; 32]
    for i in 0u64 .. 32u64 {
        perm[i] = int.narrow(i)
    }
    let stop: i32 = int.narrow(n)
    let mut i: u64 = 0
    while perm[i] != stop {
        let x = perm[i]
        perm[i] = perm[j - i]
        perm[j - i] = x
        i += 1
    }
    let mut total: u64 = 0
    for at in 0u64 .. 32u64 {
        let v: u32 = int.truncate(perm[at])
        total = int.wrapping_add(int.wrapping_mul(total, 31u64), int.widen(v))
    }
    int.wrapping_add(total, i)
}
"#;
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let mut calls = Vec::new();
    for k in (0..34).chain([2_147_483_647, 2_147_483_648]) {
        calls.push(("flip", vec![k]));
    }
    for k in [0, 1, 2, 7, 30, 31] {
        for extra in [0, 1, 2, 9] {
            calls.push(("flip_over", vec![k, extra]));
        }
    }
    for j in [0, 1, 2, 5, 31] {
        for extra in 0..4 {
            calls.push(("reach", vec![j, extra]));
        }
        for n in [0, 1, 3, 40] {
            calls.push(("unknown", vec![j, n]));
        }
    }
    let cases: Vec<_> = calls
        .into_iter()
        .map(|(name, args)| Case {
            function: names[name],
            args,
            limit: u64::MAX,
            injection: None,
        })
        .collect();
    let exports = ["flip", "flip_over", "reach", "unknown"].map(|name| names[name]);
    let object = program.llvm_program(Target::X86_64Linux, false, false, &exports);
    let reports = execute_object(&program, &cases, "", "", Optimization::Speed, Some(object));
    let mut failed = 0;
    for (case, native) in cases.iter().zip(reports) {
        let evaluated = eval::run(&program, case.function, &case.args, case.limit);
        // The flip never underflows: i < (k + 1) >> 1 <= k for every k >= 1.
        if case.function == names["flip"] && case.args[0] < 32 {
            assert!(
                matches!(evaluated.outcome, eval::Outcome::Returned(_)),
                "{:?}",
                case.args
            );
        }
        failed += usize::from(matches!(evaluated.outcome, eval::Outcome::Failed(_)));
        assert_eq!(
            native.outcome, evaluated.outcome,
            "f{}{:?}",
            case.function.0, case.args
        );
        let sites: Vec<_> = evaluated
            .sites
            .iter()
            .map(|anchor| anchor.map_or(0, |a| program.sites[a.item.index()] + u64::from(a.start)))
            .collect();
        assert_eq!(native.sites, sites, "f{}{:?}", case.function.0, case.args);
    }
    assert!(failed > 0, "no case reaches a boundary");
}

/// Release LLVM drops a counter's shift and increment checks only when a
/// reset keeps it in range on every path (`counter_range.h`). The packer
/// resets `pos` at 8, so `7u8 - pos` and `pos += 1` never fail; a reset at 9,
/// a reset on only one path, and a second write to `pos` each let `pos` reach
/// 8 at the shift, which still fails with the evaluator's text and site on
/// both native engines. `guarded` counts under a data-dependent guard, which
/// release LLVM if-converts (`guarded_check.h`): the count fails only when the
/// guard holds at 255, never when it is skipped there.
#[test]
fn counter_resets_keep_reachable_failures() {
    let source = r#"
fn packed(n: u64) -> u64 {
    let mut current: u8 = 0
    let mut pos: u8 = 0
    let mut total: u64 = 0
    for i in 0u64 .. n {
        if i % 3u64 != 0u64 {
            current |= 1u8 << (7u8 - pos)
        }
        pos += 1
        if pos == 8 {
            total += int.widen(current)
            current = 0
            pos = 0
        }
    }
    total * 16u64 + int.widen(pos)
}

fn reset_at_nine(n: u64) -> u64 {
    let mut current: u8 = 0
    let mut pos: u8 = 0
    let mut total: u64 = 0
    for i in 0u64 .. n {
        if i % 3u64 != 0u64 {
            current |= 1u8 << (7u8 - pos)
        }
        pos += 1
        if pos == 9 {
            total += int.widen(current)
            current = 0
            pos = 0
        }
    }
    total * 16u64 + int.widen(pos)
}

fn one_path(n: u64, keep: u64) -> u64 {
    let mut current: u8 = 0
    let mut pos: u8 = 0
    let mut total: u64 = 0
    for i in 0u64 .. n {
        if i % 3u64 != 0u64 {
            current |= 1u8 << (7u8 - pos)
        }
        pos += 1
        if pos == 8 {
            total += int.widen(current)
            current = 0
            if keep == 0u64 {
                pos = 0
            }
        }
    }
    total * 16u64 + int.widen(pos)
}

fn bumped(n: u64, skip: u64) -> u64 {
    let mut current: u8 = 0
    let mut pos: u8 = 0
    let mut total: u64 = 0
    for i in 0u64 .. n {
        if i == skip {
            pos += 1
        }
        if i % 3u64 != 0u64 {
            current |= 1u8 << (7u8 - pos)
        }
        pos += 1
        if pos == 8 {
            total += int.widen(current)
            current = 0
            pos = 0
        }
    }
    total * 16u64 + int.widen(pos)
}

fn guarded(n: u64, every: u64, start: u64) -> u64 {
    let mut count: u8 = int.narrow(start)
    for i in 0u64 .. n {
        if i % every == 0u64 {
            count += 1
        }
    }
    int.widen(count)
}
"#;
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    // Hand-derived. Bit i is set when i % 3 != 0, at bit 7 - pos. With a reset
    // at 8 the bytes repeat 109 (0b01101101), 182 (0b10110110), 219
    // (0b11011011); the result is the sum of completed bytes * 16 + pos.
    // Without a reset at 8, i = 8 sets bit 7 - 8 and fails. In `bumped`, the
    // extra step at i = 3 or 6 shifts the bytes to 102 (i 0..6) or 108
    // (i 0..5), then 219 (i 7..14); at i = 7 it moves pos to 8 before a set
    // bit, and at i = 15 (pos 7, no bit) it carries pos past 8 to 9, so the
    // set bit at i = 16 fails. `guarded` adds one per i in 0..n divisible by
    // `every`: from 254 every 5th of 4 reaches 255 at i = 0 and skips at 255
    // for i = 1..3; every 4th of 6 reaches 256 at i = 4.
    let overflow = || eval::Outcome::Failed(vec!["Overflow".into()]);
    let returned = |x: u64| eval::Outcome::Returned(vec![x]);
    let laws = [
        ("packed", vec![0], returned(0)),
        ("packed", vec![5], returned(5)),
        ("packed", vec![8], returned(109 * 16)),
        ("packed", vec![20], returned((109 + 182) * 16 + 4)),
        ("packed", vec![24], returned((109 + 182 + 219) * 16)),
        ("reset_at_nine", vec![8], returned(8)),
        ("reset_at_nine", vec![9], overflow()),
        ("one_path", vec![20, 0], returned((109 + 182) * 16 + 4)),
        ("one_path", vec![8, 1], returned(109 * 16 + 8)),
        ("one_path", vec![9, 1], overflow()),
        ("bumped", vec![20, 100], returned((109 + 182) * 16 + 4)),
        ("bumped", vec![20, 3], returned((102 + 219) * 16 + 5)),
        ("bumped", vec![20, 6], returned((108 + 219) * 16 + 5)),
        ("bumped", vec![9, 7], overflow()),
        ("bumped", vec![16, 15], returned(109 * 16 + 9)),
        ("bumped", vec![17, 15], overflow()),
        ("guarded", vec![6, 2, 250], returned(253)),
        ("guarded", vec![4, 5, 254], returned(255)),
        ("guarded", vec![6, 3, 253], returned(255)),
        ("guarded", vec![6, 4, 254], overflow()),
        ("guarded", vec![7, 3, 253], overflow()),
    ];
    let cases: Vec<_> = laws
        .iter()
        .map(|(name, args, _)| Case {
            function: names[*name],
            args: args.clone(),
            limit: 1_000_000,
            injection: None,
        })
        .collect();
    let evaluated: Vec<_> = cases
        .iter()
        .zip(&laws)
        .map(|(case, (_, _, outcome))| {
            let run = eval::run(&program, case.function, &case.args, case.limit);
            assert_eq!(&run.outcome, outcome, "f{}{:?}", case.function.0, case.args);
            run
        })
        .collect();
    let exports =
        ["packed", "reset_at_nine", "one_path", "bumped", "guarded"].map(|name| names[name]);
    let cranelift = program.cranelift_object("x86_64-unknown-linux-gnu", false, false, false);
    let llvm = program.llvm_program(Target::X86_64Linux, false, false, &exports);
    for (optimization, object) in [(Optimization::None, cranelift), (Optimization::Speed, llvm)] {
        let reports = execute_object(&program, &cases, "", "", optimization, Some(object));
        for ((case, evaluated), native) in cases.iter().zip(&evaluated).zip(reports) {
            assert_eq!(
                native.outcome, evaluated.outcome,
                "f{}{:?}",
                case.function.0, case.args
            );
            let sites: Vec<_> = evaluated
                .sites
                .iter()
                .map(|anchor| {
                    anchor.map_or(0, |a| program.sites[a.item.index()] + u64::from(a.start))
                })
                .collect();
            assert_eq!(native.sites, sites, "f{}{:?}", case.function.0, case.args);
        }
    }
}

const RESET_COMPARISONS: [(&str, &str, &str, &str); 4] = [
    ("lt", "0u64", "i < trips", "add"),
    ("le", "1u64", "i <= trips", "add"),
    ("gt", "trips", "i > 0u64", "sub"),
    ("ge", "trips", "i >= 1u64", "sub"),
];

fn append_reset_loop(
    source: &mut String,
    name: &str,
    ty: &str,
    width: i64,
    reset: i64,
    (start, condition, direction): (&str, &str, &str),
) {
    let last = width - 1;
    let widen = if ty == "i8" {
        "let low: u8 = int.truncate(bits)\n        let word: u64 = int.widen(low)"
    } else {
        "let word: u64 = int.widen(bits)"
    };
    writeln!(
        source,
        r#"
fn {name}(budget: u64, trips: u64) -> u64 {{
    match (bound steps budget {{
        let mut i = {start}
        let mut pos = 0{ty}
        let mut bits = 0{ty}
        while {condition} {{
            bits ^= 1{ty} << ({last}{ty} - pos)
            pos += 1
            if pos == {reset} {{ pos = 0 }}
            i = int.wrapping_{direction}(i, 1u64)
        }}
        {widen}
        word
    }}) {{ Ok(word) => word, Error(e) => 65536u64 | e.steps }}
}}
"#,
    )
    .unwrap();
}

#[test]
fn seeded_scalar_counted_loops_agree_across_engines() {
    // SEM-1 and MEM-30: the shift by 7 succeeds, a shift by 8 or -1 fails,
    // and a checked u8 increment past 255 fails. Only completed back edges
    // and calls spend steps, so a failure can occur with zero steps left.
    let mut source = String::from(
        r#"
fn last_bit(budget: u64, end: u8) -> u64 {
    match (bound steps budget {
        let mut i = 7u8
        let mut bits = 0u8
        while i <= end {
            bits ^= 1u8 << i
            i += 1
        }
        let word: u64 = int.widen(bits)
        word
    }) { Ok(bits) => bits, Error(e) => 65536u64 + e.steps }
}
fn negative_shift(budget: u64, stop: i8) -> u64 {
    match (bound steps budget {
        let mut i = 2i8
        let mut bits = 0i8
        while i >= 0 {
            if i == stop { return if bits == 3 { 41u64 } else { 42u64 } }
            bits ^= 1i8 << (i - 1i8)
            i -= 1
        }
        0u64
    }) { Ok(bits) => bits, Error(e) => 65536u64 + e.steps }
}
fn stride_overflow(budget: u64, end: u8) -> u64 {
    match (bound steps budget {
        let mut i = 253u8
        while i <= end { i += 2 }
        let word: u64 = int.widen(i)
        word
    }) { Ok(i) => i, Error(e) => 65536u64 + e.steps }
}
"#,
    );
    let returned = |value| eval::Outcome::Returned(vec![value]);
    let overflow = || eval::Outcome::Failed(vec!["Overflow".into()]);
    let mut sentinels = vec![
        ("last_bit", [1, 6], returned(0)),
        ("last_bit", [1, 7], returned(65537)),
        ("last_bit", [2, 7], returned(128)),
        ("last_bit", [0, 8], returned(65536)),
        ("last_bit", [1, 8], overflow()),
        ("negative_shift", [1, 0], returned(65537)),
        ("negative_shift", [2, 0], returned(41)),
        ("negative_shift", [2, 3], overflow()),
        ("stride_overflow", [1, 254], returned(65537)),
        ("stride_overflow", [2, 254], returned(255)),
        ("stride_overflow", [1, 255], overflow()),
    ];
    // At reset 8, pos is 0..7 at the shift and 1..8 after incrementing.
    // Nine trips XOR all eight bits then the high bit again, yielding 127.
    // Each trip charges the wrapping call and the back edge, and widening
    // charges one more. Resetting at 9 instead reaches 7 - 8 on trip nine.
    let must_fold = [
        "must_fold_lt",
        "must_fold_le",
        "must_fold_gt",
        "must_fold_ge",
    ];
    for (name, (_, start, condition, direction)) in must_fold.into_iter().zip(RESET_COMPARISONS) {
        append_reset_loop(&mut source, name, "u8", 8, 8, (start, condition, direction));
        sentinels.push((name, [18, 9], returned(65536 | 18)));
        sentinels.push((name, [19, 9], returned(127)));
    }
    append_reset_loop(
        &mut source,
        "must_keep",
        "u8",
        8,
        9,
        ("1u64", "i <= trips", "add"),
    );
    sentinels.push(("must_keep", [15, 9], returned(65536 | 15)));
    sentinels.push(("must_keep", [16, 9], overflow()));
    let mut calls: Vec<_> = sentinels
        .iter()
        .map(|(name, args, outcome)| (name.to_string(), args.to_vec(), Some(outcome)))
        .collect();

    // SplitMix64, with a fixed corpus size and a function name that identifies
    // its seed, width and body. Bounds are literal so release can prove ranges.
    const SEEDS: [u64; 2] = [0x1543, 0x9e37_79b9_7f4a_7c15];
    for (ty, width) in [("u8", 8i64), ("i8", 8), ("u16", 16)] {
        writeln!(source, "fn keep_{ty}(mut i: {ty}, value: {ty}) {{}}").unwrap();
        writeln!(
            source,
            "fn write_{ty}(mut i: {ty}, value: {ty}) {{ i = value }}"
        )
        .unwrap();
        for seed in SEEDS {
            let mut state = seed ^ ((ty.as_bytes()[0] as u64) << 16) ^ width as u64;
            let mut choose = |options: u64| {
                state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut x = state;
                x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                (x ^ (x >> 31)) % options
            };
            let update = format!(
                "let writer: fn(mut {ty}, {ty}) = if write_now {{ write_{ty} }} else {{ keep_{ty} }}"
            );
            let widen = if ty == "i8" {
                "let low: u8 = int.truncate(bits)\n        let word: u64 = int.widen(low)"
            } else {
                "let word: u64 = int.widen(bits)"
            };
            for shape in ["plain", "reset", "write", "alias", "break", "return"] {
                let name = format!("range_{seed:x}_{ty}_{shape}");
                let (comparison, advance, direction) = match choose(4) {
                    0 => ("<", "+=", 1),
                    1 => ("<=", "+=", 1),
                    2 => (">", "-=", -1),
                    _ => (">=", "-=", -1),
                };
                let edge = if direction > 0 { width - 1 } else { 1 };
                let start = edge - direction * 2 + choose(3) as i64 - 1;
                let end = edge + choose(3) as i64 - 1;
                let step = 1 + choose(3);
                let offset = [" - 1", "", " + 1"][choose(3) as usize];
                let reset = start - direction;
                let (before_loop, action) = match shape {
                    "plain" => ("", String::new()),
                    "reset" => ("", format!("if i == at {{ at = 100; i = {reset}{ty} }}")),
                    "write" => ("", "if i == at { at = 100; i = value }".into()),
                    "alias" => (
                        update.as_str(),
                        "if i == at { at = 100; writer(mut i, value) }".into(),
                    ),
                    "break" => ("", "if i == at { break }".into()),
                    "return" => ("", "if i == at { return 1000u64 }".into()),
                    _ => unreachable!(),
                };
                writeln!(
                    source,
                    r#"
fn {name}(budget: u64, trigger: {ty}, value: {ty}, write_now: bool) -> u64 {{
    match (bound steps budget {{
        let mut i = {start}{ty}
        let mut at = trigger
        let mut bits = 0{ty}
        {before_loop}
        while i {comparison} {end}{ty} {{
            {action}
            bits ^= 1{ty} << (i{offset})
            i {advance} {step}{ty}
        }}
        {widen}
        word
    }}) {{ Ok(bits) => bits, Error(e) => 65536u64 + e.steps }}
}}
"#,
                )
                .unwrap();
                // The one-shot mutation keeps the trip count bounded even if it
                // moves the counter against the stride. Adjacent budgets expose
                // both the successful and failing path's exact charged prefix.
                let inputs: &[(i64, u64)] = match shape {
                    "plain" => &[(100, 0)],
                    "alias" => &[(100, 0), (start, 0), (start, 1)],
                    _ => &[(100, 0), (start, 0)],
                };
                let replacement = match (direction, ty) {
                    (1, _) => width as u64,
                    (_, "i8") => u64::from(u8::MAX), // -1i8
                    _ => 0,
                };
                for &(trigger, replace) in inputs {
                    for budget in 0..=12 {
                        calls.push((
                            name.clone(),
                            vec![budget, trigger as u64, replacement, replace],
                            None,
                        ));
                    }
                }
            }
            // The trip bound stays dynamic so an earlier unroller cannot
            // resolve the recurring reset before CounterRangePass sees it.
            for (comparison, start, condition, direction) in RESET_COMPARISONS {
                let name = format!("range_{seed:x}_{ty}_cycle_{comparison}");
                let reset = width - choose(2) as i64;
                let trips = 7 + choose(5);
                append_reset_loop(
                    &mut source,
                    &name,
                    ty,
                    width,
                    reset,
                    (start, condition, direction),
                );
                for budget in 0..=24 {
                    calls.push((name.clone(), vec![budget, trips], None));
                }
            }
        }
    }

    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(&source, None);
    let cases: Vec<_> = calls
        .iter()
        .map(|(name, args, _)| Case {
            function: names[name],
            args: args.clone(),
            limit: 1_000_000,
            injection: None,
        })
        .collect();
    let mut exports: Vec<_> = cases.iter().map(|case| case.function).collect();
    exports.sort_unstable_by_key(|f| f.0);
    exports.dedup();
    let evaluated: Vec<_> = cases
        .iter()
        .zip(&calls)
        .map(|(case, (name, args, expected))| {
            let run = eval::run(&program, case.function, &case.args, case.limit);
            if let Some(expected) = expected {
                assert_eq!(&run.outcome, *expected, "{name}{args:?}");
            }
            run
        })
        .collect();
    let cranelift = program.cranelift_object("x86_64-unknown-linux-gnu", false, false, false);
    let (llvm, folds) =
        program.llvm_program_with_counter_range_folds(Target::X86_64Linux, &exports);
    let mut differences = Vec::new();
    for (engine, optimization, object) in [
        ("Cranelift dev", Optimization::None, cranelift),
        ("LLVM release", Optimization::Speed, llvm),
    ] {
        let reports = execute_object(&program, &cases, "", "", optimization, Some(object));
        assert_eq!(reports.len(), cases.len(), "{engine} report count");
        for (((name, args, _), oracle), native) in calls.iter().zip(&evaluated).zip(reports) {
            let sites: Vec<_> = oracle
                .sites
                .iter()
                .map(|&anchor| anchor_word(&program.sites, anchor))
                .collect();
            if (&native.outcome, &native.sites) != (&oracle.outcome, &sites) {
                differences.push(format!(
                    "{engine} {name}{args:?}: got {:?} at {:?}, expected {:?} at {sites:?}",
                    native.outcome, native.sites, oracle.outcome
                ));
            }
        }
    }
    eprintln!(
        "counted-loop seeds {SEEDS:x?}, {} cases per engine",
        cases.len()
    );
    assert!(differences.is_empty(), "{}", differences.join("\n"));

    let folded = |name: &str| {
        let symbol = symbol(&program, names[name]);
        folds.get(symbol.as_bytes()).copied().unwrap_or(0)
    };
    // Count both compiled functions and their invocations. Budget sweeps
    // must not let one function supply an entire seed's fold coverage.
    const MIN_FOLD_PERCENT: usize = 25;
    for seed in SEEDS {
        let prefix = format!("range_{seed:x}_");
        let tally = |names: &mut dyn Iterator<Item = &str>| {
            names
                .filter(|name| name.starts_with(&prefix))
                .fold((0, 0), |(removed, total), name| {
                    (removed + usize::from(folded(name) > 0), total + 1)
                })
        };
        let (shapes_with_folds, shapes) = tally(&mut names.keys().map(String::as_str));
        let (cases_with_folds, invocations) =
            tally(&mut calls.iter().map(|(name, _, _)| name.as_str()));
        eprintln!(
            "seed {seed:#x}: {shapes_with_folds}/{shapes} shapes, {cases_with_folds}/{invocations} cases folded by CounterRangePass"
        );
        assert!(
            shapes > 0 && shapes_with_folds * 100 >= shapes * MIN_FOLD_PERCENT,
            "seed {seed:#x} requires folds in at least {MIN_FOLD_PERCENT}% of its shapes"
        );
        assert!(
            invocations > 0 && cases_with_folds * 100 >= invocations * MIN_FOLD_PERCENT,
            "seed {seed:#x} requires folds in at least {MIN_FOLD_PERCENT}% of its cases"
        );
    }
    for name in must_fold {
        let count = folded(name);
        eprintln!("{name}: {count} checks folded by CounterRangePass");
        assert!(count > 0, "{name} must fold at least one check on pos");
    }
}

#[test]
fn native_flow_corpus_agrees_with_the_owner_laws() {
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(FLOW_SOURCE, None);
    let mut cases: Vec<_> = FLOW_CASES
        .iter()
        .map(|name| Case {
            function: names[*name],
            args: vec![],
            limit: 4000,
            injection: None,
        })
        .collect();
    cases.push(Case {
        function: names["foreign_push"],
        args: vec![],
        limit: 4000,
        injection: None,
    });
    let reports = execute(&program, &cases);
    for (name, report) in FLOW_CASES.iter().zip(&reports) {
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![42]), "{name}");
        assert_eq!(report.used, 0, "{name}");
    }
    assert_eq!(
        reports.last().unwrap().outcome,
        eval::Outcome::Failed(vec!["Bounds".to_owned()])
    );
}

#[test]
fn viewed_carrier_headers_thaw_after_success_error_and_failure() {
    let source = r#"
fn inspect(xs: List[u64], mode: u64) -> Result[u64, Capacity] {
    let mut scratch: List[u64] = list.with_capacity(2)
    let _ = list.push(mut scratch, 7u64)
    let _ = list.push(mut scratch, 8u64)
    let before = list.len(xs)
    if mode == 1 { return Error(Capacity(requested: 1, available: 0)) }
    if mode == 2 { let _ = 1u64 / (mode - 2) }
    Ok(before + list.len(xs) + list.len(scratch))
}
fn probe(mode: u64) -> u64 {
    let mut xs: List[u64] = list.with_capacity(2)
    let _ = list.push(mut xs, 1u64)
    let _ = list.push(mut xs, 2u64)
    let first = result.unwrap(isolate(scoped fn() -> u64 {
        match inspect(xs, mode) { Ok(n) => n, Error(_) => 900u64 }
    }))
    let before: u64 = match take first { Ok(n) => n, Error(take causes) => 901u64 }
    let _ = list.push(mut xs, 3u64)
    let after = result.unwrap(inspect(xs, 0))
    before * 100u64 + after
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let cases = [0, 1, 2].map(|mode| Case {
        function: names["probe"],
        args: vec![mode],
        limit: 1_000_000,
        injection: None,
    });
    for (case, want) in cases.iter().zip([608, 90008, 90108]) {
        assert_eq!(
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            eval::Outcome::Returned(vec![want])
        );
    }
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            let reports = execute_object(&module, &cases, "", "", optimization, Some(object));
            for (case, report) in cases.iter().zip(reports) {
                assert_eq!(
                    report.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "facts={facts}, mode={:?}, optimization={optimization:?}",
                    case.args
                );
            }
        }
    }
    let object = module.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
    let reports = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
    for (case, report) in cases.iter().zip(reports) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome
        );
    }
}

#[test]
fn plain_list_drop_makes_one_runtime_free_at_both_lengths() {
    let source = include_str!("../../../core/front/cases/rules/MEM-32-plain-drop.hirsel");
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let cases = [1, 1_000_000].map(|length| Case {
        function: names["checksum"],
        args: vec![length],
        limit: 16_000_000,
        injection: None,
    });
    let bindings = r#"
HirselCoreLimitRecord *__real_hirsel_core_block_payer(void *);
HirselCoreLimitRecord *__wrap_hirsel_core_block_payer(void *block) {
    hirsel_core_test_drop_trace(0);
    return __real_hirsel_core_block_payer(block);
}
void __real_hirsel_core_free_value(const HirselCoreContext *, void *, uint64_t);
void __wrap_hirsel_core_free_value(const HirselCoreContext *ctx, void *block, uint64_t bytes) {
    hirsel_core_test_drop_trace(bytes);
    __real_hirsel_core_free_value(ctx, block, bytes);
}
"#;
    let reports = execute_optimized(&module, &cases, bindings, "", Optimization::Speed);
    for (case, report) in cases.iter().zip(reports) {
        let length = case.args[0];
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![length]));
        assert_eq!(report.used, 0);
        assert_eq!(report.trace, vec![length * 8]);
    }
}

impl NativeInput for hirsel_core_program_tests::SourceProgram {
    fn module(&self) -> &Module {
        self
    }
    fn object(&self, target: Target, optimization: Optimization, observing: bool) -> Vec<u8> {
        self.llvm_object(
            target,
            matches!(optimization, Optimization::Speed),
            false,
            false,
            observing,
        )
    }
    fn site_bindings(&self) -> String {
        self.site_bindings()
    }
    fn sites(&self) -> Vec<u64> {
        self.sites.clone()
    }
    fn verify(&self, target: Target, observing: bool) {
        for module in &self.modules {
            verify_ir(&emit_ir_for_test(module, target, observing, false, None).unwrap());
        }
    }
}
fn verify_ir(ir: &str) {
    use llvm_sys::{
        analysis::{LLVMVerifierFailureAction, LLVMVerifyModule},
        core::*,
        ir_reader::LLVMParseIRInContext2,
    };
    use std::{ffi::CStr, ptr};
    // SAFETY: all LLVM objects are owned in this scope; parse consumes the copied buffer.
    unsafe {
        let context = LLVMContextCreate();
        let buffer = LLVMCreateMemoryBufferWithMemoryRangeCopy(
            ir.as_ptr().cast(),
            ir.len(),
            c"test".as_ptr(),
        );
        let mut module = ptr::null_mut();
        let mut error = ptr::null_mut();
        let parsed = LLVMParseIRInContext2(context, buffer, &mut module, &mut error);
        let valid = parsed == 0
            && LLVMVerifyModule(
                module,
                LLVMVerifierFailureAction::LLVMReturnStatusAction,
                &mut error,
            ) == 0;
        let message = if error.is_null() {
            String::new()
        } else {
            let s = CStr::from_ptr(error).to_string_lossy().into_owned();
            LLVMDisposeMessage(error);
            s
        };
        if !module.is_null() {
            LLVMDisposeModule(module);
        }
        LLVMContextDispose(context);
        assert!(valid, "{message}\n{ir}");
    }
}
#[test]
fn every_front_run_case_matches_the_evaluator() {
    fn files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out)
            } else if path.extension().is_some_and(|s| s == "hirsel") {
                out.push(path)
            }
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/front");
    let mut paths = vec![];
    files(&root.join("cases"), &mut paths);
    paths.sort();
    let mut count = 0;
    let mut failures = Vec::new();
    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap();
        let runs = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("// expect: run "))
            .collect::<Vec<_>>();
        if runs.is_empty() {
            continue;
        }
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (module, names) = Ok::<_, String>(hirsel_core_program_tests::SourceProgram::lower(
                &source,
                Some("main"),
            ))
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            // A run marked `evaluator-only:..llvm..` (SITES.md, NOT-BUILT) must still
            // disagree here, so the marker cannot outlive the missing emission.
            let pending = |run: &str| {
                run.split_whitespace().last().is_some_and(|m| {
                    m.strip_prefix("evaluator-only:")
                        .is_some_and(|e| e.split(',').any(|e| e == "llvm"))
                })
            };
            let case = |run: &&str| {
                let (call, _) = run.split_once(" = ").unwrap();
                let (name, args) = call.strip_suffix(')').unwrap().split_once('(').unwrap();
                let function = names[name];
                let sig = &module.signatures[module.functions[function.index()].signature.index()];
                let args = args
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .zip(&sig.params)
                    .map(|(a, t)| {
                        let Param::Value(t) = t else {
                            panic!("front argument")
                        };
                        eval::scalar(*t, a)
                    })
                    .collect();
                Case {
                    function,
                    args,
                    limit: u64::MAX,
                    injection: None,
                }
            };
            let cases = runs
                .iter()
                .filter(|r| !pending(r))
                .map(case)
                .collect::<Vec<_>>();
            println!("front {}: {} native runs", path.display(), cases.len());
            count += cases.len();
            agree_cases(&module, &cases, false);
            let marked = runs
                .iter()
                .filter(|r| pending(r))
                .copied()
                .collect::<Vec<_>>();
            let held = marked.iter().map(case).collect::<Vec<_>>();
            for ((run, case), got) in marked.iter().zip(&held).zip(execute(&module, &held)) {
                let expected = eval::run(&module, case.function, &case.args, case.limit);
                assert_ne!(
                    got.outcome, expected.outcome,
                    "`{run}` now agrees on LLVM: remove `llvm` from its marker and its NOT-BUILT row"
                );
            }
        }));
        if let Err(error) = attempt {
            let message = error
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| error.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            failures.push(format!("{}: {message}", path.display()));
        }
    }
    assert!(count > 0);
    println!(
        "front native executions: {count}; file failures: {}",
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn carrier_program_cases(target: Target) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/front");
    for name in [
        "carrier-twins",
        "cold-call-place",
        "causes-retry",
        "grow-carrier-words",
        "list-put-word",
        "list-nested-mutate",
        "own-20-mut-anchor",
    ] {
        let path = root.join(format!("cases/body/{name}.hirsel"));
        let source = std::fs::read_to_string(&path).unwrap();
        let (module, names) = Ok::<_, String>(hirsel_core_program_tests::SourceProgram::lower(
            &source,
            Some("main"),
        ))
        .unwrap();
        let cases = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("// expect: run "))
            .map(|run| {
                let (call, _) = run.split_once(" = ").unwrap();
                let (name, args) = call.strip_suffix(')').unwrap().split_once('(').unwrap();
                let function = names[name];
                let sig = &module.signatures[module.functions[function.index()].signature.index()];
                let args = args
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .zip(&sig.params)
                    .map(|(a, t)| {
                        let Param::Value(t) = t else {
                            panic!("front argument")
                        };
                        eval::scalar(*t, a)
                    })
                    .collect();
                Case {
                    function,
                    args,
                    limit: u64::MAX,
                    injection: None,
                }
            })
            .collect::<Vec<_>>();
        let object = module.llvm_program(
            target,
            false,
            false,
            &cases.iter().map(|c| c.function).collect::<Vec<_>>(),
        );
        for callee in source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("// words: "))
            .flat_map(str::split_whitespace)
        {
            let twin = format!("{}.words", symbol(&module, names[callee]));
            assert!(
                object.windows(twin.len()).any(|w| w == twin.as_bytes()),
                "{name}: {callee} takes no word path"
            );
        }
        let reports = execute_target_object(
            &module,
            &cases,
            "",
            "",
            Optimization::Speed,
            Some(object),
            target,
        );
        for (case, report) in cases.iter().zip(reports) {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            let at = format!("{name}: f{}({:?})", case.function.0, case.args);
            assert_eq!(report.outcome, expected.outcome, "{at}");
            assert_eq!(report.used, expected.used, "{at}: bytes still admitted");
        }
    }
}

fn cancelled_cold_word_call(target: Target, with_isolate: bool) {
    let source = r#"
fn grow_then_check(mut xs: List[u64]) {
    for n in 0u64 .. 100 {
        list.push(mut xs, n)
    }
    task.check_cancel()
}

fn isolated_body() {
    let mut xs: List[u64] = list.new()
    if list.len(xs) == list.capacity(xs) {
        grow_then_check(mut xs)
    }
}

fn child(take x: u64) -> u64 {
    CALL_BODY
    x
}

fn cancelled(tasks: Tasks) -> u64 waits {
    let mut h = task.spawn(tasks, bytes 65536, 41u64, child)
    task.cancel(mut h)
    match task.join(take h) {
        Ok(n) => n,
        Error(Cancelled(take c)) => {
            let _ = take c
            1
        }
        Error(Failed(take c)) => {
            let _ = take c
            2
        }
    }
}
"#;
    let source = if with_isolate {
        source.replace(
            "CALL_BODY",
            "let _ = isolate(fn() -> () { isolated_body() })",
        )
    } else {
        source.replace("CALL_BODY", "isolated_body()")
    };
    let (module, names) = Ok::<_, String>(hirsel_core_program_tests::SourceProgram::lower(
        &source,
        Some("main"),
    ))
    .unwrap();
    let object = module.llvm_program(target, false, false, &[names["cancelled"]]);
    let twin = format!("{}.words", symbol(&module, names["grow_then_check"]));
    assert!(
        object.windows(twin.len()).any(|w| w == twin.as_bytes()),
        "no {twin}"
    );
    let case = Case {
        function: names["cancelled"],
        args: vec![0],
        limit: u64::MAX,
        injection: None,
    };
    let expected = eval::run(&module, case.function, &case.args, case.limit);
    assert_eq!(expected.used, 0, "evaluator used bytes");
    let report = execute_target_object(
        &module,
        std::slice::from_ref(&case),
        "",
        "",
        Optimization::Speed,
        Some(object),
        target,
    )
    .remove(0);
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![1]));
    println!(
        "cancelled cold word call target={target:?} isolate={with_isolate}: used={} evaluator_used={}",
        report.used, expected.used
    );
    assert_eq!(report.used, expected.used);
}

fn site_fixture(target: Target) {
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
    let source = format!(
        "{}\nfn debug_identity(n: u64) -> u64 {{ n ^ 7 }}\nfn debug_resource(n: u64) -> R {{ match R(d: n) {{ Ok(take r) => take r, Error(_) => loop {{}} }} }}\nfn debug_resource_caller(n: u64) -> u64 {{ let r = debug_resource(n); r.d }}\n",
        include_str!("../../../core/front/cases/body/failure-sites.hirsel")
    );
    let (module, names) = Ok::<_, String>(hirsel_core_program_tests::SourceProgram::lower(
        &source,
        Some("main"),
    ))
    .unwrap();
    let cases = [
        ("add", u64::MAX),
        ("at", 3),
        ("raise", 3),
        ("relay", 3),
        ("twice", u64::MAX),
        ("first", 0),
        ("finalize", 0),
        ("both", 0),
        ("sites", 1),
        ("debug_identity", 7),
        ("debug_resource_caller", 0),
        ("debug_resource_caller", 1),
    ]
    .map(|(name, arg)| Case {
        function: names[name],
        args: vec![arg],
        limit: u64::MAX,
        injection: None,
    });
    let inline_fail_sites = ["child", "other"].map(|name| {
        let definition = &module.functions[names[name].index()];
        let item = module.instances[definition.instance.index()]
            .definition
            .clone();
        let function_start = source.find(&format!("fn {name}(")).unwrap();
        let fail_start = source[function_start..].find("fail(take message)").unwrap();
        (item, fail_start as u32)
    });
    let bindings = module.site_bindings();
    let fixtures = [
        (Optimization::None, false, true),
        (Optimization::Speed, false, true),
        (Optimization::Speed, true, true),
        (Optimization::Speed, false, false),
    ]
    .map(|(opt, composed, debug_lines)| {
        let bytes = if composed {
            module.llvm_program(
                target,
                debug_lines,
                false,
                &cases.iter().map(|c| c.function).collect::<Vec<_>>(),
            )
        } else {
            module.llvm_eager_object(target, opt, debug_lines)
        };
        if debug_lines {
            assert_relative_debug_rows(
                &bytes,
                &module,
                if composed { &inline_fail_sites } else { &[] },
            );
        }
        (opt, composed, debug_lines, bytes)
    });
    for (opt, composed, debug_lines, bytes) in fixtures {
        let file = object::File::parse(bytes.as_slice()).unwrap();
        assert!(
            file.section_by_name(".eh_frame").is_none(),
            "generated object has unwind tables: {target:?} {opt:?}, composed={composed}, debug_lines={debug_lines}"
        );
        let mut site_relocations = 0;
        for section in file.sections() {
            for (_, relocation) in section.relocations() {
                if let object::RelocationTarget::Symbol(index) = relocation.target()
                    && file
                        .symbol_by_index(index)
                        .unwrap()
                        .name()
                        .unwrap()
                        .starts_with("hirsel_core_site_")
                {
                    assert_eq!(
                        relocation.kind(),
                        object::RelocationKind::Absolute,
                        "{target:?} {} {relocation:?}",
                        section.name().unwrap()
                    );
                    // x86-64 code takes a site as a 32-bit immediate (SITE_END);
                    // data words and AArch64's pool keep 8 bytes.
                    let code = section.kind() == object::SectionKind::Text;
                    let width = if code && target == Target::X86_64Linux {
                        32
                    } else {
                        64
                    };
                    assert_eq!(relocation.size(), width, "{target:?} {relocation:?}");
                    assert!(relocation.addend() >= 0);
                    site_relocations += 1;
                }
            }
        }
        assert!(site_relocations > 0);
        if !debug_lines {
            assert!(
                file.section_by_name(".debug_line").is_none(),
                "default release object contains line rows"
            );
            assert!(
                file.section_by_name(".debug_info").is_none(),
                "default release object contains debug scopes"
            );
        }
        if debug_lines {
            for name in [".debug_info", ".debug_line"] {
                assert!(
                    !file
                        .section_by_name(name)
                        .unwrap()
                        .data()
                        .unwrap()
                        .is_empty(),
                    "{name}"
                );
            }
            let debug_strings = file.section_by_name(".debug_str").unwrap().data().unwrap();
            let debug_info = file.section_by_name(".debug_info").unwrap().data().unwrap();
            let has_subprogram = |name: &str| {
                let linkage = format!("{name}\0");
                [debug_strings, debug_info]
                    .iter()
                    .any(|data| data.windows(linkage.len()).any(|s| s == linkage.as_bytes()))
            };
            let source_functions = module
                .functions
                .iter()
                .enumerate()
                .filter(|(_, f)| {
                    f.body.as_ref().is_some_and(|b| {
                        b.inst_anchors.iter().any(Option::is_some)
                            || b.blocks.iter().any(|b| b.anchor.is_some())
                    })
                })
                .map(|(i, _)| symbol(&module, FuncId(i as u32)))
                .collect::<Vec<_>>();
            for function in file.symbols().filter(|s| {
                s.kind() == object::SymbolKind::Text && s.size() != 0 && !s.is_undefined()
            }) {
                let name = function.name().unwrap();
                if !source_functions
                    .iter()
                    .any(|prefix| name == prefix || name.starts_with(&format!("{prefix}.")))
                {
                    continue;
                }
                let section = file
                    .section_by_index(function.section_index().unwrap())
                    .unwrap();
                let code = &section.data().unwrap()
                    [function.address() as usize..(function.address() + function.size()) as usize];
                // MergeFunctions retains public addresses through generated jump
                // thunks. The destination's real source body owns their debug rows.
                let branch = match target {
                    Target::X86_64Linux => code.len() == 5 && code[0] == 0xe9,
                    Target::Aarch64Linux => {
                        code.len() == 4
                            && u32::from_le_bytes(code.try_into().unwrap()) & 0xfc000000
                                == 0x14000000
                    }
                };
                let thunk = branch
                    && section.relocations().any(|(offset, r)| {
                        if !(function.address()..function.address() + function.size())
                            .contains(&offset)
                        {
                            return false;
                        }
                        let object::RelocationTarget::Symbol(target) = r.target() else {
                            return false;
                        };
                        let destination = file.symbol_by_index(target).unwrap();
                        destination.name().is_ok_and(|destination| {
                            destination != name && has_subprogram(destination)
                        })
                    });
                assert!(
                    has_subprogram(name) || thunk,
                    "source function has no actual-linkage subprogram: {name}, {opt:?}, composed={composed}"
                );
            }
            if matches!(opt, Optimization::Speed) {
                let strings = file.section_by_name(".debug_str").unwrap().data().unwrap();
                let symbol = format!("{}\0", symbol(&module, names["debug_identity"]));
                assert!(
                    strings
                        .windows(symbol.len())
                        .any(|bytes| bytes == symbol.as_bytes()),
                    "optimized infallible export has no subprogram: {opt:?} composed={composed}, anchors={:?}, strings={:?}",
                    module.functions[names["debug_identity"].index()]
                        .body
                        .as_ref()
                        .unwrap()
                        .inst_anchors,
                    strings
                        .split(|b| *b == 0)
                        .filter(|s| s.windows(14).any(|w| w == b"debug_identity"))
                        .map(|s| String::from_utf8_lossy(s))
                        .collect::<Vec<_>>()
                );
            }
            if !composed {
                // The eager unit harness includes drop-trace hooks. Compile exactly its
                // IR on the lazy bridge so the parity check has identical producer inputs.
                let lazy = module.llvm_object(
                    target,
                    matches!(opt, Optimization::Speed),
                    true,
                    false,
                    false,
                );
                let left = object::File::parse(bytes.as_slice()).unwrap();
                let right = object::File::parse(lazy.as_slice()).unwrap();
                let differences = left
                    .sections()
                    .filter_map(|section| {
                        let name = section.name().unwrap();
                        let a = section.data().unwrap();
                        let b = right
                            .section_by_name(name)
                            .map(|s| s.data().unwrap())
                            .unwrap_or(&[]);
                        (a != b).then(|| {
                            (
                                name.to_owned(),
                                a.len(),
                                b.len(),
                                a.iter().zip(b).position(|(a, b)| a != b),
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                assert!(
                    bytes == lazy,
                    "eager/lazy {opt:?} object parity: {differences:?}"
                );
            }
        }
        let product = finish_native_object(
            bytes,
            hirsel_core_ids::ContentHash::of_bytes(b"executed native debug witness"),
            debug_lines,
        )
        .unwrap();
        assert_eq!(product.debug.is_some(), debug_lines);
        let reports = execute_target_object(
            &module,
            &cases,
            &bindings,
            "",
            opt,
            Some(product.bytes),
            target,
        );
        for (case, got) in cases.iter().zip(reports) {
            let expected = eval::run(&module, case.function, &case.args, u64::MAX);
            assert_eq!(got.outcome, expected.outcome);
            let expected = expected
                .sites
                .iter()
                .map(|a| a.map_or(0, |a| module.sites[a.item.index()] + u64::from(a.start)))
                .collect::<Vec<_>>();
            assert_eq!(
                got.sites, expected,
                "resolved cause sites, function {}",
                case.function.0
            );
        }
    }
    println!(
        "native failure-site observations: {} ({} with rows, {} default release without rows)",
        cases.len() * 4,
        cases.len() * 3,
        cases.len()
    );
}

#[test]
fn failure_sites_and_relative_debug_rows_match_the_evaluator() {
    site_fixture(Target::X86_64Linux);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_failure_sites_and_relative_debug_rows_match_the_evaluator() {
    site_fixture(Target::Aarch64Linux);
}

fn growth_shadow_words(target: Target) {
    let source = include_str!("../../../core/front/cases/body/grow-carrier-words.hirsel");
    let (module, names) = Ok::<_, String>(hirsel_core_program_tests::SourceProgram::lower(
        source,
        Some("main"),
    ))
    .unwrap();
    let cases: Vec<_> = [
        ("grown", 131328),
        ("refused", 130816512904),
        ("regrown", 523776),
    ]
    .into_iter()
    .map(|(name, want)| {
        let function = names[name];
        let report = eval::run(&module, function, &[], 1_000_000);
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![want]));
        assert_eq!(report.used, 0);
        Case {
            function,
            args: vec![],
            limit: 1_000_000,
            injection: None,
        }
    })
    .collect();
    for speed in [false, true] {
        let object = module.llvm_object(
            target,
            speed,
            false,
            false,
            std::env::var_os("HIRSEL_TEST_CHARGE_OBSERVATION").is_some(),
        );
        let reports = execute_target_object_checked(
            &module,
            &cases,
            include_str!("../../hirsel-core-rt/testdata/moving_realloc.c"),
            "",
            if speed {
                Optimization::Speed
            } else {
                Optimization::None
            },
            Some(object),
            target,
            true,
        );
        for (case, actual) in cases.iter().zip(reports) {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert_eq!(actual.outcome, expected.outcome);
            assert_eq!(actual.used, expected.used);
            assert_eq!(actual.admissions, expected.admissions);
            assert_eq!(actual.trace, expected.trace);
            if let Some(peak) = actual.peak_reserved {
                assert_eq!(peak, expected.peak_reserved);
            }
        }
    }
}

#[test]
fn growth_shadow_copies_back_data_and_capacity() {
    growth_shadow_words(Target::X86_64Linux);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_growth_shadow_copies_back_data_and_capacity() {
    growth_shadow_words(Target::Aarch64Linux);
}

#[test]
fn carrier_twins_match_the_evaluator_in_program_builds() {
    carrier_program_cases(Target::X86_64Linux);
    cancelled_cold_word_call(Target::X86_64Linux, false);
    cancelled_cold_word_call(Target::X86_64Linux, true);
}
#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_carrier_twins_match_the_evaluator_in_program_builds() {
    carrier_program_cases(Target::Aarch64Linux);
    cancelled_cold_word_call(Target::Aarch64Linux, false);
    cancelled_cold_word_call(Target::Aarch64Linux, true);
}

#[test]
fn step_counter_survives_inlined_mutations_and_exact_exhaustion() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/core");
    let source = r#"
fn append(mut xs: List[u64], n: u64) {
    list.push(mut xs, n)
}
fn probe(s: u64) -> u64 {
    match (bound steps s {
        let mut xs: List[u64] = list.with_capacity(8)
        for i in 0u64 .. 8u64 { append(mut xs, i) }
        list.len(xs)
    }) {
        Ok(n) => 1000 + n,
        Error(e) => e.steps
    }
}
"#;
    let (module, names) =
        hirsel_core_program_tests::SourceProgram::lower_with_core(source, None, &root);
    let cases: Vec<_> = (0..=48)
        .map(|s| Case {
            function: names["probe"],
            args: vec![s],
            limit: 1_000_000,
            injection: None,
        })
        .collect();
    assert_eq!(
        eval::run(&module, names["probe"], &[48], 1_000_000).outcome,
        eval::Outcome::Returned(vec![1008])
    );
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for opt in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(opt, Optimization::Speed)).unwrap();
            let got = execute_object(&module, &cases, "", "", opt, Some(object));
            for (case, actual) in cases.iter().zip(got) {
                assert_eq!(
                    actual.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "facts={facts}, s={}",
                    case.args[0]
                );
            }
        }
    }
}

#[test]
fn fallible_step_regions_refund_partial_work_and_check_strided_remainders() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/core");
    let source = r#"
type Op { apply: fn(u64) -> u64 }
fn nextword(x: u64) -> u64 { int.wrapping_add(x, 1u64) }
fn halfword(x: u64) -> u64 { x / 2u64 }
fn applyall(op: Op, n: u64) -> u64 {
    let mut sum = 0u64
    for i in 0u64 .. n { sum += (op.apply)(i) }
    sum
}
fn boxed(s: u64, n: u64, choice: u64) -> u64 {
    match (bound steps s {
        if choice == 0 { applyall(Op(apply: nextword), n) }
        else { applyall(Op(apply: halfword), n) }
    }) { Ok(word) => 10000000000u64 + word, Error(e) => e.steps }
}
fn triangle(s: u64, z: u64) -> u64 {
    match (bound steps s {
        let mut word = 0u64
        for i in 0u64 .. 5u64 {
            for j in i * z .. 5u64 {
                word = (word << 3u64) ^ i ^ (j << 1u64)
            }
        }
        word
    }) { Ok(n) => 1000 + n, Error(e) => e.steps }
}
fn purebits(s: u64, n: u64) -> u64 {
    match (bound steps s {
        let mut crc: u32 = 0xffffffff
        for i in 0u64 .. n {
            let byte: u32 = int.truncate(i)
            crc = crc ^ byte
            for _ in 0u32 .. 8u32 {
                crc = if crc & 1u32 != 0u32 {
                    (crc >> 1u32) ^ 0xedb88320u32
                } else { crc >> 1u32 }
            }
        }
        crc
    }) { Ok(word) => 10000000000u64 + int.widen(word), Error(e) => e.steps }
}
fn probe(s: u64) -> u64 {
    match (bound steps s {
        let first = result.unwrap(isolate(fn() -> u64 {
            let mut xs: List[u64] = list.with_capacity(8)
            for i in 0u64 .. 8u64 {
                list.push(mut xs, i)
                let _ = 100u64 / (5u64 - i)
            }
            list.len(xs)
        }))
        let n = match take first { Ok(n) => n, Error(take cs) => 19u64 }
        for _ in 0u64 .. 7u64 {}
        n
    }) { Ok(n) => 1000 + n, Error(e) => e.steps }
}
fn straight(s: u64, at: u64) -> u64 {
    match (bound steps s {
        let ran = result.unwrap(isolate(scoped fn() -> u64 {
            let xs: Inline[u64, 2] = [4u64, 7u64]
            let a: u64 = int.widen(1u8)
            let b = xs[at]
            let c: u64 = int.widen(2u8)
            a + b + c
        }))
        for _ in 0u64 .. 3u64 {}
        match take ran { Ok(n) => n, Error(take cs) => 77u64 }
    }) { Ok(n) => 1000 + n, Error(e) => e.steps }
}
fn boundary_overflow(s: u64) -> u64 {
    match (bound steps s {
        for _ in 0u64 .. 65535u64 {}
        let mut k = 18446744073709551612u64
        while k <= 18446744073709551615u64 { k += 2u64 }
        k
    }) { Ok(n) => n, Error(e) => e.steps }
}
fn resetting(s: u64) -> u64 {
    match (bound steps s {
        let mut k = 1u64
        let initial = k
        while k <= 10u64 { k = initial + 1u64 }
        k
    }) { Ok(n) => 1000 + n, Error(e) => e.steps }
}
fn strided(s: u64, stride: u64) -> u64 {
    match (bound steps s {
        let mut xs: List[u64] = list.with_capacity(16)
        let mut k = 1u64
        while k <= 10u64 {
            list.push(mut xs, k)
            k += stride
        }
        list.len(xs)
    }) { Ok(n) => 1000 + n, Error(e) => e.steps }
}
"#;
    let (module, names) =
        hirsel_core_program_tests::SourceProgram::lower_with_core(source, None, &root);
    let mut cases = Vec::new();
    // One batch call, then callback/latch plus the first callback's wrapping
    // call: 1+3n for nextword, 1+2n for halfword. Both code pointers flow
    // through a record into the same loop, including a slice boundary.
    for n in [0u64, 4, 21846] {
        for choice in [0, 1] {
            let total = 1 + (if choice == 0 { 3 } else { 2 }) * n;
            let sum = (0..n)
                .map(|i| if choice == 0 { i + 1 } else { i / 2 })
                .sum::<u64>();
            for s in [0, total - 1, total, total + 1] {
                let want = if s < total { s } else { 10_000_000_000 + sum };
                assert_eq!(
                    eval::run(&module, names["boxed"], &[s, n, choice], 1_000_000).outcome,
                    eval::Outcome::Returned(vec![want]),
                    "boxed s={s}, n={n}, choice={choice}",
                );
                cases.push(Case {
                    function: names["boxed"],
                    args: vec![s, n, choice],
                    limit: 1_000_000,
                    injection: None,
                });
            }
        }
    }
    // One scalar conversion, eight inner latches and one outer latch per
    // trip. Exercise exact bounds and both sides of the 65536-Step slice.
    for n in [0u64, 1, 8, 6554] {
        let mut crc = u32::MAX;
        for i in 0..n {
            crc ^= i as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb88320
                } else {
                    crc >> 1
                };
            }
        }
        let total = n * 10;
        for s in [
            0,
            total.saturating_sub(1),
            total,
            total + 1,
            65535,
            65536,
            65537,
        ] {
            let want = if s < total {
                s
            } else {
                10_000_000_000 + u64::from(crc)
            };
            assert_eq!(
                eval::run(&module, names["purebits"], &[s, n], 1_000_000).outcome,
                eval::Outcome::Returned(vec![want]),
                "purebits s={s}, n={n}",
            );
            cases.push(Case {
                function: names["purebits"],
                args: vec![s, n],
                limit: 1_000_000,
                injection: None,
            });
        }
    }
    for s in 0..=64 {
        cases.push(Case {
            function: names["probe"],
            args: vec![s],
            limit: 1_000_000,
            injection: None,
        });
        // A parent reserves a child's most trips from any start; z = 0
        // runs every child at that bound, so an undercharge shows at s.
        for z in [0, 1] {
            cases.push(Case {
                function: names["triangle"],
                args: vec![s, z],
                limit: 1_000_000,
                injection: None,
            });
        }
        cases.push(Case {
            function: names["resetting"],
            args: vec![s],
            limit: 1_000_000,
            injection: None,
        });
        for at in [0, 1, 2, u64::MAX] {
            cases.push(Case {
                function: names["straight"],
                args: vec![s, at],
                limit: 1_000_000,
                injection: None,
            });
        }
        for stride in [0, 1, 2, 3, u64::MAX] {
            cases.push(Case {
                function: names["strided"],
                args: vec![s, stride],
                limit: 1_000_000,
                injection: None,
            });
        }
    }
    for s in 65536..=65540 {
        cases.push(Case {
            function: names["boundary_overflow"],
            args: vec![s],
            limit: 1_000_000,
            injection: None,
        });
    }
    assert_eq!(
        eval::run(&module, names["probe"], &[64], 1_000_000).outcome,
        eval::Outcome::Returned(vec![1019])
    );
    // isolate call + user entry + two allocation calls, five full trips
    // at three Steps, two calls in the failing trip, outer unwrap, seven latches.
    // 1 + 1 + 2 + 5*3 + 2 + 1 + 7 = 29. Failure is caught before the tail.
    for (s, want) in [(28, 28), (29, 1019)] {
        assert_eq!(
            eval::run(&module, names["probe"], &[s], 1_000_000).outcome,
            eval::Outcome::Returned(vec![want])
        );
    }
    // Two setup calls, three Steps per checked-stride trip, then list.len.
    for (s, stride, want) in [(33, 1, 1010), (18, 2, 1005), (15, 3, 1004)] {
        assert_eq!(
            eval::run(&module, names["strided"], &[s, stride], 1_000_000).outcome,
            eval::Outcome::Returned(vec![want])
        );
    }
    // Two isolate entries, one widening before Bounds, outer unwrap, three
    // latches = seven. The successful second widening makes eight.
    for (s, at, want) in [(6, 2, 6), (7, 2, 1077), (8, 0, 1007)] {
        assert_eq!(
            eval::run(&module, names["straight"], &[s, at], 1_000_000).outcome,
            eval::Outcome::Returned(vec![want])
        );
    }
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            let reports = execute_object(&module, &cases, "", "", optimization, Some(object));
            for (case, report) in cases.iter().zip(reports) {
                assert_eq!(
                    report.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "facts={facts}, function={:?}, args={:?}",
                    case.function,
                    case.args
                );
            }
        }
    }
    let modules = module
        .functions
        .iter()
        .enumerate()
        .filter_map(|(root, f)| {
            f.body.as_ref()?;
            let mut unit = module.clone();
            for (i, f) in unit.functions.iter_mut().enumerate() {
                if i != root {
                    f.body = None;
                } else if let Some(body) = &mut f.body {
                    body.inst_anchors.fill(None);
                    for block in &mut body.blocks {
                        block.anchor = None;
                    }
                }
            }
            Some(unit)
        })
        .collect::<Vec<_>>();
    let facts = modules
        .iter()
        .map(hirsel_core_ir::roles::Facts::new)
        .collect::<Vec<_>>();
    let units = modules
        .iter()
        .zip(&facts)
        .map(|(module, facts)| hirsel_llvm_lazy::Unit::Core {
            module,
            facts,
            observe_charge: false,
            debug_lines: false,
        })
        .collect::<Vec<_>>();
    let exports = [
        "boxed",
        "purebits",
        "triangle",
        "probe",
        "strided",
        "resetting",
        "boundary_overflow",
        "straight",
    ]
    .map(|name| symbol(&module, names[name]));
    let object = hirsel_llvm_lazy::emit_program(&units, &exports).unwrap();
    let reports = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
    for (case, report) in cases.iter().zip(reports) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            "split units, function={:?}, args={:?}",
            case.function,
            case.args
        );
    }
}

#[test]
fn counted_reference_regions_keep_exact_bounds_and_mutation_prefixes() {
    let source = r#"
type Cell { value: u64 }
fn capacity_only(take r: Result[(), (Capacity, Arc[Cell])]) -> Result[(), Capacity] {
    match r { Ok(()) => Ok(()), Error((c, _)) => Error(c) }
}
fn share_loop(mut xs: List[Arc[Cell]], value: Arc[Cell], n: u64, at: u64) -> Result[(), Capacity] {
    for i in 0u64 .. n {
        let held = arc.share(value)
        list.push(mut xs, take held)
        if i == at { return Error(Capacity(requested: 1, available: 0)) }
    }
    Ok(())
}
fn probe(s: u64, n: u64, at: u64) -> u64 {
    let ran = bound steps s {
        let mut xs: List[Arc[Cell]] = list.new()
        let value = arc.new(Cell(value: 9u64))
        let caught = share_loop(mut xs, value, n, at)
        for _ in 0u64 .. 3u64 {}
        let prefix = list.len(xs)
        match take caught { Ok(()) => 1000000000u64 + prefix, Error(c) => 3000000000u64 + prefix }
    }
    match ran {
        Ok(answer) => answer,
        Error(e) => 2000000000u64 + e.steps * 100000u64,
    }
}
"#;
    // Arc event recording holds 4096 entries. Rc covers the slice boundary;
    // Arc exercises the same classification with its full event trace retained.
    for atomic in [false, true] {
        let source = if atomic {
            source.to_owned()
        } else {
            source.replace("Arc[", "Rc[").replace("arc.", "rc.")
        };
        let (module, names) = hirsel_core_program_tests::SourceProgram::lower(&source, None);
        let mut cases = Vec::new();
        for n in [0u64, 4, if atomic { 1024 } else { 16384 }] {
            let bounds = if n < 8 {
                (0..=4 * n + 13).collect::<Vec<_>>()
            } else {
                vec![
                    0,
                    1,
                    2,
                    3,
                    4,
                    5,
                    13,
                    14,
                    41,
                    42,
                    65535,
                    65536,
                    65537,
                    65538,
                    65544,
                    65547,
                    65548,
                    8 + 4 * n - 1,
                    8 + 4 * n,
                    8 + 4 * n + 1,
                ]
            };
            let positions = [0, 7, n]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
            for at in positions {
                for &s in &bounds {
                    // Three setup calls and the helper call cost four. Each
                    // trip shares, pushes, unwraps Capacity and latches. An early
                    // error skips its latch; the tail is three latches and len.
                    let need = if at < n { 11 + 4 * at } else { 8 + 4 * n };
                    let want = if s < need {
                        2000000000u64 + s * 100000
                    } else if at < n {
                        3000000000u64 + at + 1
                    } else {
                        1000000000u64 + n
                    };
                    assert_eq!(
                        eval::run(&module, names["probe"], &[s, n, at], 1_000_000).outcome,
                        eval::Outcome::Returned(vec![want]),
                        "hand count s={s}, n={n}, at={at}",
                    );
                    cases.push(Case {
                        function: names["probe"],
                        args: vec![s, n, at],
                        limit: 1_000_000,
                        injection: None,
                    });
                }
            }
        }
        // A failed growth must retain exactly its already-pushed prefix too.
        for s in [0, 5, 64, 1024] {
            cases.push(Case {
                function: names["probe"],
                args: vec![s, 128, 128],
                limit: 512,
                injection: None,
            });
        }
        for facts in [false, true] {
            let ir = emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts)
                .unwrap();
            verify_ir(&ir);
            for optimization in [Optimization::None, Optimization::Speed] {
                let object =
                    hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                        .unwrap();
                let got = execute_object(&module, &cases, "", "", optimization, Some(object));
                for (case, report) in cases.iter().zip(got) {
                    assert_eq!(
                        report.outcome,
                        eval::run(&module, case.function, &case.args, case.limit).outcome,
                        "facts={facts}, args={:?}, limit={}",
                        case.args,
                        case.limit,
                    );
                }
            }
        }
        let object = module.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
        let got = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
        for (case, report) in cases.iter().zip(got) {
            assert_eq!(
                report.outcome,
                eval::run(&module, case.function, &case.args, case.limit).outcome,
                "split units, args={:?}, limit={}",
                case.args,
                case.limit
            );
        }
    }
}

#[test]
fn hot_loop_helper_mutations_and_early_returns_keep_exact_steps() {
    let source = r#"
fn lane(mut xs: Inline[u64, 8], i: u64, at: u64) -> Result[(), Capacity] {
    let a: u64 = int.widen(1u8)
    xs[i % 8] += a
    let b: u64 = int.widen(2u8)
    xs[i % 8] += b
    if i == at { return Error(Capacity(requested: 1, available: 0)) }
    let c: u64 = int.widen(3u8)
    xs[i % 8] += c
    Ok(())
}

fn probe(s: u64, n: u64, at: u64) -> u64 {
    let ran = bound steps s {
        let mut xs: Inline[u64, 8] = [0; 8]
        for i in 0u64 .. n {
            if let Error(_) = lane(mut xs, i, at) { break }
        }
        for _ in 0u64 .. 3u64 {}
        xs[0] + xs[1] * 10 + xs[2] * 100 + xs[3] * 1000 + xs[4] * 10000 + xs[5] * 100000 + xs[6] * 1000000 + xs[7] * 10000000
    }
    match ran { Ok(answer) => 1000000000u64 + answer, Error(e) => 2000000000u64 + e.steps }
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let mut cases = Vec::new();
    for n in [1u64, 8, 32] {
        for at in [0, 7, n] {
            let early = at < n;
            let completed = if early { at } else { n };
            let mut words = [0u64; 8];
            for i in 0..completed {
                words[(i % 8) as usize] += 6;
            }
            if early {
                words[(at % 8) as usize] += 3;
            }
            let checksum = words
                .iter()
                .enumerate()
                .map(|(i, v)| v * 10u64.pow(i as u32))
                .sum::<u64>();
            // Call + three widen calls + latch = five per complete trip.
            // The early trip has its call and two widens, then three tail latches.
            let need = if early { 5 * at + 6 } else { 5 * n + 3 };
            for s in (0..=48).chain([157, 158, 159, 160, 161, 162, 163, 164]) {
                let want = if s < need {
                    2000000000u64 + s
                } else {
                    1000000000u64 + checksum
                };
                assert_eq!(
                    eval::run(&module, names["probe"], &[s, n, at], 1_000_000).outcome,
                    eval::Outcome::Returned(vec![want]),
                    "hand count s={s}, n={n}, at={at}",
                );
                cases.push(Case {
                    function: names["probe"],
                    args: vec![s, n, at],
                    limit: 1_000_000,
                    injection: None,
                });
            }
        }
    }
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            let got = execute_object(&module, &cases, "", "", optimization, Some(object));
            for (case, report) in cases.iter().zip(got) {
                assert_eq!(
                    report.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "facts={facts}, args={:?}",
                    case.args
                );
            }
        }
    }
    let object = module.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
    let got = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
    for (case, report) in cases.iter().zip(got) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            "split units, args={:?}",
            case.args
        );
    }
}

#[test]
fn element_stores_keep_a_shrinking_sibling_bound_and_failure_site() {
    // `pop` through `xs[j]` shrinks the same carrier that `xs[k]` reads when
    // `j == k`. The element store into `xs[1 - k]` leaves every carrier alone.
    // Reading `xs[k][i]` must fail Bounds at its own site once `i` reaches the
    // shrunk length, never pass a length loaded before the pop.
    let source = r#"
fn walk(mut xs: List[List[u64]], j: u64, k: u64, n: u64) -> u64 {
    let mut total = 0u64
    for i in 0u64 .. n {
        xs[1u64 - k][i] = i
        let _ = list.pop(mut xs[j])
        total += xs[k][i]
    }
    total
}
fn probe(n: u64, extra: u64, k: u64) -> u64 {
    let mut xs: List[List[u64]] = list.new()
    for _ in 0u64 .. 2u64 {
        let mut b: List[u64] = list.new()
        list.fill(mut b, n + extra, 3u64)
        list.push(mut xs, take b)
    }
    walk(mut xs, 0u64, k, n) + list.len(xs[0]) * 100u64
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let cases =
        [(4, 5, 0), (4, 1, 0), (4, 5, 1), (4, 1, 1), (64, 2, 0)].map(|(n, extra, k)| Case {
            function: names["probe"],
            args: vec![n, extra, k],
            limit: 1_000_000,
            injection: None,
        });
    // Four reads of 3 and five elements left; the pop before i = 2 leaves two.
    assert_eq!(
        eval::run(&module, names["probe"], &[4, 5, 0], 1_000_000).outcome,
        eval::Outcome::Returned(vec![512])
    );
    assert_eq!(
        eval::run(&module, names["probe"], &[4, 1, 0], 1_000_000).outcome,
        eval::Outcome::Failed(vec!["Bounds".into()])
    );
    assert_eq!(
        eval::run(&module, names["probe"], &[4, 5, 1], 1_000_000).outcome,
        eval::Outcome::Returned(vec![512])
    );
    let check = |reports: Vec<NativeReport>| {
        for (case, report) in cases.iter().zip(reports) {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert_eq!(report.outcome, expected.outcome, "args={:?}", case.args);
            let expected = expected
                .sites
                .iter()
                .map(|a| a.map_or(0, |a| module.sites[a.item.index()] + u64::from(a.start)))
                .collect::<Vec<_>>();
            assert_eq!(
                report.sites, expected,
                "failure sites, args={:?}",
                case.args
            );
        }
    };
    let ir = emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, true).unwrap();
    verify_ir(&ir);
    let object = hirsel_llvm_lazy::compile_ir(&ir, true).unwrap();
    check(execute_object(
        &module,
        &cases,
        "",
        "",
        Optimization::Speed,
        Some(object),
    ));
    let object = module.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
    check(execute_object(
        &module,
        &cases,
        "",
        "",
        Optimization::Speed,
        Some(object),
    ));
}

#[test]
fn exposed_loop_helpers_keep_failure_sites() {
    let source = r#"
fn helper(mut xs: Inline[u64, 8], i: u64, d: u64) -> u64 {
    xs[i % 8] = int.widen(1u8)
    let quotient = 12u64 / d
    let value: Option[u64] = if d == 1 { None } else { Some(quotient) }
    let answer = option.unwrap(take value)
    xs[i % 8] = answer
    answer
}
fn probe(n: u64, d: u64) -> u64 {
    let mut xs: Inline[u64, 8] = [0; 8]
    let mut answer = 0u64
    for i in 0u64 .. n { answer += helper(mut xs, i, d) }
    answer + xs[0]
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let cases = [1, 8]
        .into_iter()
        .flat_map(|n| {
            [0, 1, 2].map(|d| Case {
                function: names["probe"],
                args: vec![n, d],
                limit: 1_000_000,
                injection: None,
            })
        })
        .collect::<Vec<_>>();
    let check = |reports: Vec<NativeReport>| {
        for (case, report) in cases.iter().zip(reports) {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert_eq!(report.outcome, expected.outcome, "args={:?}", case.args);
            let expected = expected
                .sites
                .iter()
                .map(|a| a.map_or(0, |a| module.sites[a.item.index()] + u64::from(a.start)))
                .collect::<Vec<_>>();
            assert_eq!(
                report.sites, expected,
                "failure sites, args={:?}",
                case.args
            );
        }
    };
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            check(execute_object(
                &module,
                &cases,
                "",
                "",
                optimization,
                Some(object),
            ));
        }
    }
    let object = module.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
    check(execute_object(
        &module,
        &cases,
        "",
        "",
        Optimization::Speed,
        Some(object),
    ));
}

#[test]
fn exposed_scanner_loops_keep_exact_steps_and_early_prefixes() {
    let source = r#"
fn scan(mut xs: Inline[u64, 8], n: u64, at: u64) -> Result[(), Capacity] {
    for i in 0u64 .. n {
        xs[i % 8] += int.widen(1u8)
        if i == at { return Error(Capacity(requested: 1, available: 0)) }
    }
    Ok(())
}
fn probe(s: u64, n: u64, at: u64) -> u64 {
    let ran = bound steps s {
        let mut xs: Inline[u64, 8] = [0; 8]
        for _ in 0u64 .. 2u64 {
            if let Error(_) = scan(mut xs, n, at) { break }
        }
        for _ in 0u64 .. 3u64 {}
        xs[0] + xs[1] * 10 + xs[2] * 100 + xs[3] * 1000 + xs[4] * 10000 + xs[5] * 100000 + xs[6] * 1000000 + xs[7] * 10000000
    }
    match ran { Ok(answer) => 1000000000u64 + answer, Error(e) => 2000000000u64 + e.steps }
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let mut cases = Vec::new();
    for n in [1u64, 8, 16384] {
        for at in [0, n - 1, n] {
            let early = at < n;
            // Each complete scan trip has one conversion call and one latch.
            // Two full scans each add their call and the outer latch; tail=3.
            // An early scan has its call, at full trips, its final conversion,
            // no outer latch, then the three tail latches.
            let need = if early { 2 * at + 5 } else { 4 * n + 7 };
            let mut words = [0u64; 8];
            for _ in 0..if early { 1 } else { 2 } {
                for i in 0..if early { at + 1 } else { n } {
                    words[(i % 8) as usize] += 1;
                }
            }
            let checksum = words
                .iter()
                .enumerate()
                .map(|(i, v)| v * 10u64.pow(i as u32))
                .sum::<u64>();
            for s in [0, need - 1, need, need + 1, 65535, 65536, 65537] {
                let want = if s < need {
                    2000000000u64 + s
                } else {
                    1000000000u64 + checksum
                };
                assert_eq!(
                    eval::run(&module, names["probe"], &[s, n, at], 1_000_000).outcome,
                    eval::Outcome::Returned(vec![want]),
                    "hand count s={s}, n={n}, at={at}"
                );
                cases.push(Case {
                    function: names["probe"],
                    args: vec![s, n, at],
                    limit: 1_000_000,
                    injection: None,
                });
            }
        }
    }
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            let reports = execute_object(&module, &cases, "", "", optimization, Some(object));
            for (case, report) in cases.iter().zip(reports) {
                assert_eq!(
                    report.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "facts={facts}, args={:?}",
                    case.args
                );
            }
        }
    }
    let object = module.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
    let reports = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
    for (case, report) in cases.iter().zip(reports) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            "split units, args={:?}",
            case.args
        );
    }
}

#[test]
fn branched_regions_refund_the_unexecuted_effect_suffix() {
    let source = r#"
fn fork(mut xs: Inline[u64, 5], mode: u64) -> Result[(), Capacity] {
    xs[0] += int.widen(1u8)
    if mode == 0 {
        xs[1] += int.widen(2u8)
    } else {
        xs[2] += int.widen(3u8)
        if mode == 1 { return Error(Capacity(requested: 1, available: 0)) }
        xs[3] += int.widen(4u8)
    }
    xs[4] += int.widen(5u8)
    Ok(())
}
fn probe(s: u64, mode: u64, repeats: u64) -> u64 {
    match (bound steps s {
        let mut xs: Inline[u64, 5] = [0; 5]
        let mut early = 0u64
        for _ in 0u64 .. repeats {
            match fork(mut xs, mode) { Ok(()) => (), Error(_) => { early += 1 } }
        }
        for _ in 0u64 .. 3u64 {}
        let checksum = xs[0] + xs[1] * 10 + xs[2] * 100 + xs[3] * 1000 + xs[4] * 10000
        1000000000u64 + early * 1000000u64 + checksum
    }) { Ok(v) => v, Error(e) => 2000000000u64 + e.steps }
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let mut cases = Vec::new();
    for (mode, calls, checksum) in [(0, 3, 50021), (1, 2, 301), (2, 4, 54301)] {
        for repeats in [0, 1, 7, 16384] {
            // One fork call, its branch's calls, then the outer latch.
            // Three empty tail latches finish the bound.
            let need = repeats * (calls + 2) + 3;
            for s in [0, 1, 2, 3, need - 1, need, need + 1, 65535, 65536, 65537] {
                let want = if s < need {
                    2000000000 + s
                } else {
                    1000000000 + repeats * (checksum + if mode == 1 { 1000000 } else { 0 })
                };
                assert_eq!(
                    eval::run(&module, names["probe"], &[s, mode, repeats], 1_000_000).outcome,
                    eval::Outcome::Returned(vec![want]),
                    "s={s}, mode={mode}, repeats={repeats}"
                );
                cases.push(Case {
                    function: names["probe"],
                    args: vec![s, mode, repeats],
                    limit: 1_000_000,
                    injection: None,
                });
            }
        }
    }
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            let reports = execute_object(&module, &cases, "", "", optimization, Some(object));
            for (case, report) in cases.iter().zip(reports) {
                let expected = eval::run(&module, case.function, &case.args, case.limit);
                assert_eq!(
                    report.outcome, expected.outcome,
                    "facts={facts}, args={:?}",
                    case.args
                );
                let sites = expected
                    .sites
                    .iter()
                    .map(|a| a.map_or(0, |a| module.sites[a.item.index()] + u64::from(a.start)))
                    .collect::<Vec<_>>();
                assert_eq!(report.sites, sites, "failure site, args={:?}", case.args);
            }
        }
    }
    let object = module.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
    let reports = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
    for (case, report) in cases.iter().zip(reports) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            "split units, args={:?}",
            case.args
        );
    }
}

#[test]
fn data_dependent_cycles_refund_exact_step_prefixes() {
    let source = r#"
fn scan(mut xs: Inline[u64, 8], n: u64, at: u64) -> Result[(), Capacity] {
    let mut i = 0u64
    loop {
        let one: u64 = int.widen(1u8)
        if i == n { break }
        xs[i % 8] += one
        if i == at { return Error(Capacity(requested: 1, available: 0)) }
        i += 1
    }
    Ok(())
}
fn probe(s: u64, n: u64, at: u64) -> u64 {
    let ran = bound steps s {
        let mut xs: Inline[u64, 8] = [0; 8]
        let scanned = scan(mut xs, n, at)
        for _ in 0u64 .. 3u64 {}
        let checksum = xs[0] + xs[1] * 10 + xs[2] * 100 + xs[3] * 1000 + xs[4] * 10000 + xs[5] * 100000 + xs[6] * 1000000 + xs[7] * 10000000
        match scanned { Ok(()) => 1000000000u64 + checksum, Error(_) => 3000000000u64 + checksum }
    }
    match ran { Ok(answer) => answer, Error(e) => 2000000000u64 + e.steps }
}
fn failing(s: u64, n: u64) -> u64 {
    match (bound steps s {
        let ran = result.unwrap(isolate(scoped fn() -> u64 {
            let mut i = 0u64
            loop {
                let one: u64 = int.widen(1u8)
                if i == n { let _ = 1u64 / (i - n) }
                i += one
            }
        }))
        for _ in 0u64 .. 3u64 {}
        match take ran { Ok(v) => v, Error(take cs) => 77u64 }
    }) { Ok(v) => 1000u64 + v, Error(e) => e.steps }
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let mut cases = Vec::new();
    for n in [0u64, 1, 8, 32768] {
        for at in [0, n.saturating_sub(1), n] {
            let early = at < n;
            // One source call, two Steps per completed trip, one final
            // conversion before exit, then three tail latches.
            let need = 2 * if early { at } else { n } + 5;
            let mut words = [0u64; 8];
            for i in 0..if early { at + 1 } else { n } {
                words[(i % 8) as usize] += 1;
            }
            let checksum = words
                .iter()
                .enumerate()
                .map(|(i, v)| v * 10u64.pow(i as u32))
                .sum::<u64>();
            for s in [0, need - 1, need, need + 1, 65535, 65536, 65537] {
                let want = if s < need {
                    2000000000u64 + s
                } else {
                    (if early { 3000000000u64 } else { 1000000000u64 }) + checksum
                };
                assert_eq!(
                    eval::run(&module, names["probe"], &[s, n, at], 1_000_000).outcome,
                    eval::Outcome::Returned(vec![want]),
                    "hand count s={s}, n={n}, at={at}"
                );
                cases.push(Case {
                    function: names["probe"],
                    args: vec![s, n, at],
                    limit: 1_000_000,
                    injection: None,
                });
            }
        }
    }
    for n in [0, 7, 300] {
        for s in [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 15, 16, 17, 600, 601, 602, 603, 604, 605, 606, 607, 608,
            609, 610, 611, 612,
        ] {
            cases.push(Case {
                function: names["failing"],
                args: vec![s, n],
                limit: 1_000_000,
                injection: None,
            });
        }
    }
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            let reports = execute_object(&module, &cases, "", "", optimization, Some(object));
            for (case, report) in cases.iter().zip(reports) {
                let expected = eval::run(&module, case.function, &case.args, case.limit);
                assert_eq!(
                    report.outcome, expected.outcome,
                    "facts={facts}, f={:?}, args={:?}",
                    case.function, case.args
                );
                let sites = expected
                    .sites
                    .iter()
                    .map(|a| a.map_or(0, |a| module.sites[a.item.index()] + u64::from(a.start)))
                    .collect::<Vec<_>>();
                assert_eq!(report.sites, sites, "failure site, args={:?}", case.args);
            }
        }
    }
    let object = module.llvm_program(
        Target::X86_64Linux,
        false,
        false,
        &[names["probe"], names["failing"]],
    );
    let reports = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
    for (case, report) in cases.iter().zip(reports) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            "split units, args={:?}",
            case.args
        );
    }
}

#[test]
fn counted_breaks_refund_the_partial_trip_and_unused_iterations() {
    let source = r#"
fn probe(s: u64, n: u64, at: u64) -> u64 {
    match (bound steps s {
        let mut xs: Inline[u64, 8] = [0; 8]
        for i in 0u64 .. n {
            let one: u64 = int.widen(1u8)
            if i == at { break }
            xs[i % 8] += one
        }
        for _ in 0u64 .. 3u64 {}
        xs[0] + xs[1] * 10 + xs[2] * 100 + xs[3] * 1000 + xs[4] * 10000 + xs[5] * 100000 + xs[6] * 1000000 + xs[7] * 10000000
    }) { Ok(v) => 1000000000000u64 + v, Error(e) => 2000000000000u64 + e.steps }
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let mut cases = Vec::new();
    for n in [0u64, 1, 8, 32768, 1u64 << 63, u64::MAX] {
        let exits = if n > 32768 {
            [0, 1, 7]
        } else {
            [0, n.saturating_sub(1), n]
        };
        for at in exits {
            let trips = n.min(at);
            // One widening and latch per completed trip; an early break
            // pays its widening but no latch, then three tail latches.
            let need = 2 * trips + u64::from(at < n) + 3;
            let checksum = (0..8u32)
                .map(|i| (trips / 8 + u64::from(u64::from(i) < trips % 8)) * 10u64.pow(i))
                .sum::<u64>();
            for s in [
                0,
                1,
                2,
                3,
                4,
                5,
                6,
                7,
                need - 1,
                need,
                need + 1,
                65535,
                65536,
                65537,
            ] {
                let want = if s < need {
                    2000000000000 + s
                } else {
                    1000000000000 + checksum
                };
                assert_eq!(
                    eval::run(&module, names["probe"], &[s, n, at], 1_000_000).outcome,
                    eval::Outcome::Returned(vec![want]),
                    "s={s}, n={n}, at={at}"
                );
                cases.push(Case {
                    function: names["probe"],
                    args: vec![s, n, at],
                    limit: 1_000_000,
                    injection: None,
                });
            }
        }
    }
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            let reports = execute_object(&module, &cases, "", "", optimization, Some(object));
            for (case, report) in cases.iter().zip(reports) {
                let expected = eval::run(&module, case.function, &case.args, case.limit);
                assert_eq!(
                    report.outcome, expected.outcome,
                    "facts={facts}, args={:?}",
                    case.args
                );
                let sites = expected
                    .sites
                    .iter()
                    .map(|a| a.map_or(0, |a| module.sites[a.item.index()] + u64::from(a.start)))
                    .collect::<Vec<_>>();
                assert_eq!(report.sites, sites, "failure site, args={:?}", case.args);
            }
        }
    }
    let object = module.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
    let reports = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
    for (case, report) in cases.iter().zip(reports) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            "split units, args={:?}",
            case.args
        );
    }
}

#[test]
fn cheap_loop_traces_refund_before_expensive_effects_and_resume_once() {
    let source = r#"
fn scan(mut xs: Inline[u64, 3], n: u64, at: u64, early: bool) -> Result[(), Capacity] {
    for i in 0u64 .. n {
        xs[0] += int.widen(1u8)
        if i == at {
            xs[1] += int.widen(3u8)
            if early { return Error(Capacity(requested: 1, available: 0)) }
            xs[2] += int.widen(4u8)
        }
    }
    Ok(())
}
fn probe(s: u64, n: u64, at: u64, mode: u64) -> u64 {
    let ran = bound steps s {
        let mut xs: Inline[u64, 3] = [0; 3]
        let stopped = match scan(mut xs, n, at, mode == 1) { Ok(()) => 0u64, Error(_) => 1u64 }
        for _ in 0u64 .. 3u64 {}
        xs[0] + 100000u64 * xs[1] + 1000000u64 * xs[2] + 10000000u64 * stopped
    }
    match ran { Ok(v) => 1000000000u64 + v, Error(e) => 2000000000u64 + e.steps }
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let mut cases = Vec::new();
    for n in [0u64, 1, 8, 32768] {
        for at in [0, 3, n] {
            for mode in [0, 1] {
                let extra = at < n;
                let early = extra && mode == 1;
                // One scan call; a normal trip widens once and latches once.
                // The expensive trip widens two more times. An early trip
                // widens twice and returns before its latch. Tail = three.
                let need = if early {
                    2 * at + 6
                } else {
                    2 * n + 4 + 2 * u64::from(extra)
                };
                let checksum = if early {
                    at + 1 + 300000 + 10000000
                } else {
                    n + if extra { 4300000 } else { 0 }
                };
                for s in [
                    0,
                    1,
                    2,
                    3,
                    4,
                    5,
                    need - 1,
                    need,
                    need + 1,
                    65535,
                    65536,
                    65537,
                ] {
                    let want = if s < need {
                        2000000000u64 + s
                    } else {
                        1000000000u64 + checksum
                    };
                    assert_eq!(
                        eval::run(&module, names["probe"], &[s, n, at, mode], 1_000_000).outcome,
                        eval::Outcome::Returned(vec![want]),
                        "hand count s={s}, n={n}, at={at}, mode={mode}"
                    );
                    cases.push(Case {
                        function: names["probe"],
                        args: vec![s, n, at, mode],
                        limit: 1_000_000,
                        injection: None,
                    });
                }
            }
        }
    }
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            for (case, report) in cases.iter().zip(execute_object(
                &module,
                &cases,
                "",
                "",
                optimization,
                Some(object),
            )) {
                assert_eq!(
                    report.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "facts={facts}, args={:?}",
                    case.args
                );
            }
        }
    }
    let object = module.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
    for (case, report) in cases.iter().zip(execute_object(
        &module,
        &cases,
        "",
        "",
        Optimization::Speed,
        Some(object),
    )) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            "split units, args={:?}",
            case.args
        );
    }
}

#[test]
fn shifted_loop_headers_keep_exact_tails_and_checked_header_overflow() {
    let run = r#"
fn probe(s: u64, n: u64, at: u64, base: u64) -> u64 {
    let ran = bound steps s {
        let mut xs: Inline[u64, 8] = [0; 8]
        let mut k = base
        while k + 8u64 <= n {
            xs[((k - base) / 8u64) % 8u64] += int.widen(1u8)
            if k == at { break }
            k += 8u64
        }
        for _ in 0u64 .. 3u64 {}
        xs[0] + xs[1] * 10 + xs[2] * 100 + xs[3] * 1000 + xs[4] * 10000 + xs[5] * 100000 + xs[6] * 1000000 + xs[7] * 10000000
    }
    match ran { Ok(v) => 1000000000u64 + v, Error(e) => 2000000000u64 + e.steps }
}
"#;
    let source = format!(
        "{run}\n{}",
        run.replace("fn probe(", "fn distance(")
            .replace("k + 8u64 <= n", "n - k >= 8u64")
    );
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(&source, None);
    let mut cases = Vec::new();
    for name in ["probe", "distance"] {
        for n in [0u64, 7, 8, 9, 16, 25, 262151, (1u64 << 63) + 7, u64::MAX] {
            let trips = n / 8;
            let positions = if n > 1u64 << 62 {
                vec![0, 8]
            } else {
                vec![0, 8, trips * 8]
            };
            for at in positions {
                let early = at / 8 < trips;
                let completed = if early { at / 8 } else { trips };
                // A full trip widens once and latches once. A break widens
                // once, skips its latch, then pays the three tail latches.
                let need = 2 * completed + u64::from(early) + 3;
                let mut words = [0u64; 8];
                for i in 0..completed + u64::from(early) {
                    words[(i % 8) as usize] += 1;
                }
                let checksum = words
                    .iter()
                    .enumerate()
                    .map(|(i, v)| v * 10u64.pow(i as u32))
                    .sum::<u64>();
                for s in [0, 1, 2, 3, need - 1, need, need + 1, 65535, 65536, 65537] {
                    let want = if s < need {
                        2000000000u64 + s
                    } else {
                        1000000000u64 + checksum
                    };
                    let args = vec![s, n, at, 0];
                    assert_eq!(
                        eval::run(&module, names[name], &args, 1_000_000).outcome,
                        eval::Outcome::Returned(vec![want]),
                        "hand count {name} args={args:?}"
                    );
                    cases.push(Case {
                        function: names[name],
                        args,
                        limit: 1_000_000,
                        injection: None,
                    });
                }
            }
        }
        // Two full trips end at MAX. The addition header then overflows
        // before another Step; the distance header exits and pays its tail.
        for s in 0..=9 {
            let args = vec![s, u64::MAX, u64::MAX, u64::MAX - 16];
            let want = if name == "probe" && s >= 4 {
                eval::Outcome::Failed(vec!["Overflow".into()])
            } else if s < if name == "probe" { 4 } else { 7 } {
                eval::Outcome::Returned(vec![2000000000u64 + s])
            } else {
                eval::Outcome::Returned(vec![1000000011])
            };
            assert_eq!(
                eval::run(&module, names[name], &args, 1_000_000).outcome,
                want,
                "checked terminal header {name} args={args:?}"
            );
            cases.push(Case {
                function: names[name],
                args,
                limit: 1_000_000,
                injection: None,
            });
        }
    }
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            for (case, report) in cases.iter().zip(execute_object(
                &module,
                &cases,
                "",
                "",
                optimization,
                Some(object),
            )) {
                assert_eq!(
                    report.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "facts={facts}, args={:?}",
                    case.args
                );
            }
        }
    }
    let object = module.llvm_program(
        Target::X86_64Linux,
        false,
        false,
        &[names["probe"], names["distance"]],
    );
    for (case, report) in cases.iter().zip(execute_object(
        &module,
        &cases,
        "",
        "",
        Optimization::Speed,
        Some(object),
    )) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            "split units, args={:?}",
            case.args
        );
    }
}

use hirsel_core_program_tests::wire_socket;

#[test]
fn wire_socket_loopback_carries_addresses_deadlines_and_bytes() {
    let source = wire_socket::SOURCE;
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let binding = wire_socket::BINDINGS;
    let reports = execute_with_configuration(
        &program,
        &[Case {
            function: names["wire"],
            args: vec![1, 2],
            limit: 1 << 20,
            injection: None,
        }],
        binding,
        ",.provider=wire_config()->provider,.capabilities=wire_config()->capabilities,.capabilities_len=wire_config()->capabilities_len",
    );
    assert_eq!(reports[0].outcome, eval::Outcome::Returned(vec![42]));
    assert_eq!(reports[0].used, 0);
}

#[test]
fn direct_and_indirect_recursive_calls_keep_exact_steps_and_failure_sites() {
    let source = r#"
fn recur(n: u64, d: u64) -> u64 {
    let one: u64 = int.widen(1u8)
    if n == 0 { return one }
    one + recur(n - 1, d) + 12u64 / d
}
fn alt(n: u64, d: u64) -> u64 { recur(n, d) + int.widen(2u8) }
fn invoke(f: fn(u64, u64) -> u64, n: u64, d: u64) -> u64 { f(n, d) }
fn probe(s: u64, n: u64, d: u64, mode: u64) -> u64 {
    let ran = bound steps s {
        let f = if mode == 0 { recur } else { alt }
        let a = invoke(f, n, d)
        let b = recur(n, d)
        for _ in 0u64 .. 3u64 {}
        a + b
    }
    match ran { Ok(v) => 1000000000u64 + v, Error(e) => 2000000000u64 + e.steps }
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let mut cases = Vec::new();
    for n in [0u64, 1, 4, 8] {
        for mode in [0, 1] {
            // recur widens at every level and calls recursively at each nonleaf.
            // invoke and its callback cost two. alt calls recur and widens once.
            // A second recur call and three latches finish the scope.
            let need = 8 + 4 * n + 2 * mode;
            for d in [0, 1] {
                for s in 0..=need + 1 {
                    if d != 0 || n == 0 {
                        let want = if s < need {
                            2000000000 + s
                        } else {
                            1000000002 + 26 * n + 2 * mode
                        };
                        assert_eq!(
                            eval::run(&module, names["probe"], &[s, n, d, mode], 1_000_000).outcome,
                            eval::Outcome::Returned(vec![want]),
                            "s={s}, n={n}, mode={mode}"
                        );
                    }
                    cases.push(Case {
                        function: names["probe"],
                        args: vec![s, n, d, mode],
                        limit: 1_000_000,
                        injection: None,
                    });
                }
            }
        }
    }
    let check = |reports: Vec<NativeReport>| {
        for (case, report) in cases.iter().zip(reports) {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert_eq!(report.outcome, expected.outcome, "args={:?}", case.args);
            let sites = expected
                .sites
                .iter()
                .map(|a| a.map_or(0, |a| module.sites[a.item.index()] + u64::from(a.start)))
                .collect::<Vec<_>>();
            assert_eq!(report.sites, sites, "args={:?}", case.args);
        }
    };
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            check(execute_object(
                &module,
                &cases,
                "",
                "",
                optimization,
                Some(object),
            ));
        }
    }
    let object = module.llvm_program(Target::X86_64Linux, false, false, &[names["probe"]]);
    check(execute_object(
        &module,
        &cases,
        "",
        "",
        Optimization::Speed,
        Some(object),
    ));
}

#[test]
fn captured_cursor_and_writer_state_keep_exact_prefixes() {
    let source = r#"
fn twice(f: scoped fn(u64) -> u64, d: u64) -> u64 { f(d) + f(d) }
fn bump(mut n: u8) { n += 1 }
fn widths(n: u64) -> u64 {
    let mut cursor = 0u8
    let mut flag = false
    let scale = 1.5f64
    let end: u8 = int.narrow(n)
    let input = "xy"
    twice(scoped fn(d: u64) -> u64 {
        while cursor < end {
            bump(mut cursor)
            flag = !flag
        }
        if float.to_bits64(scale) == 4609434218613702656 && flag { string.byte_len(input) + d + int.widen(cursor) }
        else { d + int.widen(cursor) }
    }, 3)
}
fn probe(s: u64, n: u64, d: u64) -> u64 {
    let ran = bound steps s {
        let mut cursor = 0u64
        let mut state = 0u64
        let input: Inline[u8, 8] = [1; 8]
        let answer = twice(scoped fn(divisor: u64) -> u64 {
            while cursor < n {
                state += int.widen(input[cursor % 8])
                cursor += 1
                state += 12u64 / divisor
            }
            state
        }, d)
        for _ in 0u64 .. 3u64 {}
        answer + state + cursor
    }
    match ran { Ok(v) => 1000000000u64 + v, Error(e) => 2000000000u64 + e.steps }
}
fn scan(s: u64, n: u64, stop: u64) -> u64 {
    let ran = bound steps s {
        let mut cursor = 0u64
        twice(scoped fn(d: u64) -> u64 {
            while cursor < n {
                if cursor == stop { break }
                cursor += 1
            }
            cursor + d
        }, 3) + 100 * cursor
    }
    match ran { Ok(v) => 1000000000u64 + v, Error(e) => 2000000000u64 + e.steps }
}
fn scan_prefixed(s: u64, n: u64, stop: u64) -> u64 {
    let ran = bound steps s {
        let mut cursor = 0u64
        let input: Inline[u8, 8] = [0; 8]
        twice(scoped fn(d: u64) -> u64 {
            while cursor < n {
                let byte: u64 = int.widen(input[cursor % 8])
                if byte + cursor == stop { break }
                cursor += 1
            }
            cursor + d
        }, 3) + 100 * cursor
    }
    match ran { Ok(v) => 1000000000u64 + v, Error(e) => 2000000000u64 + e.steps }
}
"#;
    let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let mut cases = Vec::new();
    for (n, want) in [(0, 6), (1, 12), (2, 10), (7, 24)] {
        assert_eq!(
            eval::run(&module, names["widths"], &[n], 1_000_000).outcome,
            eval::Outcome::Returned(vec![want]),
            "widths({n})"
        );
        cases.push(Case {
            function: names["widths"],
            args: vec![n],
            limit: 1_000_000,
            injection: None,
        });
    }
    for n in [0u64, 1, 7, 32768] {
        let need = 6 + 2 * n; // twice, its two callbacks, n widenings/latches, three tail latches.
        for d in [0, 1] {
            for s in [0, 1, 2, need - 1, need, need + 1, 65535, 65536, 65537] {
                if d != 0 || n == 0 {
                    let want = if s < need {
                        2000000000 + s
                    } else {
                        1000000000 + 40 * n
                    };
                    assert_eq!(
                        eval::run(&module, names["probe"], &[s, n, d], 1_000_000).outcome,
                        eval::Outcome::Returned(vec![want]),
                        "s={s}, n={n}, d={d}"
                    );
                }
                cases.push(Case {
                    function: names["probe"],
                    args: vec![s, n, d],
                    limit: 1_000_000,
                    injection: None,
                });
            }
        }
    }
    for (n, stop) in [(0, 0), (1, 0), (8, 3), (8, 20), (65540, 65538)] {
        let cursor = n.min(stop);
        let need = 3 + cursor; // twice, two callback calls and one latch per increment.
        for s in [0, 1, 2, need - 1, need, need + 1, 65535, 65536, 65537] {
            let want = if s < need {
                2000000000 + s
            } else {
                1000000006 + 102 * cursor
            };
            assert_eq!(
                eval::run(&module, names["scan"], &[s, n, stop], 1_000_000).outcome,
                eval::Outcome::Returned(vec![want]),
                "scan({s}, {n}, {stop})"
            );
            cases.push(Case {
                function: names["scan"],
                args: vec![s, n, stop],
                limit: 1_000_000,
                injection: None,
            });
        }
    }
    for (n, stop) in [
        (0, 0),
        (1, 0),
        (8, 3),
        (8, 20),
        (65540, 65538),
        (65540, 100000),
    ] {
        let cursor = n.min(stop);
        // Three calls, a widening and latch per increment, plus a widening
        // on each callback's break test when the byte stops the scan.
        let need = 3 + 2 * cursor + if cursor < n { 2 } else { 0 };
        for s in [
            0,
            1,
            2,
            need - 1,
            need,
            need + 1,
            65535,
            65536,
            65537,
            131071,
            131072,
            131073,
        ] {
            let want = if s < need {
                2000000000 + s
            } else {
                1000000006 + 102 * cursor
            };
            assert_eq!(
                eval::run(&module, names["scan_prefixed"], &[s, n, stop], 1_000_000).outcome,
                eval::Outcome::Returned(vec![want]),
                "scan_prefixed({s}, {n}, {stop})"
            );
            cases.push(Case {
                function: names["scan_prefixed"],
                args: vec![s, n, stop],
                limit: 1_000_000,
                injection: None,
            });
        }
    }
    for facts in [false, true] {
        let ir =
            emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts).unwrap();
        verify_ir(&ir);
        for optimization in [Optimization::None, Optimization::Speed] {
            let object =
                hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                    .unwrap();
            let reports = execute_object(&module, &cases, "", "", optimization, Some(object));
            for (case, report) in cases.iter().zip(reports) {
                assert_eq!(
                    report.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "facts={facts}, args={:?}",
                    case.args
                );
            }
        }
    }
    let object = module.llvm_program(
        Target::X86_64Linux,
        false,
        false,
        &[
            names["probe"],
            names["widths"],
            names["scan"],
            names["scan_prefixed"],
        ],
    );
    let reports = execute_object(&module, &cases, "", "", Optimization::Speed, Some(object));
    for (case, report) in cases.iter().zip(reports) {
        assert_eq!(
            report.outcome,
            eval::run(&module, case.function, &case.args, case.limit).outcome,
            "split units, args={:?}",
            case.args
        );
    }
}

#[test]
fn writer_bookkeeping_preserves_payloads_overflow_poisoning_and_step_prefixes() {
    let indexed = include_str!("../../../core/front/cases/stream-indexed-span.hirsel");
    let destination = include_str!("../../../core/front/cases/body/dest-put.hirsel");
    let reservations =
        include_str!("../../../core/front/cases/body/packed-writer-reservation.hirsel");
    let looped = r#"
fn go(n: u64, cap: u64) -> u64 {
    let mut cursor = 0u64
    region r {
        let made = build_stream_in(r, bytes cap, fn(take w: Writer[Text, in r]) -> Result[Child[Text], BuildError] {
            while cursor < n {
                writer.scalar(mut w, 'A')?
                cursor += int.widen(1u8)
            }
            writer.close(take w)
        })
        match made {
            Ok(take b) => text.len(finish(take b)) + 100 * cursor,
            Error(Overflow) => 9000000u64 + 100 * cursor,
            Error(_) => 9999999u64,
        }
    }
}
fn zero_after_poison(cap: u64) -> u64 {
    let mut seen = 0u64
    region r {
        let made = build_stream_in(r, bytes cap, fn(take w: Writer[Text, in r]) -> Result[Child[Text], BuildError] {
            let _ = writer.push(mut w, "ab")
            seen = match writer.push(mut w, "") { Ok(()) => 1u64, Error(Overflow) => 2u64, Error(_) => 3u64 }
            match writer.close(take w) {
                Ok(child) => { seen += 10; Ok(child) },
                Error(e) => { seen += 20; Error(e) },
            }
        })
        match made {
            Ok(take b) => 100u64 * seen + text.len(finish(take b)),
            Error(Overflow) => 9000u64 + seen,
            Error(_) => 9999u64,
        }
    }
}
fn prior_completed(cap: u64) -> u64 {
    let mut seen = 0u64
    region r {
        let made = build_stream_in(r, bytes cap, fn(take w: Writer[IndexedSpan[Text], in r]) -> Result[Child[IndexedSpan[Text]], BuildError] {
            writer.append(mut w, fn(take t: Writer[Text, in r]) -> Result[Child[Text], BuildError] {
                writer.push(mut t, "ok")?
                writer.close(take t)
            })?
            let old: Option[View[Text]] = writer.last_completed(w)
            let old_len = match old { Some(t) => text.len(t), None => 0u64 }
            let _ = writer.append(mut w, fn(take t: Writer[Text, in r]) -> Result[Child[Text], BuildError] {
                writer.push(mut t, "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz")?
                writer.close(take t)
            })
            let saved: Option[View[Text]] = writer.completed(w, 0)
            let saved_len = match saved { Some(t) => text.len(t), None => 0u64 }
            seen = 100u64 * old_len + saved_len
            writer.close(take w)
        })
        match made {
            Ok(take b) => seq.len(finish(take b)),
            Error(Overflow) => 9000u64 + seen,
            Error(_) => 9999u64,
        }
    }
}
fn metered(s: u64, n: u64, cap: u64) -> u64 {
    match (bound steps s { go(n, cap) }) {
        Ok(v) => v, Error(e) => 1000000000u64 + e.steps,
    }
}
"#;
    let indexed_cases = [("go", vec![35], 3203302), ("go", vec![34], 99)];
    let destination_cases = [
        ("go", vec![27], 7),
        ("go", vec![20], 0),
        ("go", vec![8], 98),
        ("refused", vec![], 64),
    ];
    let reservation_cases = [
        ("packet", vec![0], 91),
        ("packet", vec![1], 91),
        ("packet", vec![2], 91),
        ("packet", vec![3], 91),
        ("packet", vec![5], 91),
        ("packet", vec![6], 31),
        ("packet", vec![7], 31),
        ("empty", vec![2], 91),
        ("empty", vec![3], 1),
        ("poisoned", vec![9], 91),
        ("poisoned", vec![10], 2),
    ];
    let mut loop_cases = Vec::new();
    for n in [0u64, 1, 7, 32768] {
        for cap in [0, 7, n + 7, n + 8] {
            let expected = if cap < 8 {
                9000000
            } else if cap < n + 8 {
                9000000 + 100 * (cap - 8)
            } else {
                101 * n
            };
            loop_cases.push(("go", vec![n, cap], expected));
        }
    }
    loop_cases.push(("prior_completed", vec![64], 9202));
    loop_cases.push(("zero_after_poison", vec![9], 9022));
    loop_cases.push(("zero_after_poison", vec![10], 1102));
    for (source, hand) in [
        (indexed, indexed_cases.as_slice()),
        (destination, destination_cases.as_slice()),
        (reservations, reservation_cases.as_slice()),
        (looped, loop_cases.as_slice()),
    ] {
        let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
        let mut cases = Vec::new();
        for (name, args, want) in hand {
            assert_eq!(
                eval::run(&module, names[*name], args, 1_000_000).outcome,
                eval::Outcome::Returned(vec![*want]),
                "{name}{args:?}"
            );
            cases.push(Case {
                function: names[*name],
                args: args.clone(),
                limit: 1_000_000,
                injection: None,
            });
        }
        if names.contains_key("metered") {
            for s in [0, 1, 4, 32, 65535, 65536, 65537, 1000000] {
                for (n, cap) in [(7, 14), (7, 15), (32768, 32776)] {
                    cases.push(Case {
                        function: names["metered"],
                        args: vec![s, n, cap],
                        limit: 1_000_000,
                        injection: None,
                    });
                }
            }
        }
        let check = |reports: Vec<NativeReport>| {
            for (case, report) in cases.iter().zip(reports) {
                assert_eq!(
                    report.outcome,
                    eval::run(&module, case.function, &case.args, case.limit).outcome,
                    "function={:?}, args={:?}",
                    case.function,
                    case.args
                );
            }
        };
        for facts in [false, true] {
            let ir = emit_ir_for_test_with_facts(&module, Target::X86_64Linux, false, false, facts)
                .unwrap();
            verify_ir(&ir);
            for optimization in [Optimization::None, Optimization::Speed] {
                let object =
                    hirsel_llvm_lazy::compile_ir(&ir, matches!(optimization, Optimization::Speed))
                        .unwrap();
                check(execute_object(
                    &module,
                    &cases,
                    "",
                    "",
                    optimization,
                    Some(object),
                ));
            }
        }
        let exports = hand
            .iter()
            .map(|(name, _, _)| names[*name])
            .chain(names.get("metered").copied())
            .collect::<Vec<_>>();
        let object = module.llvm_program(Target::X86_64Linux, false, false, &exports);
        check(execute_object(
            &module,
            &cases,
            "",
            "",
            Optimization::Speed,
            Some(object),
        ));
    }
}

#[test]
fn static_bytes_preserve_budget_law() {
    let source = include_str!("../../../core/front/cases/rules/MEM-28-static-bytes.hirsel");
    let (program, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
    let cases = [
        ("literal_refusal", vec![0], 1615),
        ("literal_refusal", vec![1], 1615),
        ("refund_after_handover", vec![0], 890),
        ("refund_after_handover", vec![1], 890),
        ("empty_is_free", vec![], 0),
        ("cloned_and_counted", vec![], 18),
        ("inline_capacity", vec![], 311),
    ];
    let runs = cases.each_ref().map(|(name, args, _)| Case {
        function: names[*name],
        args: args.clone(),
        limit: 4096,
        injection: None,
    });
    for (report, (_, _, expected)) in execute(&program, &runs).iter().zip(cases) {
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![expected]));
        assert_eq!(report.used, 0);
    }
}

#[test]
fn simd_llvm_matches_evaluator() {
    let target = if std::env::var_os("HIRSEL_TEST_SIMD_AARCH64").is_some() {
        Target::Aarch64Linux
    } else {
        Target::X86_64Linux
    };
    let observing = std::env::var_os("HIRSEL_TEST_CHARGE_OBSERVATION").is_some();
    for source in [
        include_str!("../../../core/front/fixtures/simd-operations.hirsel"),
        include_str!("../../../core/front/fixtures/simd-differential.hirsel"),
    ] {
        let (module, names) = hirsel_core_program_tests::SourceProgram::lower(source, None);
        let cases: Vec<_> = source
            .lines()
            .filter_map(|line| line.strip_prefix("// expect: run "))
            .map(|run| {
                let (call, _) = run.split_once(" = ").unwrap();
                let (name, args) = call.strip_suffix(')').unwrap().split_once('(').unwrap();
                let function = names[name];
                let signature =
                    &module.signatures[module.functions[function.index()].signature.index()];
                let args = args
                    .split(',')
                    .filter(|arg| !arg.is_empty())
                    .zip(&signature.params)
                    .map(|(arg, param)| {
                        let Param::Value(ty) = param else {
                            panic!("scalar run")
                        };
                        eval::scalar(*ty, arg.trim())
                    })
                    .collect();
                Case {
                    function,
                    args,
                    limit: u64::MAX,
                    injection: None,
                }
            })
            .collect();
        for optimization in [Optimization::None, Optimization::Speed] {
            let reports =
                execute_target_object(&module, &cases, "", "", optimization, None, target);
            check_native_reports(&module, &module.sites, &cases, reports, true);
        }

        let units: Vec<_> = module.modules.iter().collect();
        let facts = hirsel_core_ir::roles::Facts::program(&units, &[]);
        let units: Vec<_> = units
            .into_iter()
            .zip(&facts)
            .map(|(module, facts)| hirsel_llvm_lazy::Unit::Core {
                module,
                facts,
                observe_charge: observing,
                debug_lines: false,
            })
            .collect();
        let exports: std::collections::BTreeSet<_> = cases
            .iter()
            .map(|case| symbol(&module, case.function))
            .collect();
        let exports: Vec<_> = exports.into_iter().collect();
        let release = hirsel_llvm_lazy::emit_program_for_target(&units, &exports, target).unwrap();
        let caps: Vec<_> = if target == Target::X86_64Linux {
            hirsel_core_layout::isa::X86_64_LEVELS
                .iter()
                .map(|level| level.number)
                .collect()
        } else {
            vec![0]
        };
        for cap in caps {
            let bindings = if cap == 0 {
                String::new()
            } else {
                format!(
                    r#"
#include <stdatomic.h>
extern int setenv(const char *, const char *, int);
extern _Atomic uint8_t hirsel_core_isa_level;
extern void hirsel_core_init_isa(void);
__attribute__((constructor)) static void simd_test_level(void) {{
    assert(setenv("HIRSEL_ISA_CAP", "{cap}", 1) == 0);
    hirsel_core_init_isa();
    assert(atomic_load(&hirsel_core_isa_level) == {cap});
}}
"#
                )
            };
            let reports = execute_target_object(
                &module,
                &cases,
                &bindings,
                "",
                Optimization::Speed,
                Some(release.clone()),
                target,
            );
            // The production builder has no allocation-injection test hooks.
            // Compare its byte balance and observable bounds/capacity results.
            for (case, report) in cases.iter().zip(&reports) {
                let expected = eval::run(&module, case.function, &case.args, case.limit);
                assert_eq!(
                    report.used, expected.used,
                    "native byte balance at cap {cap}"
                );
            }
            check_native_reports(&module, &module.sites, &cases, reports, false);
        }
    }
}
