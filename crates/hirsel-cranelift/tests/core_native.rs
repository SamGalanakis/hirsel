//! Execute objects through a separately compiled C caller. This catches both
//! scalar semantics and the register packing a same-backend caller could mask.
use hirsel_core_ir::{self as c, *};
use hirsel_cranelift::core::*;
hirsel_guard::install!();
use hirsel_core_ir::eval::{self, Outcome};
use std::{
    fmt::Write as _,
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

#[path = "support/front.rs"]
mod front;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hirsel-core-clif-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn ctype(t: Ty) -> &'static str {
    match t {
        Ty::Ptr => "void*",
        Ty::Scalar(Scalar::Bool | Scalar::U8) => "uint8_t",
        Ty::Scalar(Scalar::I8) => "int8_t",
        Ty::Scalar(Scalar::U16) => "uint16_t",
        Ty::Scalar(Scalar::I16) => "int16_t",
        Ty::Scalar(Scalar::Char | Scalar::U32) => "uint32_t",
        Ty::Scalar(Scalar::I32) => "int32_t",
        Ty::Scalar(Scalar::I64) => "int64_t",
        Ty::Scalar(Scalar::U64) => "uint64_t",
        Ty::Scalar(Scalar::F32) => "float",
        Ty::Scalar(Scalar::F64) => "double",
    }
}
fn execute(module: &c::Module, runs: &[(FuncId, Vec<u64>)], optimize: bool) {
    execute_limit(module, runs, optimize, 1_000_000);
}
fn execute_limit(module: &c::Module, runs: &[(FuncId, Vec<u64>)], optimize: bool, limit: u64) {
    execute_injected(module, runs, optimize, limit, None);
}
fn execute_injected(
    module: &c::Module,
    runs: &[(FuncId, Vec<u64>)],
    optimize: bool,
    limit: u64,
    injection: Option<(usize, bool)>,
) {
    execute_reports(module, runs, optimize, limit, injection, None);
}
fn execute_reports(
    module: &c::Module,
    runs: &[(FuncId, Vec<u64>)],
    optimize: bool,
    limit: u64,
    injection: Option<(usize, bool)>,
    reports: Option<&[eval::Report]>,
) {
    let scratch = Scratch::new();
    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let artifact = emit_object_for_test(
        module,
        &layouts,
        &Options {
            target: "x86_64-unknown-linux-gnu",
            optimize,
        },
    )
    .unwrap();
    fs::write(scratch.0.join("input.o"), artifact.bytes).unwrap();
    let mut source = String::from("#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n");
    source.push_str(include_str!(
        "../../hirsel-core-rt/include/hirsel_core_rt.h"
    ));
    source.push_str("\nextern HirselCoreContext *hirsel_core_test_context_new(uint64_t);\nextern void hirsel_core_test_inject(HirselCoreContext *,size_t,bool);\nextern uint64_t hirsel_core_test_available(const HirselCoreContext *);\nextern size_t hirsel_core_test_attempts(const HirselCoreContext *);\nstatic uint64_t trace[1000000]; static size_t traces;\nvoid hirsel_core_test_drop_trace(uint64_t n) { if(traces >= 1000000) __builtin_abort(); trace[traces++]=n; }\n");
    for (i, s) in module.signatures.iter().enumerate() {
        if !s.results.is_empty() {
            write!(source, "struct r{i} {{ ").unwrap();
            for (j, &t) in s.results.iter().enumerate() {
                write!(source, "{} v{j};", ctype(t)).unwrap();
            }
            source.push_str("uint8_t status; };\n");
        }
    }
    for f in &module.functions {
        let s = &module.signatures[f.signature.index()];
        let result = match s.results.as_slice() {
            [] => "uint8_t".into(),
            _ => format!("struct r{}", f.signature.0),
        };
        write!(
            source,
            "extern {result} {}(void*",
            symbol(&module.instances[f.instance.index()])
        )
        .unwrap();
        if s.dest.is_some() {
            source.push_str(", void*");
        }
        for p in &s.params {
            let c = match p {
                Param::Value(t) => ctype(*t),
                Param::Address(_) => "void*",
            };
            write!(source, ", {c}").unwrap();
        }
        source.push_str(");\n");
    }
    source.push_str("#include <sys/resource.h>\nint main(void) {\n");
    if reports.is_some() {
        source.push_str(
            "struct rlimit stack = {65536,65536}; if(setrlimit(RLIMIT_STACK,&stack)) return 7;\n",
        );
    }
    let mut expected = Vec::new();
    for (i, (f, args)) in runs.iter().enumerate() {
        let function = &module.functions[f.index()];
        let s = &module.signatures[function.signature.index()];
        let computed;
        let report = if let Some(reports) = reports {
            &reports[i]
        } else {
            computed = eval::run_injected(module, *f, args, limit, injection);
            &computed
        };
        match &report.outcome {
            Outcome::Returned(values) => {
                expected.push(0);
                expected.push(values.len() as u64);
                expected.extend(values);
            }
            Outcome::Failed(causes) => {
                expected.push(1);
                expected.push(causes.len() as u64);
                for cause in causes {
                    expected.push(match cause.as_str() {
                        "Overflow" => 0,
                        "Bounds" => 1,
                        "Division" => 2,
                        "Unwrap" => 3,
                        "Fail" => 4,
                        "Cancelled" => 5,
                        "Capacity" => 6,
                        _ => panic!("unknown cause {cause}"),
                    });
                }
            }
        }
        expected.push(report.used);
        source.push_str("{\n");
        writeln!(
            source,
            "HirselCoreContext *ctx = hirsel_core_test_context_new(UINT64_C({limit})); traces=0;"
        )
        .unwrap();
        if let Some((n, later)) = injection {
            writeln!(
                source,
                "hirsel_core_test_inject(ctx,{n},{});",
                u8::from(later)
            )
            .unwrap();
        }
        for (j, (p, arg)) in s.params.iter().zip(args).enumerate() {
            let Param::Value(t) = p else {
                panic!("scalar test")
            };
            write!(
                source,
                "uint64_t bits{j} = UINT64_C({arg}); {} a{j}; ",
                ctype(*t)
            )
            .unwrap();
            if t.is_float() {
                writeln!(source, "memcpy(&a{j}, &bits{j}, sizeof(a{j}));").unwrap();
            } else {
                writeln!(source, "a{j} = ({})bits{j};", ctype(*t)).unwrap();
            }
        }
        let result = if s.results.is_empty() {
            "uint64_t status = ".into()
        } else {
            format!("struct r{} value = ", function.signature.0)
        };
        write!(
            source,
            "{result}{}(ctx",
            symbol(&module.instances[function.instance.index()])
        )
        .unwrap();
        for j in 0..args.len() {
            write!(source, ", a{j}").unwrap();
        }
        source.push_str(");\n");
        if !s.results.is_empty() {
            source.push_str("uint64_t status = value.status;\n");
        }
        source.push_str("fwrite(&status,8,1,stdout); HirselCoreCauseCell *causes = 0; if (status) { causes = hirsel_core_causes_take(ctx); uint64_t count = 0; for(const HirselCoreCauseCell *c = causes; c; c = hirsel_core_cause_next(c)) count++; fwrite(&count,8,1,stdout); for(const HirselCoreCauseCell *c = causes; c; c = hirsel_core_cause_next(c)) { uint64_t kind = hirsel_core_cause_value(c)->kind; fwrite(&kind,8,1,stdout); } } else {\n");
        writeln!(
            source,
            "uint64_t count = {}; fwrite(&count,8,1,stdout);",
            s.results.len()
        )
        .unwrap();
        for (j, t) in s.results.iter().enumerate() {
            let expr = format!("value.v{j}");
            writeln!(source, "uint64_t out{i}_{j} = 0;").unwrap();
            // Values are raw low bits, even when the language type is signed.
            if t.is_float() {
                writeln!(source, "memcpy(&out{i}_{j}, &{expr}, sizeof({expr}));").unwrap();
            } else {
                writeln!(
                    source,
                    "out{i}_{j} = (uint64_t)({expr}) & UINT64_C({});",
                    match match t {
                        Ty::Ptr => 64,
                        Ty::Scalar(s) => s.size() as u32 * 8,
                    } {
                        64 => u64::MAX,
                        n => (1u64 << n) - 1,
                    }
                )
                .unwrap();
            }
            writeln!(source, "fwrite(&out{i}_{j}, 8, 1, stdout);").unwrap();
        }
        source.push_str("}\n");
        expected.push(report.trace.len() as u64);
        expected.extend(&report.trace);
        expected.push(report.admissions as u64);
        writeln!(source,"uint64_t used = UINT64_C({limit}) - hirsel_core_test_available(ctx); fwrite(&used,8,1,stdout); uint64_t nt=traces; fwrite(&nt,8,1,stdout); fwrite(trace,8,traces,stdout); uint64_t na=hirsel_core_test_attempts(ctx); fwrite(&na,8,1,stdout); hirsel_core_causes_drop(causes); if(status && hirsel_core_test_available(ctx) != UINT64_C({limit})) return 9; hirsel_core_context_free(ctx);").unwrap();
        source.push_str("}\n");
    }
    source.push_str("return 0; }\n");
    let actual = link_run(&scratch, &source);
    assert_eq!(actual, expected, "optimize={optimize}");
}

fn link_run(scratch: &Scratch, source: &str) -> Vec<u64> {
    fs::write(scratch.0.join("caller.c"), source).unwrap();
    let archive = PathBuf::from(
        std::env::var_os("HIRSEL_CORE_RUNTIME_ARCHIVE")
            .expect("the core runtime archive is explicit test data"),
    );
    let archive = if archive.is_absolute() {
        archive
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(archive)
    };
    let archive = fs::canonicalize(archive).expect("the declared core runtime archive exists");
    let link = Command::new("cc")
        .current_dir(&scratch.0)
        .args(["caller.c", "input.o"])
        .arg(archive)
        .args([
            "-lm",
            "-ldl",
            "-lpthread",
            "-lrt",
            "-lutil",
            "-no-pie",
            "-o",
            "run",
        ])
        .output()
        .unwrap();
    assert!(
        link.status.success(),
        "{}",
        String::from_utf8_lossy(&link.stderr)
    );
    let run = Command::new(scratch.0.join("run")).output().unwrap();
    assert!(run.status.success(), "native execution: {:?}", run.status);
    let actual: Vec<_> = run
        .stdout
        .chunks_exact(8)
        .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
        .collect();
    actual
}

#[test]
fn compare_convert_sample_speed_matches_evaluator_through_c_abi() {
    let text = include_str!("../../hirsel-core-ir/samples/compare-convert.core");
    let module = text::parse(text).unwrap();
    let runs = sample_runs(&module, text);
    assert_eq!(runs.len(), 22);
    execute(&module, &runs, true);
}

fn sample_runs(m: &c::Module, text: &str) -> Vec<(FuncId, Vec<u64>)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim().strip_prefix("; run: f")?;
            let (id, rest) = line.split_once('(').unwrap();
            let f = FuncId(id.parse().unwrap());
            let args = rest.split_once(')').unwrap().0;
            let s = &m.signatures[m.functions[f.index()].signature.index()];
            let args = if args.trim().is_empty() {
                vec![]
            } else {
                s.params
                    .iter()
                    .zip(args.split(','))
                    .map(|(p, a)| {
                        let Param::Value(t) = p else { panic!() };
                        eval::scalar(*t, a.trim())
                    })
                    .collect()
            };
            Some((f, args))
        })
        .collect()
}

#[test]
fn slots_loops_calls_and_c_struct_returns_match_evaluator() {
    let source = r#"core v1
sig g0 = (u64) -> (u64, u64, u64)
sig g1 = (u64) -> (u32, f32)
sig g2 = (u32, f32) -> (u32, f32)
sig g3 = (f32, f32) -> (f32, f32)
inst n0 = test@0/abi/loop [] []
inst n1 = test@0/abi/call [] []
inst n2 = test@0/abi/pair [] []
inst n3 = test@0/abi/floats [] []
fn f0 = n0 g0 {
  slot s0 = u64
  addr a0 = s0 : u64
b0(v0: u64):
  live s0
  address a0
  v1: u64 = const 0
  v2: u64 = const 1
  store a0, v1
  jump b1(v1)
b1(v3: u64):
  v4: bool = lt v3, v0
  branch v4, b2, b3
b2:
  v5: u64 = read a0
  v6: u64 = wrapping_add v5, v3
  store a0, v6
  v7: u64 = wrapping_add v3, v2
  jump b1(v7)
b3:
  v8: u64 = read a0
  dead s0
  return v0, v8, v3
}
fn f1 = n1 g1 {
b0(v0: u64):
  v1: u64, v2: u64, v3: u64 = call f0 g0 (v0)
  v4: u32 = truncate v2
  v5: f32 = int_to_float v3
  v6: ptr = fnref f2
  v7: u32, v8: f32 = call v6 g2 (v4, v5)
  return v7, v8
}
fn f2 = n2 g2 {
b0(v0: u32, v1: f32):
  return v0, v1
}
fn f3 = n3 g3 {
b0(v0: f32, v1: f32):
  return v1, v0
}
"#;
    let m = text::parse(source).unwrap();
    wf::module(&m).unwrap();
    let runs = vec![
        (FuncId(0), vec![0]),
        (FuncId(0), vec![100]),
        (FuncId(1), vec![100]),
        (
            FuncId(3),
            vec![1f32.to_bits() as u64, (-2f32).to_bits() as u64],
        ),
    ];
    execute(&m, &runs, false);
    execute(&m, &runs, true);
}

#[test]
fn integer_modes_and_float_conversion_boundaries_match_evaluator() {
    let mut declarations = String::from("core v0\n");
    let mut bodies = String::new();
    let mut runs = Vec::new();
    let mut id = 0;
    for kind in ["i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64"] {
        let scalar = Scalar::from_name(kind).unwrap();
        let t = Ty::Scalar(scalar);
        let (signed, width) = t.int().unwrap();
        let mask = if width == 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        let min = 1u64 << (width - 1);
        let max = if signed { min - 1 } else { mask };
        for mode in ["wrapping", "saturating", "checked"] {
            for op in ["add", "sub", "mul", "div"] {
                if op == "div" && mode != "checked" {
                    continue;
                }
                let checked = mode == "checked";
                let results = if checked {
                    format!("{kind}, bool")
                } else {
                    kind.into()
                };
                writeln!(declarations,"sig g{id} = ({kind}, {kind}) -> ({results})\ninst n{id} = test@0/numeric/f{id} [] []").unwrap();
                let flags = if checked { ", v3: bool" } else { "" };
                let returned = if checked { ", v3" } else { "" };
                writeln!(bodies,"fn f{id} = n{id} g{id} {{\nb0(v0: {kind}, v1: {kind}):\n  v2: {kind}{flags} = {mode}_{op} v0, v1\n  return v2{returned}\n}}").unwrap();
                for args in [[0, 0], [max, 1], [max, 2], [min, mask], [mask, 2], [min, 0]] {
                    runs.push((FuncId(id), args.into()));
                }
                id += 1;
            }
        }
        writeln!(
            declarations,
            "sig g{id} = (f64) -> ({kind}, u8)\ninst n{id} = test@0/numeric/f{id} [] []"
        )
        .unwrap();
        writeln!(bodies,"fn f{id} = n{id} g{id} {{\nb0(v0: f64):\n  v1: {kind}, v2: u8 = float_to_int v0\n  return v1, v2\n}}").unwrap();
        let upper = (2f64).powi(width as i32 - i32::from(signed));
        let lower = if signed { -upper } else { 0.0 };
        for value in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.9,
            0.0,
            2.9,
            -2.9,
            lower,
            upper,
            upper - f64::EPSILON * upper,
            lower - 1.0,
        ] {
            runs.push((FuncId(id), vec![value.to_bits()]));
        }
        id += 1;
    }
    declarations.push_str(&bodies);
    let m = text::parse(&declarations).unwrap();
    wf::module(&m).unwrap();
    execute(&m, &runs, false);
}

#[test]
fn memory_growth_region_and_capacity_refusal_match_evaluator() {
    let source = r#"core v1
sig g0 = (u64) -> (u64, u64)
sig g1 = () -> (bool, u64)
inst n0 = test@0/memory/growth [] []
inst n1 = test@0/memory/region [] []
inst n2 = test@0/memory/refusal [] []
fn f0 = n0 g0 {
  addr a0 = v2 : u64
  addr a1 = v7 : u64
b0(v0: u64):
  v1: u64 = const 8
  v2: ptr, v3: u64 = alloc own v1 align 8 charge v1
  address a0
  store a0, v0
  v4: u64 = const 16
  v5: bool, v6: u64 = limit_enter v4
  v7: ptr, v8: u64 = alloc own v4 align 8 charge v4 like v2
  address a1
  copy a1, a0, v1
  free v2 charge v1
  limit_exit
  v9: u64 = read a1
  free v7 charge v4
  v10: u64 = const 101
  v11: bool, v12: u64 = limit_enter v10
  return v9, v12
}
fn f1 = n1 g0 {
  addr a0 = v3 : u64
b0(v0: u64):
  v1: ptr = region_open
  v2: u64 = const 8
  v3: ptr, v4: u64 = alloc own v2 align 8 charge v2 in v1
  address a0
  store a0, v0
  v5: u64 = const 128
  v6: bool, v7: u64 = limit_enter v5
  v8: u64 = read a0
  region_close v1
  return v8, v7
}
fn f2 = n2 g1 {
b0:
  v0: u64 = const 200
  v5: u64 = const 8
  v1: ptr, v2: u64 = alloc own v5 align 8 charge v0
  v3: ptr = const 0x0
  v4: bool = ne v1, v3
  return v4, v2
}
"#;
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    execute_limit(
        &module,
        &[
            (FuncId(0), vec![42]),
            (FuncId(1), vec![91]),
            (FuncId(2), vec![]),
        ],
        false,
        100,
    );
}

#[test]
fn every_whole_v0_sample_matches_evaluator() {
    let samples = [
        include_str!("../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../hirsel-core-ir/samples/compare-convert.core"),
        include_str!("../../hirsel-core-ir/samples/control.core"),
        include_str!("../../hirsel-core-ir/samples/slots.core"),
        include_str!("../../hirsel-core-ir/samples/calls.core"),
    ];
    for source in samples {
        let module = text::parse(source).unwrap();
        execute(&module, &sample_runs(&module, source), false);
    }
}

#[test]
fn every_front_end_run_case_matches_evaluator() {
    fn files(path: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out)
            } else if path.extension().is_some_and(|e| e == "hirsel") {
                out.push(path)
            }
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/front");
    let mut cases = Vec::new();
    files(&root.join("cases"), &mut cases);
    cases.sort();
    let mut count = 0;
    for file in cases {
        let source = fs::read_to_string(&file).unwrap();
        if !source.lines().any(|l| l.starts_with("// expect: run ")) {
            continue;
        }
        let (module, functions) = front::lower(&root, &source);
        let mut runs = Vec::new();
        for line in source
            .lines()
            .filter_map(|l| l.strip_prefix("// expect: run "))
        {
            let call = line.split_once(" = ").unwrap().0;
            let (name, args) = call.strip_suffix(')').unwrap().split_once('(').unwrap();
            let f = functions[name];
            let sig = &module.signatures[module.functions[f.index()].signature.index()];
            let args = sig
                .params
                .iter()
                .zip(args.split(',').filter(|a| !a.is_empty()))
                .map(|(p, a)| {
                    let Param::Value(t) = p else {
                        panic!("scalar run parameter")
                    };
                    eval::scalar(*t, a.trim())
                })
                .collect();
            runs.push((f, args));
            count += 1;
        }
        execute(&module, &runs, false);
    }
    assert!(count > 0, "the front-end corpus must execute run cases");
}

#[test]
fn declared_places_variants_and_destination_calls_match_evaluator() {
    let source = r#"core v1
type t0 = own "Row" { small: u8, large: u64 }
type t1 = own "Maybe" | None { } | Some { x: u64 }
type t2 = own "UnusedFile" { file: hostfile }
type t3 = own "UnusedSequence" { bytes: seq u8 }
type t4 = own "UnusedCounted" { data: rc t3 }
sig g0 = (u64) -> (u8, u64, u32)
sig g1 = (u64) -> () dest t0
inst n0 = test@0/places/run [] []
inst n1 = test@0/places/fill [] []
fn f0 = n0 g0 {
  slot s0 = t0
  slot s1 = t1
  addr a0 = s0 : t0
  addr a1 = s0 field 0 : u8
  addr a2 = s0 field 1 : u64
  addr a3 = s1 : t1
  addr a4 = s1 case 1 0 : u64
b0(v0: u64):
  live s0
  live s1
  address a0
  address a1
  address a2
  address a3
  address a4
  call f1 g1 (&a0, v0)
  set_case a3, 1
  store a4, v0
  v1: u8 = read a1
  v2: u64 = read a2
  v3: u32 = case a3
  dead s1
  dead s0
  return v1, v2, v3
}
fn f1 = n1 g1 {
  addr a0 = v0 : t0
  addr a1 = a0 field 0 : u8
  addr a2 = a0 field 1 : u64
b0(v0: ptr, v1: u64):
  address a0
  address a1
  address a2
  v2: u8 = const 7
  store a1, v2
  store a2, v1
  return
}
"#;
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    execute(&module, &[(FuncId(0), vec![42])], false);
}

#[test]
fn every_whole_v1a_sample_matches_evaluator() {
    for source in [
        include_str!("../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../hirsel-core-ir/samples/variants.core"),
    ] {
        let module = text::parse(source).unwrap();
        let ordinary = sample_runs(&module, source);
        execute(&module, &ordinary, false);
        let mut cases: Vec<_> = ordinary.into_iter().map(|run| (run, 1_000_000)).collect();
        for line in source
            .lines()
            .filter(|l| l.trim().starts_with("; run limit "))
        {
            let (n, call) = line
                .trim()
                .strip_prefix("; run limit ")
                .unwrap()
                .split_once(": ")
                .unwrap();
            let runs = sample_runs(&module, &format!("; run: {call}"));
            let limit = n.parse().unwrap();
            execute_limit(&module, &runs, false, limit);
            cases.extend(runs.into_iter().map(|run| (run, limit)));
        }
        for (run, limit) in cases {
            let baseline = eval::run(&module, run.0, &run.1, limit);
            for n in 1..=baseline.admissions {
                for later in [false, true] {
                    execute_injected(
                        &module,
                        std::slice::from_ref(&run),
                        false,
                        limit,
                        Some((n, later)),
                    );
                }
            }
        }
    }
}

#[test]
fn capacity_at_each_admission_preserves_cleanup_chain_and_balance() {
    let module = text::parse(include_str!("support/admissions.core")).unwrap();
    let mut executions = 0;
    for argument in [0, 1] {
        let runs = [(FuncId(0), vec![argument])];
        let baseline = eval::run(&module, FuncId(0), &[argument], 1000);
        execute_limit(&module, &runs, false, 1000);
        for n in 1..=baseline.admissions {
            for later in [false, true] {
                execute_injected(&module, &runs, false, 1000, Some((n, later)));
                executions += 1;
            }
        }
    }
    assert_eq!(executions, 14);
}

#[test]
fn counted_variant_chain_uses_constant_stack_and_refunds_every_node() {
    let module = text::parse(include_str!("support/counted-chain.core")).unwrap();
    execute(
        &module,
        &[(FuncId(0), vec![0]), (FuncId(0), vec![32])],
        false,
    );
    for n in 1..=5 {
        for later in [false, true] {
            execute_injected(
                &module,
                &[(FuncId(0), vec![4])],
                false,
                1000,
                Some((n, later)),
            );
        }
    }
    // Hand-derived oracle avoids imposing the evaluator's recursive host-stack
    // implementation on the generated code's constant-stack requirement.
    let n = 100_000;
    execute_reports(
        &module,
        &[(FuncId(0), vec![n])],
        false,
        10_000_000,
        None,
        Some(&[eval::Report {
            outcome: Outcome::Returned(vec![n]),
            used: 0,
            trace: vec![],
            admissions: n as usize + 1,
        }]),
    );
}

#[test]
fn every_compact_sequence_sample_matches_evaluator() {
    let source = include_str!("../../hirsel-core-ir/samples/sequences.core");
    let module = text::parse(source).unwrap();
    execute(&module, &sample_runs(&module, source), false);
    let mut count = 0;
    for line in source
        .lines()
        .filter(|l| l.trim().starts_with("; run limit "))
    {
        let (n, call) = line
            .trim()
            .strip_prefix("; run limit ")
            .unwrap()
            .split_once(": ")
            .unwrap();
        let runs = sample_runs(&module, &format!("; run: {call}"));
        execute_limit(&module, &runs, false, n.parse().unwrap());
        count += runs.len();
    }
    assert_eq!(count, 7);
}

#[test]
fn fixed_span_payload_pointers_and_scalar_reads_match_evaluator() {
    let module = text::parse(include_str!("support/spans.core")).unwrap();
    execute(
        &module,
        &[
            (FuncId(0), vec![0]),
            (FuncId(0), vec![1]),
            (FuncId(0), vec![257]),
        ],
        false,
    );
}

#[test]
fn owned_callable_captures_and_captureless_cleanup_use_function_row_abi() {
    let module = text::parse(include_str!("support/callable.core")).unwrap();
    let runs = [(FuncId(0), vec![12345]), (FuncId(3), vec![])];
    let reports = [
        eval::Report {
            outcome: Outcome::Returned(vec![12345]),
            used: 0,
            trace: vec![],
            admissions: 1,
        },
        eval::Report {
            outcome: Outcome::Returned(vec![13]),
            used: 0,
            trace: vec![],
            admissions: 0,
        },
    ];
    execute_reports(&module, &runs, false, 1000, None, Some(&reports));
}

#[test]
fn failure_message_moves_its_carrier_out_of_the_native_frame() {
    let module = text::parse(include_str!("support/message.core")).unwrap();
    execute(&module, &[(FuncId(0), vec![])], false);
}

#[test]
fn direct_ssa_handles_block_order_and_repeated_address_snapshots() {
    let module = text::parse(include_str!("support/ssa.core")).unwrap();
    execute(
        &module,
        &[
            (FuncId(0), vec![19]),
            (FuncId(1), vec![1, 11, 22]),
            (FuncId(1), vec![0, 11, 22]),
        ],
        false,
    );
}

#[test]
fn nested_counted_sequences_clean_refused_prefixes_and_deep_chains() {
    let module = text::parse(include_str!("support/counted-sequences.core")).unwrap();
    execute(
        &module,
        &[(FuncId(0), vec![0]), (FuncId(0), vec![8])],
        false,
    );
    // Every admission in a two-link construction, including the root.
    for n in 1..=7 {
        for later in [false, true] {
            execute_injected(
                &module,
                &[(FuncId(0), vec![2])],
                false,
                1000,
                Some((n, later)),
            );
        }
    }
    let n = 50_000;
    execute_reports(
        &module,
        &[(FuncId(0), vec![n])],
        false,
        10_000_000,
        None,
        Some(&[eval::Report {
            outcome: Outcome::Returned(vec![n]),
            used: 0,
            trace: vec![],
            admissions: 1 + 3 * n as usize,
        }]),
    );
}

#[test]
fn maps_clean_live_entries_in_order_and_continue_after_finalizer_failure() {
    // The evaluator does not model Map entries. This independent C producer
    // writes FORM's frozen layout, including poisoned removed entries.
    let module = text::parse(
        r#"core v1
type t0 = resource "R" drop f2 { id: u64 }
type t1 = own "Resources" { entries: map t0 t0 }
type t2 = own "Leaf" { value: u64 }
type t3 = own "CountedMap" { entries: map rc t2 arc t2 }
sig g0 = (ptr) -> ()
sig g1 = (ptr) -> ()
inst n0 = test@0/maps/ordinary [] []
inst n1 = test@0/maps/counted [] []
inst n2 = test@0/maps/finalizer [] []
fn f0 = n0 g0 {
  addr a0 = v0 : t1
b0(v0: ptr):
  address a0
  drop a0 t1
  return
}
fn f1 = n1 g0 {
b0(v0: ptr):
  release v0 rc t3
  return
}
fn f2 = n2 g1 {
b0(v0: ptr):
  raise unwrap
}
"#,
    )
    .unwrap();
    let scratch = Scratch::new();
    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let object = emit_object_for_test(
        &module,
        &layouts,
        &Options {
            target: "x86_64-unknown-linux-gnu",
            optimize: false,
        },
    )
    .unwrap();
    fs::write(scratch.0.join("input.o"), object.bytes).unwrap();
    let source = format!(
        r#"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
{header}
extern HirselCoreContext *hirsel_core_test_context_new(uint64_t);
extern void hirsel_core_test_inject(HirselCoreContext *,size_t,bool);
extern uint64_t hirsel_core_test_available(const HirselCoreContext *);
extern size_t hirsel_core_test_attempts(const HirselCoreContext *);
extern uint8_t {ordinary}(void *, void *);
extern uint8_t {counted}(void *, void *);
static uint64_t trace[8]; static size_t traces;
void hirsel_core_test_drop_trace(uint64_t id) {{ if(traces==8) abort(); trace[traces++]=id; }}
static void *allocate(HirselCoreContext *ctx, size_t size, uint64_t charge) {{
  void *p=hirsel_core_alloc(ctx,0,size,8,charge,false,HIRSEL_CORE_NATIVE_GRAPH).pointer; if(!p) abort(); return p;
}}
static void *resource(HirselCoreContext *ctx, uint64_t id) {{
  uint64_t *p=hirsel_core_resource_alloc(ctx,8,8,8,0).pointer; if(!p) abort(); *p=id; return p;
}}
static void *leaf(HirselCoreContext *ctx, bool atomic) {{
  uint64_t *p=allocate(ctx,16,16); hirsel_core_counted_init(p,atomic); p[1]=7; return p;
}}
int main(void) {{
  HirselCoreContext *ctx=hirsel_core_test_context_new(10000);
  uint64_t *entries=allocate(ctx,4*32+8,128);
  memset(entries,0xcc,4*32+8);
  for(size_t i=0;i<3;i+=2) {{ entries[4*i+1]=1; entries[4*i+2]=(uintptr_t)resource(ctx,10*i+1); entries[4*i+3]=(uintptr_t)resource(ctx,10*i+2); }}
  entries[5]=0;
  uint64_t carrier[]={{(uintptr_t)entries,3,4}};
  hirsel_core_test_inject(ctx,1,true);
  size_t attempts=hirsel_core_test_attempts(ctx);
  if({ordinary}(ctx,carrier)!=1 || traces!=4 || trace[0]!=1 || trace[1]!=2 || trace[2]!=21 || trace[3]!=22) return 1;
  if(hirsel_core_test_attempts(ctx)!=attempts) return 2;
  HirselCoreCauseCell *causes=hirsel_core_causes_take(ctx);
  size_t n=0; for(const HirselCoreCauseCell *c=causes;c;c=hirsel_core_cause_next(c)) {{ if(hirsel_core_cause_value(c)->kind!=3) return 3; n++; }}
  if(n!=4 || hirsel_core_test_available(ctx)!=10000-128) return 4;
  hirsel_core_causes_drop(causes);
  if(hirsel_core_test_available(ctx)!=10000) return 5;
  hirsel_core_context_free(ctx);

  ctx=hirsel_core_test_context_new(10000);
  entries=allocate(ctx,4*32+8,128);
  memset(entries,0xcc,4*32+8);
  for(size_t i=0;i<3;i+=2) {{ entries[4*i+1]=1; entries[4*i+2]=(uintptr_t)leaf(ctx,false); entries[4*i+3]=(uintptr_t)leaf(ctx,true); }}
  entries[5]=0;
  uint64_t *root=allocate(ctx,32,32); hirsel_core_counted_init(root,false);
  root[1]=(uintptr_t)entries; root[2]=3; root[3]=4;
  hirsel_core_test_inject(ctx,1,true); attempts=hirsel_core_test_attempts(ctx);
  if({counted}(ctx,root)!=0 || hirsel_core_test_available(ctx)!=10000 || hirsel_core_test_attempts(ctx)!=attempts) return 6;
  hirsel_core_context_free(ctx);
  return 0;
}}
"#,
        header = include_str!("../../hirsel-core-rt/include/hirsel_core_rt.h"),
        ordinary = symbol(&module.instances[0]),
        counted = symbol(&module.instances[1])
    );
    assert!(link_run(&scratch, &source).is_empty());
}

#[test]
fn cancellation_runtime_status_takes_cleanup_or_propagates() {
    let module = text::parse(
        r#"core v1
sig g0 = () -> ()
sig g1 = () -> (u64)
inst n0 = test@0/cancel/cleanup [] []
inst n1 = test@0/cancel/propagate [] []
fn f0 = n0 g1 {
b0:
  call rt.check_cancel g0 () fail b1
  v0: u64 = const 3
  return v0
b1:
  v1: u64 = const 7
  return v1
}
fn f1 = n1 g1 {
b0:
  call rt.check_cancel g0 ()
  v0: u64 = const 9
  return v0
}
"#,
    )
    .unwrap();
    let scratch = Scratch::new();
    let object = emit_object(
        &module,
        &Options {
            target: "x86_64-unknown-linux-gnu",
            optimize: false,
        },
    )
    .unwrap();
    fs::write(scratch.0.join("input.o"), object.bytes).unwrap();
    // Script just the C row to exercise both outcomes deterministically. The
    // real scheduler owns cancellation requests and has its own runtime tests.
    let source = format!(
        r#"
#include <stdint.h>
#include <stdlib.h>
static uint8_t cancelled; static uint64_t calls;
uint8_t hirsel_core_check_cancel(void *ctx, uint64_t site) {{
  if(ctx!=(void*)16 || site!=0) abort(); calls++; return cancelled;
}}
struct R {{ uint64_t value; uint8_t status; }};
extern struct R {cleanup}(void *);
extern struct R {propagate}(void *);
int main(void) {{
  struct R r={cleanup}((void*)16); if(r.status || r.value!=3) return 1;
  r={propagate}((void*)16); if(r.status || r.value!=9) return 2;
  cancelled=1;
  r={cleanup}((void*)16); if(r.status || r.value!=7) return 3;
  r={propagate}((void*)16); if(r.status!=1 || calls!=4) return 4;
  return 0;
}}
"#,
        cleanup = symbol(&module.instances[0]),
        propagate = symbol(&module.instances[1])
    );
    assert!(link_run(&scratch, &source).is_empty());
}
