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
    let scratch = Scratch::new();
    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let artifact = emit_object_with_layouts(
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
    source.push('\n');
    for (i, s) in module.signatures.iter().enumerate() {
        if s.results.len() > 1 {
            write!(source, "struct r{i} {{ ").unwrap();
            for (j, &t) in s.results.iter().enumerate() {
                write!(source, "{} v{j};", ctype(t)).unwrap();
            }
            source.push_str(" };\n");
        }
    }
    for f in &module.functions {
        let s = &module.signatures[f.signature.index()];
        let result = match s.results.as_slice() {
            [] => "void".into(),
            [t] => ctype(*t).into(),
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
            let Param::Value(t) = p else {
                panic!("scalar test")
            };
            write!(source, ", {}", ctype(*t)).unwrap();
        }
        source.push_str(");\n");
    }
    source.push_str("int main(void) {\n");
    let mut expected = Vec::new();
    for (i, (f, args)) in runs.iter().enumerate() {
        let function = &module.functions[f.index()];
        let s = &module.signatures[function.signature.index()];
        let report = eval::run(module, *f, args, limit);
        let Outcome::Returned(values) = report.outcome else {
            panic!("infallible test failed")
        };
        expected.extend(values);
        expected.push(report.used);
        source.push_str("{\n");
        writeln!(
            source,
            "HirselCoreContext *ctx = hirsel_core_context_new(UINT64_C({limit}));"
        )
        .unwrap();
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
        let result = match s.results.as_slice() {
            [] => String::new(),
            [t] => format!("{} value = ", ctype(*t)),
            _ => format!("struct r{} value = ", function.signature.0),
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
        for (j, t) in s.results.iter().enumerate() {
            let expr = if s.results.len() == 1 {
                "value".into()
            } else {
                format!("value.v{j}")
            };
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
        writeln!(source,"HirselCoreAllocation probe = hirsel_core_limit_enter(ctx, UINT64_MAX); uint64_t used = UINT64_C({limit}) - probe.capacity.available; fwrite(&used,8,1,stdout); hirsel_core_context_free(ctx);").unwrap();
        source.push_str("}\n");
    }
    source.push_str("return 0; }\n");
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
    assert_eq!(actual, expected, "optimize={optimize}");
}

#[test]
fn compare_convert_sample_matches_evaluator_through_c_abi() {
    let text = include_str!("../../hirsel-core-ir/samples/compare-convert.core");
    let module = text::parse(text).unwrap();
    let runs = sample_runs(&module, text);
    assert_eq!(runs.len(), 22);
    execute(&module, &runs, false);
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
fn runtime_independent_arithmetic_sample_functions_match_evaluator() {
    let text = include_str!("../../hirsel-core-ir/samples/arith.core");
    let mut m = text::parse(text).unwrap();
    let runs: Vec<_> = sample_runs(&m, text)
        .into_iter()
        .filter(|(f, _)| matches!(f.0, 0 | 2 | 3 | 4 | 5))
        .collect();
    m.functions[1].body = None;
    m.functions[6].body = None;
    assert_eq!(runs.len(), 11);
    execute(&m, &runs, false);
}

#[test]
fn slots_loops_calls_and_c_struct_returns_match_evaluator() {
    let source = r#"core v0
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
  v2: ptr, v3: u64 = alloc own 8 align 8 charge v1
  address a0
  store a0, v0
  v4: u64 = const 16
  v5: bool, v6: u64 = limit_enter v4
  v7: ptr, v8: u64 = alloc own 16 align 8 charge v4 like v2
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
  v3: ptr, v4: u64 = alloc own 8 align 8 charge v2 in v1
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
  v1: ptr, v2: u64 = alloc own 8 align 8 charge v0
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
    use hirsel_core_front::{Emitter, ModuleInput, Program};
    use hirsel_core_ids::PackageId;
    use hirsel_core_resolve::{Environment, ModuleId, NameIndex, PackageManifest};
    use hirsel_core_syntax::{ParseMode, parse};
    use std::collections::HashMap;
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
        let core = PackageId {
            name: "core".into(),
            version: "1".into(),
        };
        let app = PackageId {
            name: "app".into(),
            version: "1".into(),
        };
        let mut modules = Vec::new();
        for m in PackageManifest::directory(&root.join("core"), core.clone(), true)
            .unwrap()
            .read()
            .unwrap()
        {
            assert!(m.parsed.diagnostics.is_empty());
            modules.push(ModuleInput {
                id: m.module,
                syntax: m.parsed.syntax,
            });
        }
        let id = ModuleId {
            package: app.clone(),
            modules: vec!["main".into()],
        };
        let parsed = parse(&source, ParseMode::Source);
        assert!(parsed.diagnostics.is_empty(), "{}", file.display());
        modules.push(ModuleInput {
            id,
            syntax: parsed.syntax,
        });
        let mut index = NameIndex::new(Environment::default());
        for m in &modules {
            index.add_module(&m.id);
            for (i, item) in m.syntax.items.iter().enumerate() {
                index.index_item(&m.id, i as u32, item);
            }
        }
        index.configure_core(core);
        let program = Program::declare(&index, &modules);
        assert!(
            program.refusals.is_empty(),
            "{}: {:?}",
            file.display(),
            program.refusals
        );
        let mut emitter = Emitter::new();
        let mut functions = HashMap::new();
        for (i, f) in program.fns.iter().enumerate() {
            if f.path.0.package != app || f.body.is_none() {
                continue;
            }
            let checked = program.check_lower(i, &mut emitter);
            assert!(
                checked.refusals.is_empty(),
                "{}: {:?}",
                file.display(),
                checked.refusals
            );
            assert!(
                checked.gaps.is_empty(),
                "{}: {:?}",
                file.display(),
                checked.gaps
            );
            functions.insert(f.path.0.name.clone(), checked.function.unwrap());
        }
        let module = emitter.module;
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
