//! Execute objects through a separately compiled C caller. This catches both
//! scalar semantics and the register packing a same-backend caller could mask.
use super::*;
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
    let scratch = Scratch::new();
    let artifact = emit_object(
        module,
        &Options {
            target: "x86_64-unknown-linux-gnu",
            optimize,
        },
    )
    .unwrap();
    fs::write(scratch.0.join("input.o"), artifact.bytes).unwrap();
    let mut source = String::from("#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n");
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
        let report = eval::run(module, *f, args, u64::MAX);
        let Outcome::Returned(values) = report.outcome else {
            panic!("infallible test failed")
        };
        expected.extend(values);
        source.push_str("{\n");
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
            "{result}{}(0",
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
                    match ty(*t).bits() {
                        64 => u64::MAX,
                        n => (1u64 << n) - 1,
                    }
                )
                .unwrap();
            }
            writeln!(source, "fwrite(&out{i}_{j}, 8, 1, stdout);").unwrap();
        }
        source.push_str("}\n");
    }
    source.push_str("return 0; }\n");
    fs::write(scratch.0.join("caller.c"), source).unwrap();
    let link = Command::new("cc")
        .current_dir(&scratch.0)
        .args(["caller.c", "input.o", "-lm", "-o", "run"])
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
    let text = include_str!("../../../hirsel-core-ir/samples/compare-convert.core");
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
    let text = include_str!("../../../hirsel-core-ir/samples/arith.core");
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
