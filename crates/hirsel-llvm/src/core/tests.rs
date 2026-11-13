#[path = "../../examples/support/front.rs"]
mod front;
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
struct Case {
    function: FuncId,
    args: Vec<u64>,
    limit: u64,
    injection: Option<(usize, bool)>,
}
struct NativeReport {
    outcome: eval::Outcome,
    used: u64,
    trace: Vec<u64>,
    admissions: usize,
    messages: Vec<String>,
    arc_events: Vec<(u8, u64)>,
}
fn has_hostfile_field(shape: &hirsel_core_layout::Shape) -> bool {
    shape.fields(None).is_some_and(|fields| {
        fields.iter().any(|field| {
            field.ty == hirsel_core_layout::FieldTy::Handle(hirsel_core_layout::Handle::HostFile)
        })
    })
}
fn execute(module: &Module, cases: &[Case]) -> Vec<NativeReport> {
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
            if std::thread::panicking() {
                eprintln!("native failure artifacts: {}", self.0.display());
            } else {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
    let _scratch = Scratch(dir.clone());
    let ir = emit_ir(module).unwrap();
    let context = Context::new();
    let llvm = LlvmModule::parse(&context, "test", &ir).unwrap();
    llvm.verify().unwrap_or_else(|e| panic!("{e}\n{ir}"));
    let bytes = emit_object(module, Optimization::None).unwrap();
    std::fs::write(dir.join("core.o"), bytes).unwrap();
    let mut c = String::from(
        "#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n#include <assert.h>\n#include \"hirsel_core_rt.h\"\nextern uint64_t hirsel_core_test_available(const HirselCoreContext *);\nextern HirselCoreContext *hirsel_core_test_context_new(uint64_t);\nextern void hirsel_core_test_inject(HirselCoreContext *,size_t,bool);\nextern size_t hirsel_core_test_attempts(const HirselCoreContext *);\nstatic uint64_t trace[4096]; static size_t traces;\nvoid hirsel_core_test_drop_trace(uint64_t n){assert(traces<4096);trace[traces++]=n;}\nstatic float f32(uint32_t b){float x;memcpy(&x,&b,4);return x;}\nstatic double f64(uint64_t b){double x;memcpy(&x,&b,8);return x;}\nstatic const char *causes[]={\"Overflow\",\"Bounds\",\"Division\",\"Unwrap\",\"Fail\",\"Cancelled\",\"Capacity\"};\n",
    );
    for (i, f) in module.functions.iter().enumerate() {
        let s = &module.signatures[f.signature.index()];
        let mut fields = s.results.clone();
        fields.push(Ty::Scalar(Scalar::U8));
        let ret = if fields.len() == 1 {
            "uint8_t".into()
        } else {
            writeln!(
                c,
                "typedef struct {{ {} }} R{i};",
                fields
                    .iter()
                    .enumerate()
                    .map(|(i, t)| format!("{} r{i};", cty(*t)))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .unwrap();
            format!("R{i}")
        };
        writeln!(
            c,
            "extern {ret} {}({});",
            symbol(module, FuncId(i as u32)),
            std::iter::once("void *")
                .chain(s.dest.iter().map(|_| "void *"))
                .chain(s.params.iter().map(|p| match p {
                    Param::Value(t) => cty(*t),
                    Param::Address(_) => "void *",
                }))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .unwrap();
    }
    c.push_str(r#"
extern void hirsel_core_test_reset_limit(HirselCoreContext *, uint64_t);
static HirselCoreEvent arc_events[4096]; static size_t arc_count;
static void record_event(void *state, HirselCoreEvent e) {
    (void)state;
    if (e.kind>=10 && e.kind<=13) { assert(arc_count<4096); arc_events[arc_count++]=e; }
}
static size_t file_closes, file_deliveries;
static HirselCoreHostOutcome file_issue(void *state, HirselCoreHostRequest *r) {
    (void)state; HirselCoreHostOutcome out={0};
    if (r->operation==0) out.value=r->handle;
    else { assert(r->operation==3); file_closes++; hirsel_core_test_drop_trace(r->handle); out.status=1; out.error.tag=5; out.error.code=5; }
    return out;
}
static HirselCoreHostOutcome file_deliver(void *state, HirselCoreHostRequest *r, HirselCoreHostOutcome out) {
    (void)state; if(r->operation==3) file_deliveries++; return out;
}
static void prepare_files(HirselCoreContext *ctx, uint64_t last) {
    HirselCoreProvider provider={.issue=file_issue,.deliver=file_deliver};
    for(uint64_t i=1;i<=last;i++) {
        HirselCoreHostRequest request={.operation=0,.handle=i};
        HirselCoreHostOutcome out=hirsel_core_host_file_open(ctx,&provider,&request);
        assert(out.status==0 && out.error.tag==0 && out.value==i);
    }
    for(uint64_t i=1;i<last;i++) hirsel_core_hostfile_drop(ctx,i);
    file_closes=0; file_deliveries=0;
}
static void diagnostic(void *state, const uint8_t *s, size_t n) { (void)state; fwrite(s,1,n,stderr); assert(0); }
"#);
    let mut invocations = String::from(
        "int main(void){ HirselCoreProcessConfig cfg={.root_limit=UINT64_MAX,.diagnostic=diagnostic,.scheduler={.event=record_event}};\n",
    );

    for (k, case) in cases.iter().enumerate() {
        let f = case.function;
        let sig = &module.signatures[module.functions[f.index()].signature.index()];
        assert!(sig.dest.is_none(), "entry returns scalars to the C fixture");
        let args = sig
            .params
            .iter()
            .zip(&case.args)
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
            "ctx".into()
        } else {
            format!("ctx, {args}")
        };
        let ret = if sig.results.is_empty() {
            "uint8_t".into()
        } else {
            format!("R{}", f.0)
        };
        let limit = case.limit;
        let inject = case
            .injection
            .map(|(n, later)| format!("hirsel_core_test_inject(ctx,{n},{});", u8::from(later)))
            .unwrap_or_default();
        let hostfile = module.functions[f.index()]
            .body
            .as_ref()
            .is_some_and(|body| {
                body.slots.iter().any(|slot| {
                    let PlaceTy::Declared(t) = slot else {
                        return false;
                    };
                    has_hostfile_field(&module.types[t.index()])
                })
            });
        let files = if hostfile {
            format!("prepare_files(ctx,UINT64_C({}));", case.args[0])
        } else {
            String::new()
        };
        writeln!(c,"static uint8_t case{k}(HirselCoreContext *ctx,const HirselCoreProcessConfig *cfg,HirselCoreMainResult *result) {{ (void)cfg; (void)result; hirsel_core_test_reset_limit(ctx,UINT64_C({limit})); {files} traces=0; arc_count=0; {inject} uint64_t initial=hirsel_core_test_available(ctx); {ret} r{k} = {}({args});",symbol(module,f)).unwrap();
        writeln!(invocations, "assert(hirsel_core_run(&cfg,case{k})==0);").unwrap();
        let status = if sig.results.is_empty() {
            format!("r{k}")
        } else {
            format!("r{k}.r{}", sig.results.len())
        };
        if hostfile {
            writeln!(
                c,
                "assert(file_closes==({status} ? 0 : 1)); assert(file_deliveries==file_closes);"
            )
            .unwrap();
        }
        writeln!(
            c,
            "if ({status}) {{ printf(\"F\"); }} else {{ printf(\"R\");"
        )
        .unwrap();
        for (j, &t) in sig.results.iter().enumerate() {
            writeln!(c,"uint64_t b{k}_{j}=0; memcpy(&b{k}_{j},&r{k}.r{j},{}); printf(\" %llu\",(unsigned long long)b{k}_{j});",size(t)).unwrap();
        }
        c.push_str("}\n");
        writeln!(c,"HirselCoreCauseCell *chain=hirsel_core_causes_take(ctx); if({status}) for(const HirselCoreCauseCell *p=chain;p;p=hirsel_core_cause_next(p)) printf(\" %s\",causes[hirsel_core_cause_value(p)->kind]); printf(\"|%llu|\",(unsigned long long)(initial-hirsel_core_test_available(ctx))); for(size_t i=0;i<traces;i++) printf(\" %llu\",(unsigned long long)trace[i]); printf(\"|%zu|\",hirsel_core_test_attempts(ctx)); for(const HirselCoreCauseCell *p=chain;p;p=hirsel_core_cause_next(p)) if(hirsel_core_cause_value(p)->kind==4) {{ const HirselCoreStringMessage *msg=(const HirselCoreStringMessage *)hirsel_core_cause_value(p)->message; for(uint64_t i=0;i<msg->len;i++) printf(\"%02x\",((const unsigned char *)msg->data)[i]); printf(\" \"); }} printf(\"|\"); for(size_t j=0;j<arc_count;j++) printf(\" %u:%llu\",arc_events[j].kind,(unsigned long long)arc_events[j].value); printf(\"\\n\"); hirsel_core_causes_drop(chain); return 0; }}").unwrap();
    }
    invocations.push_str("return 0;}\n");
    c.push_str(&invocations);
    std::fs::write(dir.join("test.c"), c).unwrap();
    let prefix =
        PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").expect("pinned LLVM prefix"));
    let runtime = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(
            std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("CoreIR native runtime test input"),
        );
    output(
        Command::new(prefix.join("bin/clang"))
            .arg(format!("--ld-path={}", prefix.join("bin/ld.lld").display()))
            .args(["-std=c11", "-O0", "-no-pie", "-I"])
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hirsel-core-rt/include"))
            .arg(dir.join("test.c"))
            .arg(dir.join("core.o"))
            .arg(runtime)
            .args(["-lgcc_s", "-lpthread", "-ldl", "-lm", "-lrt", "-lutil"])
            .arg("-o")
            .arg(dir.join("test")),
    );
    output(&mut Command::new(dir.join("test")))
        .lines()
        .map(|line| {
            let mut parts = line.split('|');
            let mut values = parts.next().unwrap().split_whitespace();
            let status = values.next().unwrap();
            let outcome = if status == "R" {
                eval::Outcome::Returned(values.map(|v| v.parse().unwrap()).collect())
            } else {
                eval::Outcome::Failed(values.map(str::to_string).collect())
            };
            NativeReport {
                outcome,
                used: parts.next().unwrap().parse().unwrap(),
                trace: parts
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .map(|s| s.parse().unwrap())
                    .collect(),
                admissions: parts.next().unwrap().parse().unwrap(),
                messages: parts
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .map(str::to_string)
                    .collect(),
                arc_events: parts
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .map(|pair| {
                        let (kind, value) = pair.split_once(':').unwrap();
                        (kind.parse().unwrap(), value.parse().unwrap())
                    })
                    .collect(),
            }
        })
        .collect()
}
fn agree(module: &Module, cases: &[(FuncId, Vec<u64>)]) {
    let cases = cases
        .iter()
        .map(|(function, args)| Case {
            function: *function,
            args: args.clone(),
            limit: u64::MAX,
            injection: None,
        })
        .collect::<Vec<_>>();
    agree_cases(module, &cases, false);
}
fn agree_cases(module: &Module, cases: &[Case], memory: bool) {
    let got = execute(module, cases);
    assert_eq!(got.len(), cases.len());
    for (case, got) in cases.iter().zip(got) {
        let expected = eval::run_injected(
            module,
            case.function,
            &case.args,
            case.limit,
            case.injection,
        );
        assert_eq!(
            got.outcome, expected.outcome,
            "f{}({:?})",
            case.function.0, case.args
        );
        if memory {
            assert_eq!(got.used, expected.used, "used");
            assert_eq!(got.trace, expected.trace, "trace");
            assert_eq!(got.admissions, expected.admissions, "admissions");
        }
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
    let module = text::parse(include_str!("../../../hirsel-core-ir/samples/arith.core")).unwrap();

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

fn sample_cases(module: &Module, source: &str) -> Vec<Case> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim().strip_prefix(';')?.trim().strip_prefix("run")?;
            let (limit, rest) = line.split_once(':')?;
            let limit = limit
                .trim()
                .strip_prefix("limit ")
                .map(|n| n.parse().unwrap())
                .unwrap_or(u64::MAX);
            let (f, args) = rest.trim().strip_prefix('f')?.split_once('(')?;
            let (args, _) = args.split_once(')')?;
            let function = FuncId(f.parse().unwrap());
            let sig = &module.signatures[module.functions[function.index()].signature.index()];
            let args = args
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .zip(&sig.params)
                .map(|(v, t)| {
                    let Param::Value(t) = t else {
                        panic!("sample destination")
                    };
                    eval::scalar(*t, v.trim())
                })
                .collect();
            Some(Case {
                function,
                args,
                limit,
                injection: None,
            })
        })
        .collect()
}
#[test]
fn every_whole_v0_sample_matches_the_evaluator() {
    let mut count = 0;
    for source in [
        include_str!("../../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../../hirsel-core-ir/samples/compare-convert.core"),
        include_str!("../../../hirsel-core-ir/samples/control.core"),
        include_str!("../../../hirsel-core-ir/samples/slots.core"),
        include_str!("../../../hirsel-core-ir/samples/calls.core"),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let cases = sample_cases(&module, source);
        assert!(!cases.is_empty());
        count += cases.len();
        agree_cases(&module, &cases, false);
    }
    println!("whole-v0 sample executions: {count}");
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
    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap();
        let runs = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("// expect: run "))
            .collect::<Vec<_>>();
        if runs.is_empty() {
            continue;
        }
        let (module, names) = front::lower(&source, &root.join("core"), "main")
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let cases = runs
            .iter()
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
        println!("front {}: {} native runs", path.display(), cases.len());
        count += cases.len();
        agree_cases(&module, &cases, false);
    }
    assert!(count > 0);
    println!("front native executions: {count}");
}

#[test]
fn every_whole_v1_memory_sample_matches_the_evaluator() {
    let mut count = 0;
    for source in [
        include_str!("../../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../../hirsel-core-ir/samples/variants.core"),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let cases = sample_cases(&module, source);
        assert!(!cases.is_empty());
        count += cases.len();
        agree_cases(&module, &cases, true);
    }
    println!("whole-v1 memory sample executions: {count}");
}

#[test]
fn every_guarded_admission_refusal_matches_the_evaluator() {
    let admission = include_str!("../../../hirsel-core-ir/samples/admission.core");
    let resource = include_str!("../../tests/core_runtime/injection.core");
    let mut executions = 0;
    for source in [admission, resource] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let baselines = if source == resource {
            vec![Case {
                function: FuncId(0),
                args: vec![],
                limit: 256,
                injection: None,
            }]
        } else {
            sample_cases(&module, source)
                .into_iter()
                .filter(|c| c.function.0 <= 1)
                .collect()
        };
        let mut cases = Vec::new();
        for case in baselines {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert!(expected.admissions > 0);
            for nth in 1..=expected.admissions {
                for later in [false, true] {
                    cases.push(Case {
                        function: case.function,
                        args: case.args.clone(),
                        limit: case.limit,
                        injection: Some((nth, later)),
                    });
                }
            }
        }
        executions += cases.len();
        agree_cases(&module, &cases, true);
    }
    println!("guarded injected executions: {executions}");
}

#[test]
fn every_compact_sequence_sample_matches_the_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/sequences.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let cases = sample_cases(&module, source);
    assert_eq!(cases.len(), 11);
    agree_cases(&module, &cases, true);
}

#[test]
fn owning_callable_and_failure_message_transfer_match_the_evaluator() {
    let source = include_str!("../../tests/core_runtime/callable.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = vec![
        Case {
            function: FuncId(0),
            args: vec![7, 0],
            limit: 256,
            injection: None,
        },
        Case {
            function: FuncId(0),
            args: vec![7, 1],
            limit: 256,
            injection: None,
        },
        Case {
            function: FuncId(3),
            args: vec![],
            limit: 256,
            injection: None,
        },
    ];
    for later in [false, true] {
        cases.push(Case {
            function: FuncId(0),
            args: vec![7, 1],
            limit: 256,
            injection: Some((1, later)),
        });
    }
    let got = execute(&module, &cases);
    for (case, got) in cases.iter().zip(got) {
        let expected = eval::run_injected(
            &module,
            case.function,
            &case.args,
            case.limit,
            case.injection,
        );
        assert_eq!(got.outcome, expected.outcome);
        assert_eq!(got.used, expected.used);
        assert_eq!(got.admissions, expected.admissions);
        assert_eq!(
            got.messages,
            if case.function.0 == 3 {
                vec!["686921".to_string()]
            } else {
                vec![]
            }
        );
    }
    let source = source.replace("  raise fail a0", "  drop a0 t1\n  return");
    let module = text::parse(&source).unwrap();
    agree_cases(
        &module,
        &[Case {
            function: FuncId(3),
            args: vec![],
            limit: 256,
            injection: None,
        }],
        true,
    );
}

#[test]
fn counted_sequences_destroy_deep_chains_with_constant_stack() {
    let source = include_str!("../../tests/core_runtime/counted_sequence.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let cases = [0, 32, 50_000].map(|n| Case {
        function: FuncId(0),
        args: vec![n],
        limit: u64::MAX,
        injection: None,
    });
    let got = execute(&module, &cases);
    for (case, got) in cases.iter().zip(got) {
        let n = case.args[0];
        assert_eq!(got.outcome, eval::Outcome::Returned(vec![n]));
        assert_eq!(got.used, 0);
        assert_eq!(got.admissions, 1 + 2 * n as usize);
        if n <= 32 {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert_eq!(got.outcome, expected.outcome);
            assert_eq!(got.used, expected.used);
        }
    }
}

#[test]
fn text_fixed_span_and_pointer_views_use_packed_unaligned_bytes() {
    let source = include_str!("../../tests/core_runtime/text_span.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let expected = eval::run(&module, FuncId(0), &[], 256);
    assert_eq!(
        expected.outcome,
        eval::Outcome::Returned(vec![3, 2, 22, 2_189_672])
    );
    agree_cases(
        &module,
        &[Case {
            function: FuncId(0),
            args: vec![],
            limit: 256,
            injection: None,
        }],
        true,
    );
}

#[test]
fn counted_strings_release_their_dynamic_byte_buffer() {
    let source = include_str!("../../tests/core_runtime/callable.core")
        .replace("  addr a6 = v4 byte v1 : u8", "  addr a6 = v4 byte v1 : u8\n  addr a7 = v10 byte v2 : t1")
        .replace("  raise fail a0", "  v9: u64 = const 32\n  v10: ptr, v11: u64 = alloc counted rc v9 align 8 charge v9\n  address a7\n  v12: u64 = const 24\n  copy a7, a0, v12\n  release v10 rc t1\n  return");
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    agree_cases(
        &module,
        &[Case {
            function: FuncId(3),
            args: vec![],
            limit: 256,
            injection: None,
        }],
        true,
    );
}

#[test]
fn address_shapes_do_not_depend_on_block_emission_order() {
    let source = r#"core v1
type t0 = own "Record" { value: u64 }
sig g0 = (u64) -> (u64)
inst n0 = llvm@1/addresses/order [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 : t0
  addr a1 = a0 field 0 : u64
b0(v0: u64):
  live s0
  jump b2
b1:
  address a1
  store a1, v0
  v1: u64 = read a1
  dead s0
  return v1
b2:
  address a0
  jump b1
}
"#;
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    agree(&module, &[(FuncId(0), vec![123])]);
}

#[test]
fn aggregate_destination_precedes_parameters_and_returns_failure_status() {
    let source = r#"core v1
type t0 = own "Pair" { left: u64, right: u64 }
sig g0 = (u64) -> (u64, u64)
sig g1 = (u64, u64) -> () dest t0
inst n0 = llvm@1/destination/run [] []
inst n1 = llvm@1/destination/write [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 : t0
  addr a1 = s0 field 0 : u64
  addr a2 = s0 field 1 : u64
b0(v0: u64):
  live s0
  address a0
  address a1
  address a2
  v1: u64 = const 9
  call f1 g1 (&a0, v0, v1)
  v2: u64 = read a1
  v3: u64 = read a2
  dead s0
  return v2, v3
}
fn f1 = n1 g1 {
  addr a0 = v0 : t0
  addr a1 = a0 field 0 : u64
  addr a2 = a0 field 1 : u64
b0(v0: ptr, v1: u64, v2: u64):
  v3: u64 = const 0
  v4: bool = eq v1, v3
  branch v4, b1, b2
b1:
  raise unwrap
b2:
  address a0
  address a1
  address a2
  store a1, v1
  store a2, v2
  return
}
"#;
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    agree(&module, &[(FuncId(0), vec![123]), (FuncId(0), vec![0])]);
}

#[test]
fn every_memory_sample_admission_refusal_matches_the_evaluator() {
    let mut executions = 0;
    for source in [
        include_str!("../../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../../hirsel-core-ir/samples/variants.core"),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let mut cases = Vec::new();
        for case in sample_cases(&module, source) {
            let baseline = eval::run(&module, case.function, &case.args, case.limit);
            for nth in 1..=baseline.admissions {
                for later in [false, true] {
                    cases.push(Case {
                        function: case.function,
                        args: case.args.clone(),
                        limit: case.limit,
                        injection: Some((nth, later)),
                    });
                }
            }
            cases.push(case);
        }
        executions += cases.len();
        agree_cases(&module, &cases, true);
    }
    // The HostFile cleanup sample adds one baseline and two admission refusals.
    assert_eq!(executions, 147);
    println!("whole memory sample executions including every admission refusal: {executions}");
}

#[test]
fn infallible_direct_calls_keep_the_external_and_indirect_failure_abi() {
    let module = text::parse(include_str!("../../tests/core_runtime/infallible.core")).unwrap();
    wf::module(&module).unwrap();
    assert_eq!(
        failure::Failures::new(&module).may_fail,
        [false, false, true, true, true]
    );
    let ir = emit_ir(&module).unwrap();
    assert!(ir.contains(&format!(
        "call i64 @{}.infallible",
        symbol(&module, FuncId(1))
    )));
    let mut unknown = module.clone();
    unknown.functions[1].body = None;
    assert!(failure::Failures::new(&unknown).may_fail.iter().all(|f| *f));
    let mut cases = vec![];
    for f in [0, 1, 4] {
        for n in [0, 5, 100] {
            cases.push((FuncId(f), vec![n]));
        }
    }
    for f in [2, 3] {
        for n in [0, 1, 63, 64, u64::MAX] {
            cases.push((FuncId(f), vec![7, n]));
        }
    }
    agree(&module, &cases);
}

#[test]
fn maps_clean_live_entries_in_order_and_continue_after_both_failures() {
    let module = text::parse(include_str!("../../tests/core_runtime/map.core")).unwrap();
    wf::module(&module).unwrap();
    let cases = [
        (FuncId(2), vec![0]),
        (FuncId(2), vec![1]),
        (FuncId(3), vec![]),
        (FuncId(4), vec![]),
    ]
    .map(|(function, args)| Case {
        function,
        args,
        limit: 1024,
        injection: None,
    });
    let reports = execute(&module, &cases);
    for (i, got) in reports.iter().enumerate() {
        assert_eq!(
            got.outcome,
            if i == 1 {
                eval::Outcome::Failed(vec!["Bounds".into(), "Unwrap".into()])
            } else {
                eval::Outcome::Returned(vec![])
            }
        );
        assert_eq!(got.used, if i == 1 { 64 } else { 0 });
        assert_eq!(
            got.trace,
            match i {
                0 => vec![11, 12, 30, 33],
                1 => vec![11, 12, 31, 32],
                _ => vec![],
            }
        );
        assert_eq!(
            got.admissions,
            match i {
                0 | 1 => 5,
                2 => 1,
                _ => 0,
            }
        );
    }
}

#[test]
fn counted_maps_skip_removed_entries_and_destroy_deep_chains_with_constant_stack() {
    let module = text::parse(include_str!("../../tests/core_runtime/counted_map.core")).unwrap();
    wf::module(&module).unwrap();
    let cases = [0, 32, 50_000].map(|n| Case {
        function: FuncId(0),
        args: vec![n],
        limit: u64::MAX,
        injection: None,
    });
    for (case, got) in cases.iter().zip(execute(&module, &cases)) {
        assert_eq!(got.outcome, eval::Outcome::Returned(case.args.clone()));
        assert_eq!(got.used, 0);
        assert_eq!(got.admissions, 1 + 2 * case.args[0] as usize);
    }
}

#[path = "fast_tests.rs"]
mod fast_tests;
