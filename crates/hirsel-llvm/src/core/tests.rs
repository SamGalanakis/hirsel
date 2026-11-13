use super::*;
use hirsel_core_ir::{eval, text, wf};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn output(command: &mut Command) -> String {
    let out = command
        .output()
        .unwrap_or_else(|e| panic!("{command:?}: {e}"));
    assert!(
        out.status.success(),
        "{command:?}: {}\n{}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
fn cty(t: Ty) -> &'static str {
    match t {
        Ty::Ptr => "void *",
        Ty::Scalar(s) => match s {
            Scalar::I8 => "int8_t",
            Scalar::U8 | Scalar::Bool => "uint8_t",
            Scalar::I16 => "int16_t",
            Scalar::U16 => "uint16_t",
            Scalar::I32 => "int32_t",
            Scalar::U32 | Scalar::Char => "uint32_t",
            Scalar::I64 => "int64_t",
            Scalar::U64 => "uint64_t",
            Scalar::F32 => "float",
            Scalar::F64 => "double",
        },
    }
}
fn execute(module: &Module, cases: &[(FuncId, Vec<u64>)]) -> Vec<Vec<u64>> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "hirsel-core-llvm-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _scratch = Scratch(dir.clone());
    let ir = emit_ir(module).unwrap();
    // Test-only verifier: the shipped entry point trusts its producer.
    let context = Context::new();
    let llvm = LlvmModule::parse(&context, "test", &ir).unwrap();
    llvm.verify().unwrap_or_else(|e| panic!("{e}\n{ir}"));
    let bytes = emit_object(module, Optimization::None).unwrap();
    std::fs::write(dir.join("core.o"), bytes).unwrap();
    let mut c = String::from(
        "#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\nstatic float f32(uint32_t b){float x;memcpy(&x,&b,4);return x;}\nstatic double f64(uint64_t b){double x;memcpy(&x,&b,8);return x;}\n",
    );
    for (i, f) in module.functions.iter().enumerate() {
        let s = &module.signatures[f.signature.index()];
        if s.dest.is_some() {
            panic!("destination C fixture not implemented");
        }
        let ret = match s.results.as_slice() {
            [] => "void".into(),
            [t] => cty(*t).into(),
            ts => {
                writeln!(
                    c,
                    "typedef struct {{ {} }} R{i};",
                    ts.iter()
                        .enumerate()
                        .map(|(i, t)| format!("{} r{i};", cty(*t)))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
                .unwrap();
                format!("R{i}")
            }
        };
        writeln!(
            c,
            "extern {ret} {}({});",
            symbol(module, FuncId(i as u32)),
            std::iter::once("void *")
                .chain(s.params.iter().map(|p| match p {
                    Param::Value(t) => cty(*t),
                    Param::Address(_) => "void *",
                }))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .unwrap();
    }
    c.push_str("int main(void){\n");
    for (k, (f, args)) in cases.iter().enumerate() {
        let sig = &module.signatures[module.functions[f.index()].signature.index()];
        let args = sig
            .params
            .iter()
            .zip(args)
            .map(|(t, b)| {
                let Param::Value(t) = t else {
                    panic!("pointer fixture")
                };
                match t {
                    Ty::Scalar(Scalar::F32) => format!("f32(UINT32_C({b}))"),
                    Ty::Scalar(Scalar::F64) => format!("f64(UINT64_C({b}))"),
                    _ => format!("({})UINT64_C({b})", cty(*t)),
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let args = if args.is_empty() {
            "NULL".into()
        } else {
            format!("NULL, {args}")
        };
        let ret = match sig.results.as_slice() {
            [] => "void".into(),
            [t] => cty(*t).into(),
            _ => format!("R{}", f.0),
        };
        if ret == "void" {
            writeln!(c, "{}({args});", symbol(module, *f)).unwrap();
        } else {
            writeln!(c, "{ret} r{k} = {}({args});", symbol(module, *f)).unwrap();
        }
        for (j, &t) in sig.results.iter().enumerate() {
            let value = if sig.results.len() == 1 {
                format!("r{k}")
            } else {
                format!("r{k}.r{j}")
            };
            writeln!(c,"uint64_t b{k}_{j}=0; memcpy(&b{k}_{j},&{value},{}); printf(\"%llu \",(unsigned long long)b{k}_{j});",size(t)).unwrap();
        }
        c.push_str("puts(\"\");\n");
    }
    c.push_str("return 0;}\n");
    std::fs::write(dir.join("test.c"), c).unwrap();
    let prefix =
        PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").expect("pinned LLVM prefix"));
    output(
        Command::new(prefix.join("bin/clang"))
            .arg(format!("--ld-path={}", prefix.join("bin/ld.lld").display()))
            .args(["-std=c11", "-O0"])
            .arg(dir.join("test.c"))
            .arg(dir.join("core.o"))
            .arg("-lm")
            .arg("-o")
            .arg(dir.join("test")),
    );
    output(&mut Command::new(dir.join("test")))
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|x| x.parse().unwrap())
                .collect()
        })
        .collect()
}
fn agree(module: &Module, cases: &[(FuncId, Vec<u64>)]) {
    let got = execute(module, cases);
    assert_eq!(got.len(), cases.len());
    for ((f, args), got) in cases.iter().zip(got) {
        assert_eq!(
            eval::Outcome::Returned(got),
            eval::run(module, *f, args, u64::MAX).outcome,
            "f{}({args:?})",
            f.0
        );
    }
}

#[test]
fn compare_convert_sample_matches_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/compare-convert.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = vec![];
    for line in source.lines() {
        let Some(run) = line.trim().strip_prefix("; run: f") else {
            continue;
        };
        let (f, rest) = run.split_once('(').unwrap();
        let (args, _) = rest.split_once(')').unwrap();
        let f = FuncId(f.parse().unwrap());
        let sig = &module.signatures[module.functions[f.index()].signature.index()];
        let args = args
            .split(',')
            .zip(&sig.params)
            .map(|(arg, t)| {
                let Param::Value(t) = t else { unreachable!() };
                eval::scalar(*t, arg.trim())
            })
            .collect();
        cases.push((f, args));
    }
    assert_eq!(cases.len(), 22);
    agree(&module, &cases);
}

#[test]
fn infallible_arithmetic_sample_matches_evaluator() {
    let mut module =
        text::parse(include_str!("../../../hirsel-core-ir/samples/arith.core")).unwrap();
    // Those two bodies require the not-yet-specified v1 failure ABI. Their
    // declarations remain; the v0 numeric cases below call only infallible bodies.
    module.functions[1].body = None;
    module.functions[6].body = None;
    let cases = vec![
        (FuncId(0), vec![3f64.to_bits(), 4f64.to_bits()]),
        (FuncId(0), vec![1e308f64.to_bits(), 1e308f64.to_bits()]),
        (FuncId(2), vec![200]),
        (FuncId(2), vec![1]),
        (FuncId(3), vec![200]),
        (FuncId(3), vec![154]),
        (FuncId(4), vec![155]),
        (FuncId(4), vec![156]),
        (FuncId(5), vec![0]),
        (FuncId(5), vec![1]),
        (FuncId(5), vec![u32::MAX.into()]),
    ];
    agree(&module, &cases);
}

#[test]
fn c_results_and_indirect_calls_match_evaluator() {
    let module = text::parse(
        r#"core v0
sig g0 = (u32, u32) -> (u32, u32)
sig g1 = (u32, u32) -> (u32)
sig g2 = (u8, u8, u8) -> (u8, u8, u8)
sig g3 = (f32, f32, f64) -> (f32, f32, f64)
sig g4 = (u64, u64, u64) -> (u64, u64, u64)
inst n0 = tests@0/abi/pair [] []
inst n1 = tests@0/abi/caller [] []
inst n2 = tests@0/abi/small [] []
inst n3 = tests@0/abi/floats [] []
inst n4 = tests@0/abi/large [] []
fn f0 = n0 g0 {
b0(v0: u32, v1: u32):
 return v1, v0
}
fn f1 = n1 g1 {
b0(v0: u32, v1: u32):
 v2: ptr = fnref f0
 v3: u32, v4: u32 = call v2 g0 (v0, v1)
 v5: u32 = wrapping_sub v3, v4
 return v5
}
fn f2 = n2 g2 {
b0(v0: u8, v1: u8, v2: u8):
 return v2, v0, v1
}
fn f3 = n3 g3 {
b0(v0: f32, v1: f32, v2: f64):
 return v1, v0, v2
}
fn f4 = n4 g4 {
b0(v0: u64, v1: u64, v2: u64):
 return v2, v1, v0
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    agree(
        &module,
        &[
            (FuncId(0), vec![11, 900]),
            (FuncId(1), vec![11, 900]),
            (FuncId(2), vec![1, 2, 255]),
            (
                FuncId(3),
                vec![
                    u64::from(3.5f32.to_bits()),
                    u64::from((-0f32).to_bits()),
                    8.25f64.to_bits(),
                ],
            ),
            (FuncId(4), vec![u64::MAX, 3, 9]),
        ],
    );
}

#[test]
fn loop_edges_transfer_parameters_in_parallel() {
    let module = text::parse(
        r#"core v0
sig g0 = (u32) -> (u32, u32)
inst n0 = tests@0/loop/swap [] []
fn f0 = n0 g0 {
 slot s0 = u32
 addr a0 = s0 : u32
b0(v0: u32):
 live s0
 address a0
 store a0, v0
 v1: u32 = const 1
 v2: u32 = const 2
 v3: u32 = const 0
 jump b1(v1, v2)
b1(v4: u32, v5: u32):
 v6: u32 = read a0
 v7: bool = ne v6, v3
 branch v7, b2, b3
b2:
 v8: u32 = wrapping_sub v6, v1
 store a0, v8
 jump b1(v5, v4)
b3:
 dead s0
 return v4, v5
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    agree(
        &module,
        &[
            (FuncId(0), vec![0]),
            (FuncId(0), vec![1]),
            (FuncId(0), vec![2]),
            (FuncId(0), vec![7]),
        ],
    );
}

#[test]
fn float_intrinsics_preserve_nan_and_signed_zero() {
    let mut source = String::from("core v0\n");
    let mut cases = vec![];
    for kind in ["f32", "f64"] {
        let bits = |n: f64| {
            if kind == "f32" {
                u64::from((n as f32).to_bits())
            } else {
                n.to_bits()
            }
        };
        for op in [
            "min",
            "max",
            "min_number",
            "max_number",
            "sqrt",
            "floor",
            "ceil",
            "trunc",
            "round",
            "abs",
        ] {
            let binary = op.starts_with("min") || op.starts_with("max");
            let i = source.matches("\nfn ").count();
            let result = if binary { 2 } else { 1 };
            writeln!(source,"sig g{i} = ({kind}{}) -> ({kind})\ninst n{i} = tests@0/float/{kind}_{op} [] []\nfn f{i} = n{i} g{i} {{\nb0(v0: {kind}{}):\n v{result}: {kind} = {op} v0{}\n return v{result}\n}}",if binary {format!(", {kind}")} else {String::new()},if binary {format!(", v1: {kind}")} else {String::new()},if binary {", v1"} else {""}).unwrap();
            if binary {
                for (a, b) in [
                    (0.0, -0.0),
                    (-0.0, 0.0),
                    (f64::NAN, 2.5),
                    (2.5, f64::NAN),
                    (f64::NAN, f64::NAN),
                    (-3.0, 2.5),
                ] {
                    cases.push((FuncId(i as u32), vec![bits(a), bits(b)]));
                }
            } else {
                for a in [0.0, -0.0, f64::NAN, f64::INFINITY, -3.5, 2.5] {
                    cases.push((FuncId(i as u32), vec![bits(a)]));
                }
            }
        }
    }
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    agree(&module, &cases);
}
