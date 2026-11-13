#[path = "../../examples/support/front.rs"]
mod front;
use super::*;
use hirsel_core_ir::{eval, text, wf};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "../../../hirsel-core-rt/testdata/host_memory.rs"]
mod host_memory;

#[test]
fn loan_facts_and_diagnostic_off_preserve_abi_and_legal_aliases() {
    let module = text::parse(include_str!("../../tests/core_runtime/loans.core")).unwrap();
    wf::module(&module).unwrap();
    let on = emit_ir_with_facts(&module, false, true).unwrap();
    let off = emit_ir_with_facts(&module, false, false).unwrap();
    assert!(on.contains("ptr noalias readonly %p0"));
    assert!(on.contains("ptr noalias %dest"));
    for function in [2, 3] {
        let name = symbol(&module, FuncId(function));
        let definition = on
            .lines()
            .find(|l| l.starts_with("define ") && l.contains(&format!("@{name}(")))
            .unwrap();
        assert!(definition.contains("ptr noalias %p0"));
    }
    for fact in [
        "noalias",
        "readonly",
        "nocapture",
        "captures(",
        "invariant",
        "!range",
    ] {
        assert!(!off.contains(fact), "fact retained: {fact}");
    }
    assert!(off.contains("sret("), "ABI return convention stays intact");
    for source in [
        include_str!("../../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../../hirsel-core-ir/samples/strings.core"),
    ] {
        let module = text::parse(source).unwrap();
        let off = emit_ir_with_facts(&module, false, false).unwrap();
        for fact in ["noalias", "readonly", "invariant", "memory(", "nounwind"] {
            assert!(!off.contains(fact), "fact retained: {fact}");
        }
    }
    // Views may overlap, and a copyable take can alias another view. A view
    // can escape through a returned pointer; mut writes remain observable.
    agree(&module, &[(FuncId(0), vec![])]);
}

#[test]
fn scalar_checked_add_keeps_wrapped_results_and_failure_edges() {
    let mut source = String::from("core v1\n");
    for (i, t) in ["u8", "i8"].iter().enumerate() {
        writeln!(source, "sig g{i} = ({t}, {t}) -> ({t})").unwrap();
    }
    source.push_str("sig g2 = (u8, u8) -> (u8, bool)\n");
    for i in 0..7 {
        writeln!(source, "inst n{i} = llvm@1/checked/op{i} [] []").unwrap();
    }
    for (i, op) in ["add", "sub", "mul"].iter().cycle().take(6).enumerate() {
        let t = if i < 3 { "u8" } else { "i8" };
        writeln!(source, "fn f{i} = n{i} g{} {{\nb0(v0: {t}, v1: {t}):\n  v2: {t} = {op} v0, v1\n  return v2\n}}", i / 3).unwrap();
    }
    source.push_str("fn f6 = n6 g2 {\nb0(v0: u8, v1: u8):\n  v2: u8, v3: bool = checked_add v0, v1\n  return v2, v3\n}\n");
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for function in 0..7 {
        for a in [0, 1, 127, 128, 255] {
            for b in [0, 1, 127, 128, 255] {
                cases.push((FuncId(function), vec![a, b]));
            }
        }
    }
    // The explicit checked row returns wrapped bits even on overflow. Binary
    // rows instead leave via Overflow before the result can be used.
    agree(&module, &cases);
}

#[test]
fn native_allocator_refusals_are_reported_run_stops() {
    host_memory::check(
        |module| emit_object(module, Optimization::Speed).unwrap(),
        symbol,
    );
}

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
    peak_reserved: Option<u64>,
}
fn has_hostfile_field(shape: &hirsel_core_layout::Shape) -> bool {
    shape.fields(None).is_some_and(|fields| {
        fields.iter().any(|field| {
            field.ty == hirsel_core_layout::FieldTy::Handle(hirsel_core_layout::Handle::HostFile)
        })
    })
}
fn execute(module: &Module, cases: &[Case]) -> Vec<NativeReport> {
    execute_with_bindings(module, cases, "")
}
fn execute_with_bindings(module: &Module, cases: &[Case], bindings: &str) -> Vec<NativeReport> {
    execute_with_configuration(module, cases, bindings, "")
}
fn execute_with_configuration(
    module: &Module,
    cases: &[Case],
    bindings: &str,
    configuration: &str,
) -> Vec<NativeReport> {
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
    let observing = std::env::var_os("HIRSEL_TEST_CHARGE_OBSERVATION").is_some();
    let ir = emit_ir_with_charge_observation(module, observing).unwrap();
    let context = Context::new();
    let llvm = LlvmModule::parse(&context, "test", &ir).unwrap();
    llvm.verify().unwrap_or_else(|e| panic!("{e}\n{ir}"));
    let bytes = emit_object_with_charge_observation(module, Optimization::None, observing).unwrap();
    std::fs::write(dir.join("core.o"), bytes).unwrap();
    let mut c = String::from(
        "#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n#include <assert.h>\n#include \"hirsel_core_rt.h\"\nextern uint64_t hirsel_core_test_available(const HirselCoreContext *);\nextern HirselCoreContext *hirsel_core_test_context_new(uint64_t);\nextern void hirsel_core_test_inject(HirselCoreContext *,size_t,bool);\nextern size_t hirsel_core_test_attempts(const HirselCoreContext *);\nstatic uint64_t trace[4096]; static size_t traces;\nvoid hirsel_core_test_drop_trace(uint64_t n){assert(traces<4096);trace[traces++]=n;}\nstatic float f32(uint32_t b){float x;memcpy(&x,&b,4);return x;}\nstatic double f64(uint64_t b){double x;memcpy(&x,&b,8);return x;}\nstatic const char *causes[]={\"Overflow\",\"Bounds\",\"Division\",\"Unwrap\",\"Fail\",\"Cancelled\",\"Capacity\"};\n",
    );
    if observing {
        c.push_str("extern void *hirsel_core_test_observe_begin(const HirselCoreContext *); extern uint64_t hirsel_core_test_observe_end(void *);\n");
    }
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
                    Param::Address { .. } => "void *",
                }))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .unwrap();
    }
    c.push_str(bindings);
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
        "int main(void){ HirselCoreProcessConfig cfg={.root_limit=UINT64_MAX,.diagnostic=diagnostic,.scheduler={.event=record_event}CONFIGURATION};\n",
    ).replace("CONFIGURATION", configuration);

    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    for (k, case) in cases.iter().enumerate() {
        let f = case.function;
        let sig = &module.signatures[module.functions[f.index()].signature.index()];
        let destination = sig
            .dest
            .map(|ty| {
                let hirsel_core_layout::Layout::Fixed(layout) = &layouts[ty.index()].layout else {
                    panic!("fixed entry destination");
                };
                format!(
                    "_Alignas({}) uint8_t destination{k}[{}];",
                    layout.align,
                    layout.size.max(1)
                )
            })
            .unwrap_or_default();
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
        let context = if sig.dest.is_some() {
            format!("ctx, destination{k}")
        } else {
            "ctx".into()
        };
        let args = if args.is_empty() {
            context
        } else {
            format!("{context}, {args}")
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
        let begin = if observing {
            "void *observation=hirsel_core_test_observe_begin(ctx);"
        } else {
            ""
        };
        writeln!(c,"static uint8_t case{k}(HirselCoreContext *ctx,const HirselCoreProcessConfig *cfg,HirselCoreMainResult *result) {{ (void)cfg; (void)result; hirsel_core_test_reset_limit(ctx,UINT64_C({limit})); {files} traces=0; arc_count=0; {begin} {inject} {destination} uint64_t initial=hirsel_core_test_available(ctx); {ret} r{k} = {}({args});",symbol(module,f)).unwrap();
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
        if observing {
            c.push_str("uint64_t peak=hirsel_core_test_observe_end(observation);\n");
        }
        if observing {
            c.push_str("printf(\"%llu|\",(unsigned long long)peak);\n");
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
        writeln!(c,"HirselCoreCauseCell *chain=hirsel_core_causes_take(ctx); if({status}) for(const HirselCoreCauseCell *p=chain;p;p=hirsel_core_cause_next(p)) printf(\" %s\",causes[hirsel_core_cause_kind(hirsel_core_cause_value(p))]); printf(\"|%llu|\",(unsigned long long)(initial-hirsel_core_test_available(ctx))); for(size_t i=0;i<traces;i++) printf(\" %llu\",(unsigned long long)trace[i]); printf(\"|%zu|\",hirsel_core_test_attempts(ctx)); for(const HirselCoreCauseCell *p=chain;p;p=hirsel_core_cause_next(p)) if(hirsel_core_cause_kind(hirsel_core_cause_value(p))==4) {{ const HirselCoreStringMessage *msg=&hirsel_core_cause_value(p)->message; for(uint64_t i=0;i<msg->len;i++) printf(\"%02x\",((const unsigned char *)msg->data)[i]); printf(\" \"); }} printf(\"|\"); for(size_t j=0;j<arc_count;j++) printf(\" %u:%llu\",arc_events[j].kind,(unsigned long long)arc_events[j].value); printf(\"\\n\"); hirsel_core_causes_drop(chain); return 0; }}").unwrap();
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
            let peak_reserved = observing.then(|| parts.next().unwrap().parse().unwrap());
            let mut values = parts.next().unwrap().split_whitespace();
            let status = values.next().unwrap();
            let outcome = if status == "R" {
                eval::Outcome::Returned(values.map(|v| v.parse().unwrap()).collect())
            } else {
                eval::Outcome::Failed(values.map(str::to_string).collect())
            };
            NativeReport {
                outcome,
                peak_reserved,
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
        if let Some(peak) = got.peak_reserved {
            assert_eq!(peak, expected.peak_reserved, "peak reservation");
        }
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
    assert_eq!(cases.len(), 32);
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
            let (module, names) = front::lower(&source, &root.join("../core"), "main")
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let cases = runs
                .iter()
                .map(|run| {
                    let (call, _) = run.split_once(" = ").unwrap();
                    let (name, args) = call.strip_suffix(')').unwrap().split_once('(').unwrap();
                    let function = names[name];
                    let sig =
                        &module.signatures[module.functions[function.index()].signature.index()];
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

#[test]
fn every_whole_v1_memory_sample_matches_the_evaluator() {
    let mut count = 0;
    for source in [
        include_str!("../../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../../hirsel-core-ir/samples/variants.core"),
        include_str!("../../../hirsel-core-ir/samples/stable-keys.core"),
        include_str!("../../../hirsel-core-ir/samples/kernel.core"),
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

/// Bug: LLVM emits code for a `mark` (FORM v1c), which has no run-time effect, or
/// lets one change what the rest of the body becomes (an empty bound scope it
/// breaks up, a value numbering it shifts).
#[test]
fn marks_emit_nothing() {
    let module = text::parse(include_str!("../../../hirsel-core-ir/samples/kernel.core")).unwrap();
    let stripped = wf::without_marks(&module);
    assert_ne!(stripped, module, "the sample has marks");
    for opt in [Optimization::None, Optimization::Speed] {
        assert!(
            emitted_ir(&module, opt).unwrap() == emitted_ir(&stripped, opt).unwrap(),
            "a mark changed the IR at {opt:?}"
        );
    }
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
fn terminal_results_forward_without_abandoning_cleanup_or_local_arguments() {
    let module = text::parse(include_str!("../../tests/core_runtime/forward.core")).unwrap();
    wf::module(&module).unwrap();
    let recipes = hirsel_core_layout::compute(&module.types).unwrap();
    let failures = failure::Failures::new(&module);
    let ending = |f: usize| {
        let function = &module.functions[f];
        let body = function.body.as_ref().unwrap();
        forward::Forwarding::new(body).ending(
            &module,
            &recipes,
            &module.signatures[function.signature.index()],
            &failures.constants[f],
            0,
        )
    };
    assert!(ending(1).unwrap().frame_safe);
    assert!(!ending(3).unwrap().frame_safe);
    let ir = emit_ir(&module).unwrap();
    assert!(ir.contains(&format!(
        "tail call zeroext i8 @{}(ptr %context, ptr %dest,",
        symbol(&module, FuncId(2))
    )));
    assert!(!ir.contains(&format!(
        "tail call void @{}.infallible",
        symbol(&module, FuncId(4))
    )));
    agree(&module, &[(FuncId(0), vec![123]), (FuncId(0), vec![0])]);

    // FAIL-8/OWN-12: a pending result stays local until its caller's
    // finalizers have succeeded. Even an infallible drop ends forwarding.
    let cleanup = include_str!("../../tests/core_runtime/forward.core")
        .replace("  copy a1, a0, v2", "  drop a0 t0\n  copy a1, a0, v2");
    let cleanup = text::parse(&cleanup).unwrap();
    let body = cleanup.functions[1].body.as_ref().unwrap();
    assert!(
        forward::Forwarding::new(body)
            .ending(
                &cleanup,
                &recipes,
                &cleanup.signatures[1],
                &failures.constants[1],
                0
            )
            .is_none()
    );
    let cleanup =
        text::parse(include_str!("../../../hirsel-core-ir/samples/failure.core")).unwrap();
    let recipes = hirsel_core_layout::compute(&cleanup.types).unwrap();
    let failures = failure::Failures::new(&cleanup);
    let body = cleanup.functions[1].body.as_ref().unwrap();
    assert!(
        forward::Forwarding::new(body)
            .ending(
                &cleanup,
                &recipes,
                &cleanup.signatures[0],
                &failures.constants[1],
                1
            )
            .is_none()
    );
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
    // cleanup.core's f7 runs under limits 64, 100 and 150 make 1, 2 and 3 admissions:
    // each adds its baseline and two refusals per admission (3 + 5 + 7) to 158.
    assert_eq!(executions, 173);
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

#[test]
fn static_capacity_zero_carriers_drop_and_grow_through_runtime_cleanup() {
    let source = include_str!("../../../hirsel-core-ir/samples/strings.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    agree_cases(&module, &sample_cases(&module, source), true);
    let module = text::parse(include_str!(
        "../../../hirsel-cranelift/tests/support/static-carriers.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let cases: Vec<_> = (0..4)
        .flat_map(|f| {
            [0, 5].map(move |n| Case {
                function: FuncId(f),
                args: vec![n],
                limit: 1000,
                injection: None,
            })
        })
        .collect();
    // Map cleanup is not implemented by the evaluator. These expected balances
    // follow the literal and two declared container sizes directly from FORM #48.
    let actual = execute(&module, &cases);
    for (case, got) in cases.iter().zip(actual) {
        assert_eq!(got.outcome, eval::Outcome::Returned(vec![]));
        assert_eq!(got.used, 0);
        assert!(got.trace.is_empty());
        assert_eq!(got.admissions, [0, 1, 1, 2][case.function.index()]);
    }
}

#[test]
fn string_literals_growth_and_byte_order_match_the_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/strings.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let cases = sample_cases(&module, source);
    assert_eq!(cases.len(), 10);
    // Refuse each growth/probe admission without ever freeing the static carrier.
    let mut refusals = Vec::new();
    for case in &cases {
        let baseline = eval::run(&module, case.function, &case.args, case.limit);
        for nth in 1..=baseline.admissions {
            for later in [false, true] {
                refusals.push(Case {
                    function: case.function,
                    args: case.args.clone(),
                    limit: case.limit,
                    injection: Some((nth, later)),
                });
            }
        }
    }
    agree_cases(&module, &refusals, true);
    println!(
        "string sample runs: {}; refusal runs: {}",
        cases.len(),
        refusals.len()
    );
}

#[test]
fn task_custody_samples_and_admission_refusals_match_the_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/custody.core");
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
    println!("task custody executions: {}", cases.len());
    agree_cases(&module, &cases, true);
}

#[test]
fn channel_fifo_clone_full_and_closed_match_the_evaluator() {
    let module = text::parse(include_str!("channel_tests.core")).unwrap();
    wf::module(&module).unwrap();
    let baseline = eval::run(&module, FuncId(0), &[], u64::MAX);
    assert_eq!(baseline.outcome, eval::Outcome::Returned(vec![7, 8, 0, 1]));
    let mut cases = vec![Case {
        function: FuncId(0),
        args: vec![],
        limit: u64::MAX,
        injection: None,
    }];
    for nth in 1..=baseline.admissions {
        for later in [false, true] {
            cases.push(Case {
                function: FuncId(0),
                args: vec![],
                limit: u64::MAX,
                injection: Some((nth, later)),
            });
        }
    }
    println!("channel executions: {}", cases.len());
    agree_cases(&module, &cases, true);
}

#[test]
fn provider_extern_uses_form_results_and_failure_status() {
    let module = text::parse(
        r#"
core v1
sig g0 = (u64) -> (u64)
inst n0 = tests@0/provider/call [] []
inst n1 = tests@0/provider/clock [] []
inst n2 = tests@0/provider/catch [] []
fn f0 = n0 g0 {
b0(v0: u64):
 v1: u64 = call f1 g0 (v0)
 return v1
}
fn f1 = n1 g0 extern
fn f2 = n2 g0 {
b0(v0: u64):
 v1: u64 = call f1 g0 (v0) fail b1
 return v1
b1:
 v2: u64 = const 0
 v3: u64 = call f1 g0 (v2)
 propagate
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    let bindings = r#"
static HirselCoreHostOutcome clock_issue(void *state, HirselCoreHostRequest *request) {
    (void)state; assert(request->operation==10); hirsel_core_test_drop_trace(7+request->handle); HirselCoreHostOutcome out={0};
    if(request->handle) out.status=1; else out.value=1234567;
    return out;
}
static HirselCoreHostOutcome clock_deliver(void *state, HirselCoreHostRequest *request, HirselCoreHostOutcome out) {
    (void)state; assert(request->operation==10); return out;
}
R1 EXTERNAL(void *context, uint64_t mode) {
    HirselCoreProvider provider={.issue=clock_issue,.deliver=clock_deliver};
    HirselCoreHostRequest request={.operation=10,.handle=mode};
    HirselCoreHostOutcome out=hirsel_core_host(context,&provider,&request);
    if(out.status) {
        HirselCoreCause cause=hirsel_core_cause_new(5,0); hirsel_core_raise(context,cause);
        return (R1){.r1=1};
    }
    return (R1){.r0=out.value};
}
"#
    .replace("EXTERNAL", &symbol(&module, FuncId(1)));
    let cases = [(0, 0), (0, 1), (2, 1)].map(|(f, a)| Case {
        function: FuncId(f),
        args: vec![a],
        limit: u64::MAX,
        injection: None,
    });
    let reports = execute_with_bindings(&module, &cases, &bindings);
    assert_eq!(reports[0].outcome, eval::Outcome::Returned(vec![1234567]));
    assert_eq!(
        reports[1].outcome,
        eval::Outcome::Failed(vec!["Cancelled".into()])
    );
    assert_eq!(reports[2].trace, vec![8, 7]);
    assert_eq!(
        reports[2].outcome,
        eval::Outcome::Failed(vec!["Cancelled".into()])
    );
}

struct ProviderCase {
    call: Case,
    argv: Vec<&'static str>,
    outcome: eval::Outcome,
    stdout: Vec<u8>,
}
fn provider_cases() -> Vec<ProviderCase> {
    use eval::Outcome::{Failed, Returned};
    // The eight hand-derived runs in providers.core, in their documented order.
    [
        (
            0,
            vec![0, 0, 1],
            u64::MAX,
            vec!["prog", "hello"],
            Returned(vec![2, 5]),
            b"hello".to_vec(),
        ),
        (
            0,
            vec![0, 0, 2],
            u64::MAX,
            vec!["prog", "hello"],
            Returned(vec![2, 0]),
            vec![],
        ),
        (
            0,
            vec![0, 0, 0],
            u64::MAX,
            vec![],
            Returned(vec![0, 0]),
            vec![],
        ),
        (
            1,
            vec![7],
            1000,
            vec![],
            Returned(vec![3, 2, 105, 2]),
            vec![],
        ),
        (1, vec![7], 4, vec![], Failed(vec!["Unwrap".into()]), vec![]),
        (2, vec![7, 40], 1000, vec![], Returned(vec![2, 3]), vec![]),
        (2, vec![7, 32], 1000, vec![], Returned(vec![2, 3]), vec![]),
        (
            2,
            vec![7, 31],
            1000,
            vec![],
            Failed(vec!["Bounds".into()]),
            vec![],
        ),
    ]
    .into_iter()
    .map(|(f, args, limit, argv, outcome, stdout)| ProviderCase {
        call: Case {
            function: FuncId(f),
            args,
            limit,
            injection: None,
        },
        argv,
        outcome,
        stdout,
    })
    .collect()
}

#[test]
fn provider_samples_match_the_hand_oracle_and_every_closed_row_emits() {
    let module = text::parse(include_str!(
        "../../../hirsel-core-ir/samples/providers.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let context = Context::new();
    LlvmModule::parse(&context, "providers", &emit_ir(&module).unwrap())
        .unwrap()
        .verify()
        .unwrap();
    for case in provider_cases() {
        let expected = eval::run_args(
            &module,
            case.call.function,
            &case.call.args,
            case.call.limit,
            &case.argv,
        );
        assert_eq!(expected.outcome, case.outcome);
        assert_eq!(expected.used, 0);
        assert_eq!(expected.stdout, case.stdout);
        assert!(expected.stderr.is_empty());
    }
    for op in HostOp::ALL {
        let params: &[&str] = match op {
            HostOp::Open => &["u64", "ptr", "u8"],
            HostOp::Close | HostOp::TryLock | HostOp::Unlock | HostOp::WallMs | HostOp::MonoNs => {
                &["u64"]
            }
            _ => &["u64", "ptr"],
        };
        let results: &[&str] = if op.waits() {
            &["u64", "u8", "u32", "u64"]
        } else {
            &["u64"]
        };
        let names = |n: usize| {
            (0..n)
                .map(|i| format!("v{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let typed = |ts: &[&str], offset: usize| {
            ts.iter()
                .enumerate()
                .map(|(i, t)| format!("v{}: {t}", i + offset))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let source = format!(
            "core v1\nsig g0 = ({}) -> ({})\ninst n0 = tests@0/provider/{} [] []\nfn f0 = n0 g0 {{\nb0({}):\n {} = call rt.{} g0 ({})\n return {}\n}}\n",
            params.join(", "),
            results.join(", "),
            op.name(),
            typed(params, 0),
            typed(results, params.len()),
            op.name(),
            names(params.len()),
            (params.len()..params.len() + results.len())
                .map(|i| format!("v{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let module = text::parse(&source).unwrap();
        wf::module(&module).unwrap();
        let ir = emit_ir(&module).unwrap();
        LlvmModule::parse(&context, op.name(), &ir)
            .unwrap()
            .verify()
            .unwrap();
        assert!(ir.contains("sret([32 x i8])"));
        assert_eq!(
            ir.contains("icmp ne i8"),
            op.waits(),
            "{} status",
            op.name()
        );
    }
}

#[test]
fn native_provider_samples_and_refusals_match_the_hand_oracle() {
    let module = text::parse(include_str!(
        "../../../hirsel-core-ir/samples/providers.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let mut executions = 0;
    for case in provider_cases() {
        let baseline = eval::run_args(
            &module,
            case.call.function,
            &case.call.args,
            case.call.limit,
            &case.argv,
        );
        let mut injections = vec![None];
        for nth in 1..=baseline.admissions {
            injections.extend([Some((nth, false)), Some((nth, true))]);
        }
        for injection in injections {
            let expected = if injection.is_none() {
                eval::run_args(
                    &module,
                    case.call.function,
                    &case.call.args,
                    case.call.limit,
                    &case.argv,
                )
            } else {
                assert!(case.argv.is_empty());
                eval::run_injected(
                    &module,
                    case.call.function,
                    &case.call.args,
                    case.call.limit,
                    injection,
                )
            };
            let mut call = Case {
                function: case.call.function,
                args: case.call.args.clone(),
                limit: case.call.limit,
                injection,
            };
            // World's mock handles are zero; real capabilities are configured indices.
            if call.function == FuncId(0) {
                call.args[0] = 2;
                call.args[1] = 1;
            }
            let configuration = format!(
                ",.args=scripted_args,.args_len={},.capabilities=scripted_grants,.capabilities_len=7",
                case.argv.len()
            );
            let reports = execute_with_configuration(
                &module,
                &[call],
                include_str!("provider_tests.c"),
                &configuration,
            );
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].outcome, expected.outcome);
            assert_eq!(reports[0].used, expected.used);
            assert_eq!(reports[0].admissions, expected.admissions);
            executions += 1;
        }
    }
    println!("native provider sample/refusal executions: {executions}");
}

#[test]
fn provider_error_payload_and_nonwaiting_clocks_use_the_c_abi() {
    let module = text::parse(include_str!("provider_abi_tests.core")).unwrap();
    wf::module(&module).unwrap();
    let cases = [
        Case {
            function: FuncId(0),
            args: vec![7],
            limit: 1000,
            injection: None,
        },
        Case {
            function: FuncId(1),
            args: vec![3],
            limit: 1000,
            injection: None,
        },
    ];
    let reports = execute_with_configuration(
        &module,
        &cases,
        include_str!("provider_tests.c"),
        ",.capabilities=scripted_grants,.capabilities_len=7",
    );
    assert_eq!(
        reports[0].outcome,
        eval::Outcome::Returned(vec![0, 5, 0xdecafbad, 0])
    );
    assert_eq!(reports[1].outcome, eval::Outcome::Returned(vec![0, 1]));
    for report in reports {
        assert_eq!(report.used, 0);
        assert_eq!(report.admissions, 0);
    }
}

#[test]
fn joined_cause_views_use_core_layout() {
    let module = text::parse(include_str!("../../../hirsel-core-ir/samples/custody.core")).unwrap();
    let actual = execute(
        &module,
        &[Case {
            function: FuncId(6),
            args: vec![],
            limit: 1000,
            injection: None,
        }],
    );
    assert_eq!(actual[0].outcome, eval::Outcome::Returned(vec![1, 1, 4, 4]));
    assert_eq!(actual[0].used, 0);
    assert_eq!(actual[0].trace, vec![7]);
}

#[test]
fn lazy_component_preserves_exact_core_objects() {
    for source in [
        include_str!("../../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../../hirsel-core-ir/samples/strings.core"),
        include_str!("../../../hirsel-core-ir/samples/custody.core"),
    ] {
        let module = text::parse(source).unwrap();
        for speed in [false, true] {
            let direct = emit_object(
                &module,
                if speed {
                    Optimization::Speed
                } else {
                    Optimization::None
                },
            )
            .unwrap();
            let lazy = hirsel_llvm_lazy::compile_ir(&emit_ir(&module).unwrap(), speed).unwrap();
            assert_eq!(lazy, direct, "identical LLVM objects, speed={speed}");
        }
    }
}

#[test]
fn dynamic_overflow_and_local_like_admissions_keep_exact_refusal_counts() {
    let module = text::parse(include_str!(
        "../../../hirsel-cranelift/tests/support/inline-admission.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let mut cases = vec![];
    for function in 0..3 {
        for charge in [0, 8, 40, 72, u64::MAX] {
            for overflow in [0, 1] {
                let args = vec![charge, overflow];
                let baseline = eval::run(&module, FuncId(function), &args, 64);
                for nth in 1..=baseline.admissions {
                    for later in [false, true] {
                        cases.push(Case {
                            function: FuncId(function),
                            args: args.clone(),
                            limit: 64,
                            injection: Some((nth, later)),
                        });
                    }
                }
                cases.push(Case {
                    function: FuncId(function),
                    args,
                    limit: 64,
                    injection: None,
                });
            }
        }
    }
    println!("ordinary admission cases: {}", cases.len());
    agree_cases(&module, &cases, true);
    // Null Like is the native ABI's Current spelling; the evaluator expects a
    // real block for Like. Its one admission and room are derived independently.
    let mut null_cases = vec![];
    for charge in [0, 8, 40, 72, u64::MAX] {
        for overflow in [0, 1] {
            for injection in [None, Some((1, false)), Some((1, true))] {
                null_cases.push(Case {
                    function: FuncId(3),
                    args: vec![charge, overflow],
                    limit: 64,
                    injection,
                });
            }
        }
    }
    let actual = execute(&module, &null_cases);
    for (case, got) in null_cases.iter().zip(actual) {
        let success = case.args[1] == 0 && case.args[0] <= 64 && case.injection.is_none();
        assert_eq!(
            got.outcome,
            eval::Outcome::Returned(vec![u64::from(success), if success { 0 } else { 64 }])
        );
        assert_eq!(got.used, 0);
        assert_eq!(got.admissions, 1);
        assert!(got.trace.is_empty());
    }
    println!("null-Like native admission cases: {}", null_cases.len());
}
