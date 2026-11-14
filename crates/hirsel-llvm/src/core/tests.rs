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
fn cleared_scalar_carriers_do_not_drop_saved_storage_but_keep_finalizers() {
    let module = text::parse(include_str!(
        "../../tests/core_runtime/cleared-carrier.core"
    ))
    .unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for value in [0, 7, 255] {
        for limit in [0, 8] {
            cases.push(Case {
                function: FuncId(1),
                args: vec![value],
                limit,
                injection: None,
            });
        }
    }
    cases.push(Case {
        function: FuncId(1),
        args: vec![7],
        limit: 8,
        injection: Some((1, false)),
    });
    cases.push(Case {
        function: FuncId(2),
        args: vec![],
        limit: 8,
        injection: None,
    });
    for optimization in [Optimization::None, Optimization::Speed] {
        let got = execute_optimized(&module, &cases, "", "", optimization);
        for (case, actual) in cases.iter().zip(got) {
            let expected = eval::run_injected(
                &module,
                case.function,
                &case.args,
                case.limit,
                case.injection,
            );
            assert_eq!(actual.outcome, expected.outcome);
            assert_eq!(actual.used, expected.used);
            assert_eq!(actual.admissions, expected.admissions);
        }
    }
}

#[test]
fn scalar_growth_shadow_copies_back_before_success_failure_and_forwarded_return() {
    let source = include_str!("../../tests/core_runtime/carrier-shadow.core");
    let nonterminal = " v2: u64 = const 8\n address a1\n v3: u64 = read a1\n store a1, v3\n";
    for terminal in [false, true] {
        let source = if terminal {
            source.replace(nonterminal, " v2: u64 = const 8\n")
        } else {
            source.into()
        };
        for blind in [true, false] {
            let source = if blind {
                source.clone()
            } else {
                source.replace(
                    " v5: u32 = const 0",
                    " v13: bool = eq v0, v0\n v5: u32 = const 0",
                )
            };
            let module = text::parse(&source).unwrap();
            wf::module(&module).unwrap();
            let facts = hirsel_core_ir::roles::Facts::new(&module);
            assert_eq!(facts.place_blind(FuncId(1), 0), blind);
            assert!(
                !facts.sealed(FuncId(1), 0),
                "the carrier really copies between slots"
            );
            let records = facts.record(&module);
            let mut declaration = module.clone();
            declaration.functions[1].body = None;
            let no_record =
                emit_ir_relocatable_for_target(&declaration, Target::X86_64Linux, false, false)
                    .unwrap();
            assert!(!no_record.contains("call void @llvm.lifetime.start.p0"));
            let installed = hirsel_core_ir::roles::Facts::program(&[&declaration], &records)
                .pop()
                .unwrap();
            let restored = emit_ir_relocatable_with_role_facts_for_target(
                &declaration,
                Target::X86_64Linux,
                false,
                false,
                &installed,
            )
            .unwrap();
            assert_eq!(
                restored.contains("call void @llvm.lifetime.start.p0"),
                blind
            );
            let ir = emit_ir_with_facts(&module, false, true).unwrap();
            assert_eq!(
                ir.contains("call void @llvm.lifetime.start.p0"),
                blind,
                "{ir}"
            );
            let off = emit_ir_with_facts(&module, false, false).unwrap();
            assert!(!off.contains("call void @llvm.lifetime.start.p0"));
            let cases = [0, 1].map(|mode| Case {
                function: FuncId(2),
                args: vec![mode],
                limit: u64::MAX,
                injection: None,
            });
            for opt in [Optimization::None, Optimization::Speed] {
                let reports = execute_optimized(&module, &cases, "", "", opt);
                for (case, report) in cases.iter().zip(reports) {
                    assert_eq!(
                        report.outcome,
                        eval::run(&module, case.function, &case.args, case.limit).outcome
                    );
                    if case.args[0] == 0 {
                        assert_eq!(report.outcome, eval::Outcome::Returned(vec![7, 0]));
                    }
                    assert_eq!(report.used, 0);
                }
                let context = crate::context::Context::new();
                let direct = crate::context::Module::parse(&context, "hirsel-core", &ir).unwrap();
                let machine = crate::context::TargetMachine::new(
                    &hirsel_lir::layout_types::TargetSpec::x86_64_unknown_linux_gnu(),
                    matches!(opt, Optimization::Speed),
                )
                .unwrap();
                direct.set_target(&machine);
                if matches!(opt, Optimization::Speed) {
                    direct.run_core_passes(&machine).unwrap();
                }
                direct.verify().unwrap();
                assert_eq!(
                    direct.emit_object(&machine).unwrap(),
                    hirsel_llvm_lazy::compile_ir(&ir, matches!(opt, Optimization::Speed)).unwrap()
                );
            }
        }
    }
}

#[test]
fn storage_roles_keep_moved_buffers_in_one_scope() {
    let module = text::parse(include_str!("../../tests/core_runtime/roles.core")).unwrap();
    wf::module(&module).unwrap();
    let mut declarations = BTreeSet::new();
    let scopes = alias::Scopes::new(
        &module,
        &hirsel_core_ir::roles::Facts::new(&module),
        FuncId(0),
        "test",
        &mut 2,
        &mut declarations,
    )
    .unwrap();
    assert_eq!(
        scopes.access(&[AddressId(3)]),
        scopes.access(&[AddressId(7)]),
        "a move changes the carrier, not the buffer role"
    );
    let on = emit_ir_with_facts(&module, false, true).unwrap();
    assert!(on.contains("@llvm.experimental.noalias.scope.decl(metadata !{"));
    assert!(on.contains("!alias.scope !{"));
    assert!(on.contains("!noalias !{"));
    let off = emit_ir_with_facts(&module, false, false).unwrap();
    assert!(!off.contains("!alias.scope"));
    assert!(!off.contains("!noalias"));
    assert!(!off.contains("@llvm.experimental.noalias.scope.decl"));
    agree(&module, &[(FuncId(0), vec![])]);
}

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
    source.push_str("sig g2 = (u8, u8) -> (u8, bool)\nsig g3 = (u8, u8) -> (u8, bool)\n");
    for i in 0..8 {
        writeln!(source, "inst n{i} = llvm@1/checked/op{i} [] []").unwrap();
    }
    for (i, op) in ["add", "sub", "mul"].iter().cycle().take(6).enumerate() {
        let t = if i < 3 { "u8" } else { "i8" };
        writeln!(source, "fn f{i} = n{i} g{} {{\nb0(v0: {t}, v1: {t}):\n  v2: {t} = {op} v0, v1\n  return v2\n}}", i / 3).unwrap();
    }
    source.push_str("fn f6 = n6 g2 {\nb0(v0: u8, v1: u8):\n  v2: u8, v3: bool = checked_add v0, v1\n  return v2, v3\n}\n");
    source.push_str("fn f7 = n7 g3 {\nb0(v0: u8, v1: u8):\n  v2: u8, v3: bool = checked_sub v0, v1\n  return v2, v3\n}\n");
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for function in 0..8 {
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
    host_memory::check_with_site(
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
    sites: Vec<u64>,
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
    execute_optimized(module, cases, bindings, configuration, Optimization::None)
}
fn execute_optimized(
    module: &Module,
    cases: &[Case],
    bindings: &str,
    configuration: &str,
    optimization: Optimization,
) -> Vec<NativeReport> {
    execute_object(module, cases, bindings, configuration, optimization, None)
}
fn execute_object(
    module: &Module,
    cases: &[Case],
    bindings: &str,
    configuration: &str,
    optimization: Optimization,
    object: Option<Vec<u8>>,
) -> Vec<NativeReport> {
    execute_target_object(
        module,
        cases,
        bindings,
        configuration,
        optimization,
        object,
        Target::X86_64Linux,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_target_object(
    module: &Module,
    cases: &[Case],
    bindings: &str,
    configuration: &str,
    optimization: Optimization,
    object: Option<Vec<u8>>,
    target: Target,
) -> Vec<NativeReport> {
    execute_target_object_checked(
        module,
        cases,
        bindings,
        configuration,
        optimization,
        object,
        target,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_target_object_checked(
    module: &Module,
    cases: &[Case],
    bindings: &str,
    configuration: &str,
    optimization: Optimization,
    object: Option<Vec<u8>>,
    target: Target,
    memcheck: bool,
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
    let ir = emit_ir_for_target(module, target, observing).unwrap();
    let context = Context::new();
    let llvm = LlvmModule::parse(&context, "test", &ir).unwrap();
    llvm.verify().unwrap_or_else(|e| panic!("{e}\n{ir}"));
    let bytes = object.unwrap_or_else(|| {
        emit_object_for_target(module, target, optimization, observing).unwrap()
    });
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
                .chain(std::iter::once("uint64_t"))
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
        let args = format!("{args}, 0");
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
        writeln!(c,"HirselCoreCauseCell *chain=hirsel_core_causes_take(ctx); if({status}) for(const HirselCoreCauseCell *p=chain;p;p=hirsel_core_cause_next(p)) printf(\" %s\",causes[hirsel_core_cause_kind(hirsel_core_cause_value(p))]); printf(\"|%llu|\",(unsigned long long)(initial-hirsel_core_test_available(ctx))); for(size_t i=0;i<traces;i++) printf(\" %llu\",(unsigned long long)trace[i]); printf(\"|%zu|\",hirsel_core_test_attempts(ctx)); for(const HirselCoreCauseCell *p=chain;p;p=hirsel_core_cause_next(p)) if(hirsel_core_cause_kind(hirsel_core_cause_value(p))==4) {{ const HirselCoreStringMessage *msg=&hirsel_core_cause_value(p)->message; for(uint64_t i=0;i<msg->len;i++) printf(\"%02x\",((const unsigned char *)msg->data)[i]); printf(\" \"); }} printf(\"|\"); for(size_t j=0;j<arc_count;j++) printf(\" %u:%llu\",arc_events[j].kind,(unsigned long long)arc_events[j].value); printf(\"|\"); for(const HirselCoreCauseCell *p=chain;p;p=hirsel_core_cause_next(p)) printf(\" %llu\",(unsigned long long)hirsel_core_cause_value(p)->site); printf(\"\\n\"); hirsel_core_causes_drop(chain); return 0; }}").unwrap();
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
    let mut compiler = match target {
        Target::X86_64Linux => {
            let mut command = Command::new(prefix.join("bin/clang"));
            command.arg(format!("--ld-path={}", prefix.join("bin/ld.lld").display()));
            command
        }
        Target::Aarch64Linux => Command::new(std::env::var_os("HIRSEL_AARCH64_CC").unwrap()),
    };
    if memcheck && target == Target::X86_64Linux {
        // The pool's system loader is stripped; use the declared symbol-bearing
        // loader/runtime paired with Memcheck, just as the toolchain proof does.
        let runtime = PathBuf::from(
            std::env::var_os("HIRSEL_VALGRIND_RUNTIME").expect("declared Memcheck runtime"),
        );
        compiler
            .args(["-Xlinker", "--dynamic-linker", "-Xlinker"])
            .arg(runtime.join("ld-linux-x86-64.so.2"))
            .args(["-Xlinker", "-rpath", "-Xlinker"])
            .arg(&runtime);
    }
    output(
        compiler
            .args(["-std=c11", "-O0", "-fPIC", "-pie", "-Wl,-z,text", "-I"])
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hirsel-core-rt/include"))
            .arg(dir.join("test.c"))
            .arg(dir.join("core.o"))
            .arg(runtime)
            .args(["-lgcc_s", "-lpthread", "-ldl", "-lm", "-lrt", "-lutil"])
            .arg("-o")
            .arg(dir.join("test")),
    );
    {
        use object::{Object as _, ObjectSymbol as _};
        let image = std::fs::read(dir.join("test")).unwrap();
        let file = object::File::parse(image.as_slice()).unwrap();
        assert_eq!(file.kind(), object::ObjectKind::Dynamic);
        for symbol in file
            .symbols()
            .filter(|s| s.name().is_ok_and(|n| n.starts_with("hirsel_core_site_")))
        {
            assert_eq!(symbol.section(), object::SymbolSection::Absolute);
            assert!(
                matches!(symbol.flags(), object::SymbolFlags::Elf { st_other, .. } if st_other.0 & 3 == object::elf::STV_HIDDEN.0)
            );
        }
    }
    let mut executable = match target {
        Target::X86_64Linux if memcheck => {
            let mut command =
                Command::new(std::env::var_os("HIRSEL_VALGRIND").expect("declared Memcheck tool"));
            command
                .args([
                    "--tool=memcheck",
                    "--quiet",
                    "--exit-on-first-error=yes",
                    "--error-exitcode=99",
                ])
                .arg(dir.join("test"));
            command
        }
        Target::X86_64Linux => Command::new(dir.join("test")),
        Target::Aarch64Linux => {
            let mut command = Command::new(std::env::var_os("HIRSEL_QEMU_AARCH64").unwrap());
            command
                .arg("-L")
                .arg(std::env::var_os("HIRSEL_AARCH64_SYSROOT").unwrap())
                .arg(dir.join("test"));
            command
        }
    };
    output(&mut executable)
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let case = &cases[i];
            let mut parts = line.split('|');
            let peak_reserved = observing.then(|| parts.next().unwrap().parse().unwrap());
            let mut values = parts.next().unwrap().split_whitespace();
            let status = values.next().unwrap();
            let outcome = if status == "R" {
                let signature =
                    &module.signatures[module.functions[case.function.index()].signature.index()];
                eval::Outcome::Returned(
                    values
                        .enumerate()
                        .map(|(i, v)| eval::observed(signature.results[i], v.parse().unwrap()))
                        .collect(),
                )
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
                sites: parts
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .map(|s| s.parse().unwrap())
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
        let actual_sites = got
            .sites
            .iter()
            .map(|word| {
                (*word != 0).then(|| {
                    (
                        module.items[((word >> 32) - 1) as usize].clone(),
                        *word as u32,
                    )
                })
            })
            .collect::<Vec<_>>();
        let expected_sites = expected
            .sites
            .iter()
            .map(|a| a.map(|a| (module.items[a.item.index()].clone(), a.start)))
            .collect::<Vec<_>>();
        assert_eq!(
            actual_sites, expected_sites,
            "failure sites f{}",
            case.function.0
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
fn failure_edges_precede_later_scalar_and_address_definitions() {
    let module = text::parse(
        r#"core v1
sig g0 = (u64, u64) -> (u64, u64)
inst n0 = tests@0/homes/failing_prefix [] []
fn f0 = n0 g0 {
 slot s0 = u64
 addr a0 = s0 : u64
b0(v0: u64, v1: u64):
 live s0
 v2: u64, v3: bool = checked_add v0, v1
 v4: bool = const false
 v5: bool = eq v3, v4
 v6: u64 = add v0, v1 fail b1
 v7: u64 = const 1
 v8: u64 = wrapping_add v0, v7
 address a0
 store a0, v8
 jump b4()
b1():
 branch v5, b2(), b3()
b2():
 v9: u64 = read a0
 v10: u64 = wrapping_add v8, v9
 dead s0
 propagate
b3():
 dead s0
 propagate
b4():
 v11: u64 = read a0
 dead s0
 return v8, v11
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    agree(
        &module,
        &[
            (FuncId(0), vec![0, 0]),
            (FuncId(0), vec![2, 3]),
            (FuncId(0), vec![u64::MAX, 1]),
        ],
    );
}

#[test]
fn message_raise_reads_a_correlated_cross_block_address() {
    let source = include_str!("../../tests/core_runtime/callable.core");
    let (prefix, body) = source.split_once("fn f3 = n3 g3 {").unwrap();
    let prefix = prefix.replace("sig g3 = () -> ()", "sig g3 = (bool) -> ()");
    let body = body
        .replacen("b0:", "  addr a7 = s0 : t1\nb0(v9: bool):", 1)
        .replacen("  address a0\n", "", 1)
        .replace(
            "  raise fail a0",
            r#"  branch v9, b1(), b2()
b1():
  address a0
  jump b2()
b2():
  branch v9, b3(), b4()
b3():
  raise fail a0
b4():
  address a7
  drop a7 t1
  dead s0
  return"#,
        );
    let module = text::parse(&format!("{prefix}fn f3 = n3 g3 {{{body}")).unwrap();
    wf::module(&module).unwrap();
    agree_cases(
        &module,
        &[
            Case {
                function: FuncId(3),
                args: vec![0],
                limit: 256,
                injection: None,
            },
            Case {
                function: FuncId(3),
                args: vec![1],
                limit: 256,
                injection: None,
            },
        ],
        true,
    );
}

#[test]
fn local_homes_preserve_inactive_addresses_and_entry_back_edges() {
    let module = text::parse(
        r#"core v1
sig g0 = (u64) -> (u64)
sig g1 = (bool, u64) -> (u64)
inst n0 = tests@0/homes/local [] []
inst n1 = tests@0/homes/identity [] []
inst n2 = tests@0/homes/inactive [] []
inst n3 = tests@0/homes/entry_loop [] []
fn f0 = n0 g0 {
 slot s0 = u64
 addr a0 = s0 : u64
b0(v0: u64):
 live s0
 address a0
 store a0, v0
 v1: u64 = read a0
 v2: u64 = call f1 g0 (v1)
 dead s0
 return v2
}
fn f1 = n1 g0 {
b0(v0: u64):
 return v0
}
fn f2 = n2 g1 {
 slot s0 = u64
 addr a0 = s0 : u64
b0(v0: bool, v1: u64):
 live s0
 branch v0, b1(), b2()
b1():
 address a0
 store a0, v1
 jump b2()
b2():
 branch v0, b3(), b4()
b3():
 v2: u64 = read a0
 dead s0
 return v2
b4():
 v3: u64 = const 0
 dead s0
 return v3
}
fn f3 = n3 g0 {
b0(v0: u64):
 v1: u64 = const 0
 v2: bool = ne v0, v1
 branch v2, b1(), b2()
b1():
 v3: u64 = const 1
 v4: u64 = wrapping_sub v0, v3
 jump b0(v4)
b2():
 return v0
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    let raw = emit_ir(&module).unwrap();
    let local = raw
        .split("define ")
        .find(|f| f.contains(&format!("@{}", symbol(&module, FuncId(0)))))
        .unwrap();
    assert!(
        !local.contains("%a0 = alloca"),
        "local address is an SSA value"
    );
    assert!(
        !local.contains("%v1 = alloca"),
        "local call operand is an SSA value"
    );
    agree(
        &module,
        &[
            (FuncId(0), vec![42]),
            (FuncId(0), vec![u64::MAX]),
            (FuncId(2), vec![0, 42]),
            (FuncId(2), vec![1, 42]),
            (FuncId(3), vec![0]),
            (FuncId(3), vec![4]),
        ],
    );
}

#[test]
fn scalar_values_keep_inactive_cross_block_paths_in_storage() {
    let module = text::parse(
        r#"
core v1
sig g0 = (bool, u64) -> (u64)
inst n0 = tests@0/local_values [] []
fn f0 = n0 g0 {
b0(v0: bool, v1: u64):
 branch v0, b1(), b2()
b1():
 v2: u64 = const 1
 v3: u64 = add v1, v2
 jump b2()
b2():
 branch v0, b3(), b4()
b3():
 return v3
b4():
 v4: u64 = const 0
 return v4
}
"#,
    )
    .unwrap();
    wf::module(&module).unwrap();
    agree(
        &module,
        &[(FuncId(0), vec![0, 41]), (FuncId(0), vec![1, 41])],
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

#[test]
fn numerical_returns_keep_failure_status_and_canonical_bits() {
    let module = text::parse(include_str!("../../tests/core_runtime/numeric-status.core")).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for function in [FuncId(0), FuncId(1)] {
        for index in [0, 7, u64::MAX - 1, u64::MAX] {
            for bits in [
                0,
                (-0.0f64).to_bits(),
                1.0f64.to_bits(),
                f64::INFINITY.to_bits(),
                0x7ff0_0000_0000_0001,
                0xfff8_0000_0000_0042,
            ] {
                cases.push(Case {
                    function,
                    args: vec![index, bits],
                    limit: u64::MAX,
                    injection: None,
                });
            }
        }
    }
    for facts in [false, true] {
        let ir = emit_ir_with_facts(&module, false, facts).unwrap();
        for speed in [false, true] {
            let object = hirsel_llvm_lazy::compile_ir(&ir, speed).unwrap();
            let context = crate::context::Context::new();
            let direct = crate::context::Module::parse(&context, "hirsel-core", &ir).unwrap();
            let machine = crate::context::TargetMachine::new(
                &hirsel_lir::layout_types::TargetSpec::x86_64_unknown_linux_gnu(),
                speed,
            )
            .unwrap();
            direct.set_target(&machine);
            if speed {
                direct.run_core_passes(&machine).unwrap();
            }
            assert_eq!(object, direct.emit_object(&machine).unwrap());
            let results = execute_object(
                &module,
                &cases,
                "",
                "",
                if speed {
                    Optimization::Speed
                } else {
                    Optimization::None
                },
                Some(object),
            );
            assert_eq!(results.len(), cases.len());
            for (case, actual) in cases.iter().zip(results) {
                let expected = eval::run(&module, case.function, &case.args, case.limit);
                assert_eq!(actual.outcome, expected.outcome, "{:?}", case.args);
                assert_eq!(actual.used, expected.used);
            }
        }
    }
}

#[test]
fn numerical_chains_observe_bits_after_memory_copies_and_calls() {
    let mut source = String::from("core v1\n");
    let mut cases = Vec::new();
    for (kind, word, inputs) in [
        (
            "f32",
            "u32",
            vec![
                0,
                0x8000_0000,
                1,
                0x0080_0000,
                0x3f80_0000,
                0xbf80_0000,
                0x7f80_0000,
                0xff80_0000,
                0x7fc0_0001,
                0xffc0_0042,
                0x7f80_0001,
            ],
        ),
        (
            "f64",
            "u64",
            vec![
                0,
                0x8000_0000_0000_0000,
                1,
                0x0010_0000_0000_0000,
                0x3ff0_0000_0000_0000,
                0xbff0_0000_0000_0000,
                0x7ff0_0000_0000_0000,
                0xfff0_0000_0000_0000,
                0x7ff8_0000_0000_0001,
                0xfff8_0000_0000_0042,
                0x7ff0_0000_0000_0001,
            ],
        ),
    ] {
        let identity = source.matches("\nfn ").count();
        writeln!(source, "sig g{identity} = ({kind}) -> ({kind})\ninst n{identity} = tests@0/numeric/{kind}_identity [] []\nfn f{identity} = n{identity} g{identity} {{\nb0(v0: {kind}):\n return v0\n}}").unwrap();
        let bytes = if kind == "f32" { 4 } else { 8 };
        for op in [
            "add",
            "sub",
            "mul",
            "div",
            "min",
            "max",
            "min_number",
            "max_number",
        ] {
            let i = source.matches("\nfn ").count();
            // Memory, byte copies, edges and calls carry floats transparently.
            // Typed float reads followed by reinterpret observe their bits.
            writeln!(source, "sig g{i} = ({kind}, {kind}) -> ({kind}, {word}, {word}, {word}, {word})\ninst n{i} = tests@0/numeric/{kind}_{op} [] []\nfn f{i} = n{i} g{i} {{\n slot s0 = {kind}\n slot s1 = {kind}\n slot s2 = {kind}\n slot s3 = {kind}\n addr a0 = s0 : {kind}\n addr a1 = v9 index v8 : {kind}\n addr a2 = s1 : {kind}\n addr a3 = s2 : {kind}\n addr a4 = s3 : {kind}\nb0(v0: {kind}, v1: {kind}):\n v2: {kind} = {op} v0, v1\n jump b1(v2)\nb1(v3: {kind}):\n v4: {kind} = neg v3\n v5: {kind} = abs v4\n v6: {kind} = sqrt v5\n v7: {kind} = mul_add v6, v6, v3\n live s0\n address a0\n store a0, v7\n live s1\n live s2\n live s3\n address a2\n address a3\n address a4\n store a2, v7\n store a3, v7\n v13: u64 = const {bytes}\n copy a4, a3, v13\n v14: {kind} = read a2\n v15: {word} = reinterpret v14\n v16: {kind} = read a4\n v17: {word} = reinterpret v16\n v8: u64 = const 0\n v9: ptr = pointer a0\n address a1\n v10: {kind} = read a1\n v18: {word} = reinterpret v10\n v11: {word} = reinterpret v7\n v12: {kind} = call f{identity} g{identity} (v7)\n dead s3\n dead s2\n dead s1\n dead s0\n return v12, v18, v11, v15, v17\n}}").unwrap();
            for &a in &inputs {
                for &b in &inputs {
                    cases.push(Case {
                        function: FuncId(i as u32),
                        args: vec![a, b],
                        limit: u64::MAX,
                        injection: None,
                    });
                }
            }
        }
    }
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let expected: Vec<_> = cases
        .iter()
        .map(|c| eval::run(&module, c.function, &c.args, c.limit).outcome)
        .collect();
    for optimization in [Optimization::None, Optimization::Speed] {
        let reports = execute_optimized(&module, &cases, "", "", optimization);
        assert_eq!(reports.len(), expected.len());
        for (got, want) in reports.iter().zip(&expected) {
            assert_eq!(&got.outcome, want);
        }
    }
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
        .replace("  raise fail a0", "  v9: u64 = const 32\n  v10: ptr, v11: u64 = alloc for rc t1 v9 align 8 charge v9\n  address a7\n  v12: u64 = const 24\n  copy a7, a0, v12\n  release v10 rc t1\n  return");
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
fn hash_read_facts_observe_intervening_writes() {
    let module = text::parse(include_str!("../../tests/core_runtime/hash-writes.core")).unwrap();
    wf::module(&module).unwrap();
    let cases = [(0, 1), (1, 0), (u64::MAX, 0), (1 << 63, u64::MAX)].map(|(a, b)| Case {
        function: FuncId(0),
        args: vec![a, b],
        limit: 0,
        injection: None,
    });
    for facts in [false, true] {
        let ir = emit_ir_with_facts(&module, false, facts).unwrap();
        for speed in [false, true] {
            let object = hirsel_llvm_lazy::compile_ir(&ir, speed).unwrap();
            let got = execute_object(&module, &cases, "", "", Optimization::None, Some(object));
            for (case, actual) in cases.iter().zip(got) {
                let expected = eval::run(&module, case.function, &case.args, case.limit);
                assert_eq!(actual.outcome, expected.outcome);
                assert_eq!(actual.used, 0);
                assert_eq!(actual.admissions, 0);
            }
        }
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
fn isolate_sample_and_admission_refusals_match_the_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/isolate.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for case in sample_cases(&module, source) {
        let baseline = eval::run(&module, case.function, &case.args, case.limit);
        cases.push(Case {
            function: case.function,
            args: case.args.clone(),
            limit: case.limit,
            injection: None,
        });
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
    }
    println!("isolate executions: {}", cases.len());
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
R1 EXTERNAL(void *context, uint64_t mode, uint64_t caller_site) {
    (void)caller_site;
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
            HostOp::Rename => &["u64", "ptr", "ptr"],
            HostOp::SleepUntil => &["u64", "u64"],
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
fn log_append_facts_on_and_off_never_store_through_the_handle() {
    let module = text::parse(include_str!("../../../hirsel-core-ir/samples/log.core")).unwrap();
    wf::module(&module).unwrap();
    for facts in [false, true] {
        let ir = emit_ir_with_facts(&module, false, facts).unwrap();
        let body = ir
            .split_once("define")
            .and_then(|(_, rest)| rest.split_once("define").map(|(first, _)| first))
            .unwrap_or(&ir);
        assert!(
            !body
                .lines()
                .any(|line| line.contains("store") && line.contains("%p0")),
            "log append stored through its handle (facts={facts}): {ir}"
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
fn raw_library_bitcode_preserves_text_objects_and_runs() {
    let library = text::parse(
        r#"core v1
sig g0 = (u64) -> (u64)
inst n0 = samples@1/raw_library/xor [] []
fn f0 = n0 g0 {
b0(v0: u64):
  v1: u64 = const 5
  v2: u64 = bitxor v0, v1
  return v2
}
"#,
    )
    .unwrap();
    let caller = text::parse(
        r#"core v1
sig g0 = (u64) -> (u64)
inst n0 = samples@1/raw_library/xor [] []
inst n1 = samples@1/raw_library/entry [] []
fn f0 = n0 g0 extern
fn f1 = n1 g0 {
b0(v0: u64):
  v1: u64 = call f0 g0 (v0)
  return v1
}
"#,
    )
    .unwrap();
    wf::module(&library).unwrap();
    wf::module(&caller).unwrap();
    let facts = hirsel_core_ir::roles::Facts::program(&[&library, &caller], &[]);
    for target in [Target::X86_64Linux, Target::Aarch64Linux] {
        let product = hirsel_llvm_lazy::compile_library(
            &library,
            &facts[0],
            target,
            false,
            false,
            hirsel_core_ids::ContentHash::of_bytes(b"bitcode fixture"),
        )
        .unwrap();
        let library_ir = hirsel_llvm_lazy::core::emit_ir_relocatable_with_role_facts_for_target(
            &library, target, false, false, &facts[0],
        )
        .unwrap();
        assert_eq!(
            product.object.bytes,
            hirsel_llvm_lazy::compile_ir_for_target(&library_ir, true, target).unwrap()
        );
        assert_eq!(product.summary.facts, facts[0].record(&library));
        assert!(product.summary.selected());
        assert!(
            product
                .summary
                .functions
                .iter()
                .any(|f| f.symbol == symbol(&library, FuncId(0)) && f.instructions > 2)
        );
        let caller_ir = hirsel_llvm_lazy::core::emit_ir_relocatable_with_role_facts_for_target(
            &caller, target, false, false, &facts[1],
        )
        .unwrap();
        let exports = [symbol(&caller, FuncId(1))];
        for reversed in [false, true] {
            let mut text_units = [
                hirsel_llvm_lazy::Unit::Text {
                    ir: &library_ir,
                    imported: true,
                },
                hirsel_llvm_lazy::Unit::Text {
                    ir: &caller_ir,
                    imported: false,
                },
            ];
            let mut bitcode_units = [
                hirsel_llvm_lazy::Unit::Import {
                    bitcode: &product.bitcode,
                    summary: &product.summary,
                },
                hirsel_llvm_lazy::Unit::Text {
                    ir: &caller_ir,
                    imported: false,
                },
            ];
            if reversed {
                text_units.reverse();
                bitcode_units.reverse();
            }
            let control =
                hirsel_llvm_lazy::emit_program_for_target(&text_units, &exports, target).unwrap();
            let native =
                hirsel_llvm_lazy::emit_program_for_target(&bitcode_units, &exports, target)
                    .unwrap();
            assert_eq!(control, native, "{target:?}, reversed={reversed}");
            if target == Target::X86_64Linux && !reversed {
                let mut oracle = caller.clone();
                oracle.functions[0].body = library.functions[0].body.clone();
                let cases = [0, 1, 7, u64::MAX].map(|value| Case {
                    function: FuncId(1),
                    args: vec![value],
                    limit: 0,
                    injection: None,
                });
                let reports =
                    execute_object(&oracle, &cases, "", "", Optimization::Speed, Some(native));
                for (case, report) in cases.iter().zip(reports) {
                    assert_eq!(
                        report.outcome,
                        eval::run(&oracle, case.function, &case.args, 0).outcome
                    );
                    assert_eq!(report.used, 0);
                }
            }
        }
        let dir = std::env::temp_dir().join(format!(
            "hirsel-raw-bitcode-{}-{target:?}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("library.bc"), product.bitcode).unwrap();
        let prefix = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
        let decoded = output(
            Command::new(prefix.join("bin/llvm-dis"))
                .arg(dir.join("library.bc"))
                .arg("-o")
                .arg("-"),
        );
        assert!(
            decoded.contains("alloca"),
            "raw homes must precede every pass"
        );
        assert!(
            !decoded.contains("probe-stack"),
            "probe settings are applied after the snapshot"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn raw_imports_above_the_body_bound_keep_the_rest_of_the_module() {
    let mut library = "@library_value = constant i64 17\ndefine i64 @large(i64 %x) {\n".to_owned();
    let limit = hirsel_llvm_lazy::INLINE_IMPORT_INSTRUCTION_LIMIT;
    for i in 0..limit {
        let previous = if i == 0 {
            "%x".into()
        } else {
            format!("%x{}", i - 1)
        };
        writeln!(library, "%x{i} = add i64 {previous}, 0").unwrap();
    }
    writeln!(library, "ret i64 %x{}\n}}", limit - 1).unwrap();
    let caller = "@library_value = external constant i64\ndefine i64 @entry() {\n%x = load i64, ptr @library_value\nret i64 %x\n}";
    let dir = std::env::temp_dir().join(format!("hirsel-raw-bound-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("library.ll"), &library).unwrap();
    let prefix = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    output(
        Command::new(prefix.join("bin/llvm-as"))
            .arg(dir.join("library.ll"))
            .arg("-o")
            .arg(dir.join("library.bc")),
    );
    let bitcode = std::fs::read(dir.join("library.bc")).unwrap();
    let summary = hirsel_llvm_lazy::ImportSummary {
        functions: vec![hirsel_llvm_lazy::ImportFunction {
            symbol: "large".into(),
            instructions: limit + 1,
        }],
        facts: Vec::new(),
    };
    assert!(!summary.selected());
    let text = hirsel_llvm_lazy::emit_program(
        &[
            hirsel_llvm_lazy::Unit::Text {
                ir: &library,
                imported: true,
            },
            hirsel_llvm_lazy::Unit::Text {
                ir: caller,
                imported: false,
            },
        ],
        &["entry".into()],
    )
    .unwrap();
    let decoded = hirsel_llvm_lazy::emit_program(
        &[
            hirsel_llvm_lazy::Unit::Import {
                bitcode: &bitcode,
                summary: &summary,
            },
            hirsel_llvm_lazy::Unit::Text {
                ir: caller,
                imported: false,
            },
        ],
        &["entry".into()],
    )
    .unwrap();
    assert_eq!(decoded, text, "the bound selects bodies, not whole modules");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn release_import_bound_keeps_small_bodies_and_uses_cached_large_entries() {
    fn body(name: &str, instructions: usize, linkage: &str) -> String {
        let mut text = format!("define {linkage}i64 @{name}(i64 %x) {{\n");
        for i in 0..instructions - 1 {
            let input = if i == 0 {
                "%x".into()
            } else {
                format!("%v{}", i - 1)
            };
            writeln!(text, "%v{i} = add i64 {input}, 0").unwrap();
        }
        writeln!(text, "ret i64 %v{}\n}}", instructions - 2).unwrap();
        text
    }
    let dir = std::env::temp_dir().join(format!("hirsel-core-import-bound-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let prefix = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    std::fs::write(dir.join("probe.c"), "#include <stdint.h>\nextern uint64_t entry(uint64_t);\nint main(void){uint64_t xs[]={0,1,UINT64_MAX};for(unsigned i=0;i<3;i++)if(entry(xs[i])!=xs[i])return 1;return 0;}\n").unwrap();
    let entry = "declare i64 @import_bound(i64)\ndefine i64 @entry(i64 %x) {%r = call i64 @import_bound(i64 %x)\nret i64 %r}";
    let limit = hirsel_llvm_lazy::INLINE_IMPORT_INSTRUCTION_LIMIT as usize;
    for instructions in [limit - 1, limit + 1] {
        for wrapper in [false, true] {
            let imported = if wrapper {
                let mut text = body("import_bound.infallible", instructions - 2, "internal ");
                text.push_str("define i64 @import_bound(i64 %x) {%r = call i64 @import_bound.infallible(i64 %x)\nret i64 %r}\n");
                text
            } else {
                body("import_bound", instructions, "")
            };
            let units = [
                hirsel_llvm_lazy::Unit::Text {
                    ir: &imported,
                    imported: true,
                },
                hirsel_llvm_lazy::Unit::Text {
                    ir: entry,
                    imported: false,
                },
            ];
            let bytes = hirsel_llvm_lazy::emit_program(&units, &["entry".into()]).unwrap();
            std::fs::write(dir.join("unit.o"), bytes).unwrap();
            let undefined = output(
                Command::new(prefix.join("bin/llvm-nm"))
                    .arg("--undefined-only")
                    .arg(dir.join("unit.o")),
            );
            assert_eq!(
                undefined.contains("import_bound"),
                instructions > limit,
                "{instructions} instructions, wrapper={wrapper}: {undefined}"
            );
            // The cached library supplies the same public definition when the
            // application keeps only its declaration, including wrapper bodies.
            std::fs::write(
                dir.join("library.o"),
                hirsel_llvm_lazy::compile_ir(&imported, true).unwrap(),
            )
            .unwrap();
            output(
                Command::new(prefix.join("bin/clang"))
                    .arg(format!("--ld-path={}", prefix.join("bin/ld.lld").display()))
                    .arg(dir.join("probe.c"))
                    .arg(dir.join("unit.o"))
                    .arg(dir.join("library.o"))
                    .arg("-o")
                    .arg(dir.join("probe")),
            );
            output(&mut Command::new(dir.join("probe")));
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn release_imports_keep_transitive_bodies_in_either_unit_order() {
    let leaf = "define i64 @leaf(i64 %x) { %y = mul i64 %x, 3\n ret i64 %y }";
    let middle = "declare i64 @leaf(i64)\ndefine i64 @middle(i64 %x) { %a = call i64 @leaf(i64 %x)\n %y = add i64 %a, 1\n ret i64 %y }";
    let entry = "declare i64 @middle(i64)\ndefine i64 @entry(i64 %x) { %y = call i64 @middle(i64 %x)\n ret i64 %y }";
    let dir = std::env::temp_dir().join(format!("hirsel-core-imports-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let prefix = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    std::fs::write(dir.join("probe.c"), "#include <stdint.h>\nextern uint64_t entry(uint64_t);\nint main(void) {uint64_t xs[]={0,1,UINT64_MAX,UINT64_MAX/3}; for(unsigned i=0;i<4;i++) if(entry(xs[i])!=xs[i]*3+1) return 1; return 0;}\n").unwrap();
    for order in [
        [(leaf, true), (middle, true), (entry, false)],
        [(entry, false), (middle, true), (leaf, true)],
    ] {
        let units = order.map(|(ir, imported)| hirsel_llvm_lazy::Unit::Text { ir, imported });
        let object = hirsel_llvm_lazy::emit_program(&units, &["entry".into()]).unwrap();
        std::fs::write(dir.join("unit.o"), object).unwrap();
        let undefined = output(
            Command::new(prefix.join("bin/llvm-nm"))
                .arg("--undefined-only")
                .arg(dir.join("unit.o")),
        );
        assert!(
            undefined.trim().is_empty(),
            "small supplied bodies must inline: {undefined}"
        );
        output(
            Command::new(prefix.join("bin/clang"))
                .arg(format!("--ld-path={}", prefix.join("bin/ld.lld").display()))
                .arg(dir.join("probe.c"))
                .arg(dir.join("unit.o"))
                .arg("-o")
                .arg(dir.join("probe")),
        );
        output(&mut Command::new(dir.join("probe")));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn private_view_inputs_preserve_public_addresses_and_failure_destinations() {
    let imported = r#"
define zeroext i8 @view(ptr %ctx, ptr %out, ptr readonly %pair, i8 zeroext %fail) noinline "hirsel-view-input" {
entry:
  %bad = icmp ne i8 %fail, 0
  br i1 %bad, label %failure, label %success
success:
  %a = load i64, ptr %pair, align 1
  %p = getelementptr i8, ptr %pair, i64 8
  %b = load i64, ptr %p, align 1
  %sum = add i64 %a, %b
  store i64 %sum, ptr %out, align 1
  ret i8 0
failure:
  ret i8 %fail
}
"#;
    let owned = r#"
declare zeroext i8 @view(ptr, ptr, ptr, i8 zeroext)
@addresses = private constant [1 x ptr] [ptr @view]
define ptr @address() {
  %p = load ptr, ptr @addresses
  ret ptr %p
}
define zeroext i8 @direct(ptr %out, ptr %pair, i8 zeroext %fail) {
  %status = call zeroext i8 @view(ptr null, ptr %out, ptr %pair, i8 zeroext %fail)
  ret i8 %status
}
"#;
    let dir = std::env::temp_dir().join(format!("hirsel-core-view-inputs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let prefix = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    std::fs::write(
        dir.join("probe.c"),
        r#"#include <stdint.h>
typedef uint8_t (*View)(void *, uint64_t *, const uint64_t *, uint8_t);
uint8_t view(void *ctx, uint64_t *out, const uint64_t *p, uint8_t fail) {
    if (fail) return fail;
    *out = p[0] + p[1]; return 0;
}
extern View address(void);
extern uint8_t direct(uint64_t *, const uint64_t *, uint8_t);
int main(void) {
    if (address() != view) return 1;
    uint64_t pair[] = {UINT64_MAX, 3};
    for (unsigned fail = 0; fail < 256; ++fail) {
        uint64_t a = 123, b = 123;
        if (direct(&a, pair, fail) != fail) return 2;
        if (address()(0, &b, pair, fail) != fail) return 3;
        if (a != b || a != (fail ? 123 : 2)) return 4;
    }
    return 0;
}
"#,
    )
    .unwrap();
    for order in [
        [(imported, true), (owned, false)],
        [(owned, false), (imported, true)],
    ] {
        let units = order.map(|(ir, imported)| hirsel_llvm_lazy::Unit::Text { ir, imported });
        let object =
            hirsel_llvm_lazy::emit_program(&units, &["address".into(), "direct".into()]).unwrap();
        std::fs::write(dir.join("unit.o"), object).unwrap();
        output(
            Command::new(prefix.join("bin/clang"))
                .arg(format!("--ld-path={}", prefix.join("bin/ld.lld").display()))
                .arg(dir.join("probe.c"))
                .arg(dir.join("unit.o"))
                .arg("-o")
                .arg(dir.join("probe")),
        );
        output(&mut Command::new(dir.join("probe")));
    }
    std::fs::remove_dir_all(dir).unwrap();
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

#[test]
fn private_register_results_keep_the_public_destination_and_failure_abi() {
    // A 24-byte destination plus status keeps the ordinary destination ABI.
    for (source, function) in [
        (
            include_str!("../../tests/core_runtime/forward.core"),
            FuncId(0),
        ),
        (
            include_str!("../../tests/core_runtime/register.core"),
            FuncId(0),
        ),
        (
            include_str!("../../../hirsel-core-ir/samples/failure.core"),
            FuncId(1),
        ),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let ir = emit_ir(&module).unwrap();
        let exports = module
            .functions
            .iter()
            .enumerate()
            .map(|(i, _)| symbol(&module, FuncId(i as u32)))
            .collect::<Vec<_>>();
        let bytes = hirsel_llvm_lazy::emit_program(
            &[hirsel_llvm_lazy::Unit::Text {
                ir: &ir,
                imported: false,
            }],
            &exports,
        )
        .unwrap();
        let cases = [0, 1, 2, 3, 123].map(|value| Case {
            function,
            args: vec![value],
            limit: u64::MAX,
            injection: None,
        });
        let got = execute_object(&module, &cases, "", "", Optimization::None, Some(bytes));
        assert_eq!(got.len(), cases.len());
        for (case, actual) in cases.iter().zip(got) {
            let expected = eval::run(&module, case.function, &case.args, case.limit);
            assert_eq!(actual.outcome, expected.outcome);
            assert_eq!(actual.used, expected.used);
            assert_eq!(actual.trace, expected.trace);
        }
    }
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_memory_custody_and_calls_match_the_evaluator() {
    let target = Target::Aarch64Linux;
    let mut count = 0;
    for source in [
        include_str!("../../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../../hirsel-core-ir/samples/calls.core"),
        include_str!("../../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../../hirsel-core-ir/samples/variants.core"),
        include_str!("../../../hirsel-core-ir/samples/custody.core"),
        include_str!("../../../hirsel-core-ir/samples/isolate.core"),
    ] {
        let module = text::parse(source).unwrap();
        wf::module(&module).unwrap();
        let mut cases = sample_cases(&module, source);
        for case in sample_cases(&module, source) {
            let report = eval::run(&module, case.function, &case.args, case.limit);
            for nth in 1..=report.admissions {
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
        assert!(!cases.is_empty());
        for speed in [false, true] {
            for facts in [false, true] {
                let ir = emit_ir_target_with_facts(
                    &module,
                    target,
                    false,
                    facts,
                    SourceEmission {
                        relocatable_sites: false,
                        debug_lines: true,
                    },
                )
                .unwrap();
                let object = hirsel_llvm_lazy::compile_ir_for_target(&ir, speed, target).unwrap();
                let got = execute_target_object(
                    &module,
                    &cases,
                    "",
                    "",
                    if speed {
                        Optimization::Speed
                    } else {
                        Optimization::None
                    },
                    Some(object),
                    target,
                );
                assert_eq!(got.len(), cases.len());
                for (case, got) in cases.iter().zip(got) {
                    let expected = eval::run_injected(
                        &module,
                        case.function,
                        &case.args,
                        case.limit,
                        case.injection,
                    );
                    assert_eq!(
                        got.outcome, expected.outcome,
                        "f{}({:?}),speed={speed},facts={facts}",
                        case.function.0, case.args
                    );
                    assert_eq!(got.used, expected.used, "used");
                    assert_eq!(got.trace, expected.trace, "cleanup trace");
                    assert_eq!(got.admissions, expected.admissions, "admissions");
                    count += 1;
                }
            }
        }
    }
    println!("ARM runtime sample and refusal executions: {count}");
}

fn assert_relative_debug_rows(bytes: &[u8], module: &Module) {
    use object::{Object as _, ObjectSection as _};
    let file = object::File::parse(bytes).unwrap();
    let data = file.section_by_name(".debug_line").unwrap().data().unwrap();
    let strings = file.section_by_name(".debug_str").unwrap().data().unwrap();
    let strings = gimli::DebugStr::new(strings, gimli::LittleEndian);
    let text = |value: gimli::AttributeValue<gimli::EndianSlice<'_, gimli::LittleEndian>>| {
        value
            .string_value(&strings)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    let line = gimli::DebugLine::new(data, gimli::LittleEndian);
    let mut offset = 0;
    let mut mapped = 0;
    while offset < data.len() {
        let program = line
            .program(gimli::DebugLineOffset(offset), 8, None, None)
            .unwrap();
        let length = program.header().unit_length() as usize;
        let mut rows = program.rows();
        while let Some((header, row)) = rows.next_row().unwrap() {
            if row.end_sequence() {
                continue;
            }
            let Some(line) = row.line() else {
                continue;
            };
            let entry = row.file(header).unwrap();
            let path = text(entry.path_name());
            let directory = header
                .directory(entry.directory_index())
                .map(&text)
                .unwrap_or_default();
            let path = if directory.is_empty() {
                path
            } else {
                format!("{directory}/{path}")
            };
            let item = hirsel_core_ids::DefPath::from_full(&path)
                .unwrap_or_else(|| panic!("not an item identity: {path}"));
            let item = module.items.iter().position(|p| *p == item).unwrap();
            let start = (line.get() - 1) as u32;
            assert!(
                start == 0
                    || module
                        .functions
                        .iter()
                        .filter_map(|f| f.body.as_ref())
                        .any(|b| b
                            .inst_anchors
                            .iter()
                            .flatten()
                            .chain(b.blocks.iter().filter_map(|b| b.anchor.as_ref()))
                            .any(|a| a.item.index() == item && a.start == start)),
                "debug offset {start} in {path}"
            );
            mapped += 1;
        }
        offset += 4 + length;
    }
    assert!(mapped > 0, "no item-relative machine rows");
    assert_native_debug_product(bytes);
}

fn assert_native_debug_product(bytes: &[u8]) {
    use hirsel_core_ir::source::ScopeKind;
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
    let unit = hirsel_core_ids::ContentHash::of_bytes(b"native debug witness");
    let product = finish_native_object(bytes.to_vec(), unit, true).unwrap();
    let lazy = hirsel_llvm_lazy::core::finish_native_object(bytes.to_vec(), unit, true).unwrap();
    assert_eq!(product, lazy, "eager/lazy debug extraction differs");
    let file = object::File::parse(product.bytes.as_slice()).unwrap();
    assert!(file.section_by_name(".debug_info").is_none());
    assert!(file.section_by_name(".debug_line").is_none());
    let original = object::File::parse(bytes).unwrap();
    for section in original
        .sections()
        .filter(|s| s.kind() == object::SectionKind::Text)
    {
        assert_eq!(
            section.data().unwrap(),
            file.section_by_name(section.name().unwrap())
                .unwrap()
                .data()
                .unwrap()
        );
    }
    let debug = product.debug.unwrap();
    assert!(!debug.sequences.is_empty());
    assert!(!debug.items.is_empty());
    for (index, section) in debug.sections.iter().enumerate() {
        assert_eq!(
            section.symbol,
            hirsel_core_ir::source::debug_symbol(unit, index as u32)
        );
        let symbol = file
            .symbols()
            .find(|s| s.name().unwrap() == section.symbol)
            .unwrap();
        assert_eq!(symbol.address(), 0);
        assert!(
            matches!(symbol.flags(), object::SymbolFlags::Elf { st_info, st_other }
            if st_info.0 >> 4 == object::elf::STB_GLOBAL.0 && st_other.0 & 3 == object::elf::STV_HIDDEN.0)
        );
    }
    for sequence in &debug.sequences {
        assert!(sequence.range.begin <= sequence.range.end);
        assert!(sequence.range.end <= debug.sections[sequence.range.section as usize].size);
        for row in &sequence.rows {
            assert!((sequence.range.begin..=sequence.range.end).contains(&row.offset));
            if let Some(at) = row.source {
                assert!((at.item as usize) < debug.items.len());
            }
        }
    }
    for (index, scope) in debug.scopes.iter().enumerate() {
        if let Some(parent) = scope.parent {
            assert!((parent as usize) < index);
        }
        if let Some(origin) = scope.origin {
            assert!(matches!(
                debug.scopes[origin as usize].kind,
                ScopeKind::Function { .. }
            ));
        }
    }
    assert!(
        debug
            .scopes
            .iter()
            .any(|s| matches!(&s.kind, ScopeKind::Function { linkage } if !linkage.is_empty()))
    );
    let off = finish_native_object(bytes.to_vec(), unit, false).unwrap();
    assert_eq!(off.bytes, bytes);
    assert!(off.debug.is_none());
}

fn site_fixture(target: Target) {
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/core");
    let source = format!(
        "{}\nfn debug_identity(n: u64) -> u64 {{ n ^ 7 }}\nfn debug_resource(n: u64) -> R {{ match R(d: n) {{ Ok(take r) => take r, Error(_) => loop {{}} }} }}\nfn debug_resource_caller(n: u64) -> u64 {{ let r = debug_resource(n); r.d }}\n",
        include_str!("../../../../core/front/cases/body/failure-sites.hirsel")
    );
    let (module, names) = front::lower(&source, &core, "main").unwrap();
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
    let mut bindings = String::new();
    for (i, item) in module.items.iter().enumerate() {
        writeln!(
            bindings,
            "__asm__(\".globl {0}\\n.hidden {0}\\n.set {0}, {1}\\n\");",
            hirsel_core_ir::source::site_symbol(item),
            (i as u64 + 1) << 32
        )
        .unwrap();
    }
    for (opt, composed, debug_lines) in [
        (Optimization::None, false, true),
        (Optimization::Speed, false, true),
        (Optimization::Speed, true, true),
        (Optimization::Speed, false, false),
    ] {
        let bytes = if composed {
            let ir = emit_ir_relocatable_for_target(&module, target, false, true).unwrap();
            let exports = cases
                .iter()
                .map(|case| symbol(&module, case.function))
                .collect::<Vec<_>>();
            hirsel_llvm_lazy::emit_program_for_target(
                &[hirsel_llvm_lazy::Unit::Text {
                    ir: &ir,
                    imported: false,
                }],
                &exports,
                target,
            )
            .unwrap()
        } else if debug_lines {
            emit_object_relocatable_for_target(&module, target, opt, false, true).unwrap()
        } else {
            emit_object_relocatable_for_target(&module, target, opt, false, false).unwrap()
        };
        let file = object::File::parse(bytes.as_slice()).unwrap();
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
                    assert_eq!(relocation.size(), 64);
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
            assert_relative_debug_rows(&bytes, &module);
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
                let ir = emit_ir_relocatable_for_target(&module, target, false, true).unwrap();
                let lazy = hirsel_llvm_lazy::compile_ir_for_target(
                    &ir,
                    matches!(opt, Optimization::Speed),
                    target,
                )
                .unwrap();
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
            let actual = got
                .sites
                .iter()
                .map(|word| {
                    (*word != 0).then(|| {
                        (
                            module.items[((word >> 32) - 1) as usize].clone(),
                            *word as u32,
                        )
                    })
                })
                .collect::<Vec<_>>();
            let expected = expected
                .sites
                .iter()
                .map(|a| a.map(|a| (module.items[a.item.index()].clone(), a.start)))
                .collect::<Vec<_>>();
            assert_eq!(
                actual, expected,
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

fn inheritance_fixture(target: Target) {
    let module = text::parse(
        r#"core v1
item i0 = p@0/m/caller
item i1 = p@0/m/local
item i2 = core@1/m/k
sig g0 = (u64) -> (u64)
inst n0 = core@1/m/k [] []
inst n1 = p@0/m/caller [] []
inst n2 = p@0/m/local [] []
inst n3 = p@0/m/caller.$glue [] []
inst n4 = p@0/m/caller_local [] []
inst n5 = p@0/m/caller_generated [] []
inst n6 = p@0/m/caller_indirect_core [] []
inst n7 = p@0/m/caller_indirect_local [] []
fn f0 = n0 g0 {
b0(v0: u64):
  v1: u64 = const 1
  v2: u64 = add v0, v1 at i2 5..10
  return v2
}
fn f1 = n1 g0 {
b0(v0: u64):
  v1: u64 = call f0 g0 (v0) at i0 20..30
  return v1
}
fn f2 = n2 g0 {
b0(v0: u64):
  v1: u64 = const 1
  v2: u64 = add v0, v1 at i1 35..40
  return v2
}
fn f3 = n4 g0 {
b0(v0: u64):
  v1: u64 = call f2 g0 (v0) at i0 40..45
  return v1
}
fn f4 = n3 g0 {
b0(v0: u64):
  v1: u64 = const 1
  v2: u64 = add v0, v1
  return v2
}
fn f5 = n5 g0 {
b0(v0: u64):
  v1: u64 = call f4 g0 (v0) at i0 50..55
  return v1
}
fn f6 = n6 g0 {
b0(v0: u64):
  v1: ptr = fnref f0
  v2: u64 = call v1 g0 (v0) at i0 60..65
  return v2
}
fn f7 = n7 g0 {
b0(v0: u64):
  v1: ptr = fnref f2
  v2: u64 = call v1 g0 (v0) at i0 70..75
  return v2
}
"#,
    )
    .unwrap();
    hirsel_core_ir::wf::module(&module).unwrap();
    let ir = emit_ir_for_target(&module, target, false).unwrap();
    let callee = symbol(&module, FuncId(2));
    let direct = ir
        .lines()
        .find(|line| line.contains("call ") && line.contains(&format!("@{callee}(")))
        .unwrap();
    assert!(
        direct.contains("i64 poison)"),
        "non-inheriting direct call materializes a site: {direct}"
    );
    let expected = [
        0,
        (1u64 << 32) | 20,
        (2u64 << 32) | 35,
        (2u64 << 32) | 35,
        0,
        (1u64 << 32) | 50,
        (1u64 << 32) | 60,
        (2u64 << 32) | 35,
    ];
    let cases = (0..8)
        .map(|f| Case {
            function: FuncId(f),
            args: vec![u64::MAX],
            limit: u64::MAX,
            injection: None,
        })
        .collect::<Vec<_>>();
    for opt in [Optimization::None, Optimization::Speed] {
        let bytes = emit_object_for_target(&module, target, opt, false).unwrap();
        let reports = execute_target_object(&module, &cases, "", "", opt, Some(bytes), target);
        for ((case, actual), site) in cases.iter().zip(reports).zip(expected) {
            let oracle = eval::run(&module, case.function, &case.args, u64::MAX);
            assert_eq!(actual.outcome, oracle.outcome);
            assert_eq!(actual.sites, vec![site], "inheritance f{}", case.function.0);
        }
    }
    println!("caller inheritance observations: 16");
}

#[test]
fn caller_sites_follow_core_generated_direct_and_indirect_inheritance() {
    inheritance_fixture(Target::X86_64Linux);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_caller_sites_follow_core_generated_direct_and_indirect_inheritance() {
    inheritance_fixture(Target::Aarch64Linux);
}

fn check_consuming_growth(target: Target) {
    let source = include_str!("../../../hirsel-core-rt/testdata/grow.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut cases = Vec::new();
    for case in sample_cases(&module, source) {
        let report = eval::run(&module, case.function, &case.args, case.limit);
        for nth in 1..=report.admissions {
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
    for speed in [false, true] {
        let ir = emit_ir_target_with_facts(
            &module,
            target,
            std::env::var_os("HIRSEL_TEST_CHARGE_OBSERVATION").is_some(),
            true,
            SourceEmission {
                relocatable_sites: false,
                debug_lines: false,
            },
        )
        .unwrap();
        let object = hirsel_llvm_lazy::compile_ir_for_target(&ir, speed, target).unwrap();
        let reports = execute_target_object(
            &module,
            &cases,
            "",
            "",
            if speed {
                Optimization::Speed
            } else {
                Optimization::None
            },
            Some(object),
            target,
        );
        assert_eq!(reports.len(), cases.len());
        for (case, actual) in cases.iter().zip(reports) {
            let expected = eval::run_injected(
                &module,
                case.function,
                &case.args,
                case.limit,
                case.injection,
            );
            assert_eq!(actual.outcome, expected.outcome);
            assert_eq!(actual.used, expected.used);
            assert_eq!(actual.trace, expected.trace);
            assert_eq!(actual.admissions, expected.admissions);
            if let Some(peak) = actual.peak_reserved {
                assert_eq!(peak, expected.peak_reserved);
            }
        }
    }
    println!(
        "consuming grow: {} cases per profile, both profiles",
        cases.len()
    );
}
#[test]
fn consuming_growth_keeps_the_carrier_and_original_payer_on_refusal() {
    check_consuming_growth(Target::X86_64Linux);
}
#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_consuming_growth_keeps_the_carrier_and_original_payer_on_refusal() {
    check_consuming_growth(Target::Aarch64Linux);
}

#[test]
#[ignore = "explicit AArch64 runtime integration target"]
fn aarch64_consuming_growth_host_memory_refusal_is_a_reported_run_stop() {
    host_memory::check_aarch64(
        |module| {
            let ir = emit_ir_target_with_facts(
                module,
                Target::Aarch64Linux,
                false,
                true,
                SourceEmission {
                    relocatable_sites: false,
                    debug_lines: false,
                },
            )
            .unwrap();
            hirsel_llvm_lazy::compile_ir_for_target(&ir, true, Target::Aarch64Linux).unwrap()
        },
        symbol,
    );
}

fn growth_shadow_words(target: Target) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/core");
    let source = include_str!("../../../../core/front/cases/body/grow-carrier-words.hirsel");
    let (module, names) = front::lower(source, &root, "main").unwrap();
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
        let ir = emit_ir_target_with_facts(
            &module,
            target,
            std::env::var_os("HIRSEL_TEST_CHARGE_OBSERVATION").is_some(),
            true,
            SourceEmission {
                relocatable_sites: false,
                debug_lines: false,
            },
        )
        .unwrap();
        let object = hirsel_llvm_lazy::compile_ir_for_target(&ir, speed, target).unwrap();
        let reports = execute_target_object_checked(
            &module,
            &cases,
            include_str!("../../../hirsel-core-rt/testdata/moving_realloc.c"),
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
