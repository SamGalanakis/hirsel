// Execute objects through a separately compiled C caller. This catches both
// scalar semantics and the register packing a same-backend caller could mask.
use hirsel_core_ir::eval::{self, Outcome};
use hirsel_core_ir::{self as c, *};
use hirsel_cranelift::core::*;
use std::{
    fmt::Write as _,
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

#[path = "front.rs"]
mod front;
#[path = "providers.rs"]
mod providers;

#[allow(dead_code)] // The shared fixture also exposes the pre-site ABI for LLVM's cutover.
#[path = "../../../hirsel-core-rt/testdata/host_memory.rs"]
mod host_memory;

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
fn declare_functions(source: &mut String, module: &c::Module) {
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
                Param::Address { .. } => "void*",
            };
            write!(source, ", {c}").unwrap();
        }
        source.push_str(",uint64_t);\n");
    }
    for (i, item) in module.items.iter().enumerate() {
        writeln!(
            source,
            "const uint64_t {}=UINT64_C({});",
            c::source::site_symbol(item),
            ((i as u64) + 1) << 32
        )
        .unwrap();
    }
    source.push_str("static const uint64_t test_site_items[][4]={");
    if module.items.is_empty() {
        source.push_str("{0,0,0,0}");
    }
    for item in &module.items {
        source.push('{');
        for chunk in item.fingerprint().0.chunks_exact(8) {
            write!(
                source,
                "UINT64_C({}),",
                u64::from_le_bytes(chunk.try_into().unwrap())
            )
            .unwrap();
        }
        source.push_str("},");
    }
    source.push_str("};\nstatic void emit_test_site(uint64_t site) { uint64_t present=site!=0, start=(uint32_t)site, empty[4]={0}; fwrite(&present,8,1,stdout); fwrite(present?test_site_items[(site>>32)-1]:empty,8,4,stdout); fwrite(&start,8,1,stdout); }\n");
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
    let cases: Vec<_> = runs
        .iter()
        .enumerate()
        .map(|(i, (function, args))| NativeCase {
            function: *function,
            args,
            limit,
            injection,
            report: reports.map(|reports| &reports[i]),
        })
        .collect();
    execute_cases(module, optimize, &cases);
}

struct NativeCase<'a> {
    function: FuncId,
    args: &'a [u64],
    limit: u64,
    injection: Option<(usize, bool)>,
    report: Option<&'a eval::Report>,
}

fn execute_cases(module: &c::Module, optimize: bool, cases: &[NativeCase<'_>]) {
    assert!(!cases.is_empty(), "native fixture must execute cases");
    println!("native cases: {} (optimize={optimize})", cases.len());
    fn has_hostfile(field: &hirsel_core_layout::FieldTy) -> bool {
        use hirsel_core_layout::{FieldTy, Handle};
        match field {
            FieldTy::Handle(Handle::HostFile) => true,
            FieldTy::Array(t, _) | FieldTy::Seq(t) => has_hostfile(t),
            FieldTy::Map(k, v) => has_hostfile(k) || has_hostfile(v),
            _ => false,
        }
    }
    let host_files = module.types.iter().any(|t| {
        t.fields(None)
            .is_some_and(|fields| fields.iter().any(|f| has_hostfile(&f.ty)))
    });
    let scheduled = !host_files
        && module
            .functions
            .iter()
            .filter_map(|f| f.body.as_ref())
            .any(|body| {
                body.calls.iter().any(|call| {
                    matches!(
                        call.callee,
                        Callee::Runtime(
                            RtOp::Spawn { .. }
                                | RtOp::Join { .. }
                                | RtOp::Cancel
                                | RtOp::ChannelNew { .. }
                                | RtOp::ChannelSender
                                | RtOp::Send { .. }
                                | RtOp::TrySend { .. }
                                | RtOp::Receive { .. }
                        )
                    )
                })
            });
    let scratch = Scratch::new();
    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let observing = std::env::var_os("HIRSEL_TEST_CHARGE_OBSERVATION").is_some();
    let artifact = hirsel_cranelift::core::emit_object_for_test_observed(
        module,
        &layouts,
        &Options {
            target: NATIVE_TARGET,
            optimize,
        },
        observing,
    )
    .unwrap();
    fs::write(scratch.0.join("input.o"), artifact.bytes).unwrap();
    let mut source = String::from("#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n");
    source.push_str(include_str!(
        "../../../hirsel-core-rt/include/hirsel_core_rt.h"
    ));
    source.push_str("\nextern HirselCoreContext *hirsel_core_test_context_new(uint64_t);\nextern void hirsel_core_test_inject(HirselCoreContext *,size_t,bool);\nextern uint64_t hirsel_core_test_available(const HirselCoreContext *);\nextern size_t hirsel_core_test_attempts(const HirselCoreContext *);\nstatic uint64_t trace[1000000]; static size_t traces;\nvoid hirsel_core_test_drop_trace(uint64_t n) { if(traces >= 1000000) __builtin_abort(); trace[traces++]=n; }\n");
    if observing {
        source.push_str("extern void *hirsel_core_test_observe_begin(const HirselCoreContext *); extern uint64_t hirsel_core_test_observe_end(void *);\n");
    }
    declare_functions(&mut source, module);
    if scheduled {
        source.push_str("extern uint8_t hirsel_core_test_run(HirselCoreContext *,uint8_t (*)(HirselCoreContext*,void*,void*),void*,void*,size_t,size_t);\n");
        let mut wrappers = std::collections::BTreeSet::new();
        for case in cases {
            let f = &case.function;
            if !wrappers.insert(f.0) {
                continue;
            }
            let function = &module.functions[f.index()];
            let signature = &module.signatures[function.signature.index()];
            write!(source, "struct input{} {{", f.0).unwrap();
            if signature.params.is_empty() {
                source.push_str("uint8_t empty;");
            }
            for (i, param) in signature.params.iter().enumerate() {
                let Param::Value(t) = param else {
                    panic!("scalar test input");
                };
                write!(source, "{} a{i};", ctype(*t)).unwrap();
            }
            let result = if signature.results.is_empty() {
                "uint64_t".into()
            } else {
                format!("struct r{}", function.signature.0)
            };
            let returned_place = signature
                .dest
                .map(|ty| {
                    let hirsel_core_layout::Layout::Fixed(layout) = &layouts[ty.index()].layout
                    else {
                        panic!("fixed fixture result");
                    };
                    format!(
                        "_Alignas({}) uint8_t returned_place[{}];",
                        layout.align,
                        layout.size.max(1)
                    )
                })
                .unwrap_or_default();
            write!(source, "}}; static uint8_t run{}(HirselCoreContext *ctx,void *destination,void *input) {{ struct input{} *args=input; {returned_place} *({result}*)destination={} (ctx", f.0, f.0, symbol(&module.instances[function.instance.index()])).unwrap();
            if signature.dest.is_some() {
                source.push_str(",returned_place");
            }
            for i in 0..signature.params.len() {
                write!(source, ",args->a{i}").unwrap();
            }
            source.push_str(",0); return 0; }\n");
        }
    }
    let mut process_calls = String::new();
    if host_files {
        source.push_str(r#"
extern void hirsel_core_test_reset_limit(HirselCoreContext *,uint64_t);
extern uint64_t hirsel_core_test_primary_refund(const HirselCoreContext *);
static HirselCoreHostOutcome file_issue(void *state,HirselCoreHostRequest *r) {
 if(r->operation==0) return (HirselCoreHostOutcome){.value=r->handle};
 if(r->operation!=3) __builtin_abort();
 hirsel_core_test_drop_trace(r->handle);
 return (HirselCoreHostOutcome){.error={5,99},.status=1};
}
static HirselCoreHostOutcome file_deliver(void *s,HirselCoreHostRequest *r,HirselCoreHostOutcome out) { return out; }
static HirselCoreHostOutcome wrong_provider(void *s,HirselCoreHostRequest *r) { __builtin_abort(); }
static void diagnostic(void *s,const uint8_t *p,size_t n) { __builtin_abort(); }
"#);
    } else {
        source.push_str("#include <sys/resource.h>\nint main(void) {\n");
    }
    if cases.iter().any(|case| case.report.is_some()) {
        source.push_str(
            "struct rlimit stack = {65536,65536}; if(setrlimit(RLIMIT_STACK,&stack)) return 7;\n",
        );
    }
    let mut expected = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let f = &case.function;
        let args = case.args;
        let limit = case.limit;
        let injection = case.injection;
        let function = &module.functions[f.index()];
        let s = &module.signatures[function.signature.index()];
        let computed;
        let report = if let Some(report) = case.report {
            report
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
                for (cause_index, cause) in causes.iter().enumerate() {
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
                    let site = report.sites.get(cause_index).copied().flatten();
                    expected.push(u64::from(site.is_some()));
                    if let Some(anchor) = site {
                        for chunk in module.items[anchor.item.index()]
                            .fingerprint()
                            .0
                            .chunks_exact(8)
                        {
                            expected.push(u64::from_le_bytes(chunk.try_into().unwrap()));
                        }
                        expected.push(u64::from(anchor.start));
                    } else {
                        expected.extend([0; 5]);
                    }
                }
            }
        }
        expected.push(report.used);
        if host_files {
            writeln!(source, "static uint8_t fixture{i}(HirselCoreContext *ctx,const HirselCoreProcessConfig *cfg,HirselCoreMainResult *out) {{ hirsel_core_test_reset_limit(ctx,UINT64_C({limit}));").unwrap();
            // The scalar sample ABI erases which input word is an opaque handle.
            // Seed the authored input keys through real OPEN/registry operations;
            // the run owns and drains the unused fixture keys after observation.
            let maximum = args.iter().copied().max().unwrap_or(0);
            writeln!(source, "HirselCoreProvider issuing={{0,file_issue,file_deliver}}; for(uint64_t key=1;key<=UINT64_C({maximum});key++) {{ HirselCoreHostRequest request={{.operation=0,.handle=key}}; HirselCoreHostOutcome opened=hirsel_core_host_file_open(ctx,&issuing,&request);if(opened.status || opened.error.tag || opened.value!=key) __builtin_abort(); }} traces=0;").unwrap();
            writeln!(
                process_calls,
                "{{ uint8_t code=hirsel_core_run(&config,fixture{i}); if(code) {{ fprintf(stderr,\"fixture f{} args {:?} limit {limit} injection {injection:?}: process code %u\\n\",code); return 11; }} }}", f.0, args
            )
            .unwrap();
        } else {
            source.push_str("{\n");
            writeln!(source, "HirselCoreContext *ctx = hirsel_core_test_context_new(UINT64_C({limit})); traces=0;").unwrap();
        }
        if observing {
            source.push_str("void *observation=hirsel_core_test_observe_begin(ctx);\n");
        }
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
        if scheduled {
            let result_type = if s.results.is_empty() {
                "uint64_t".into()
            } else {
                format!("struct r{}", function.signature.0)
            };
            let variable = if s.results.is_empty() {
                "status"
            } else {
                "value"
            };
            writeln!(
                source,
                "{result_type} {variable}; struct input{} args = {{",
                f.0
            )
            .unwrap();
            if args.is_empty() {
                source.push_str("0");
            }
            for j in 0..args.len() {
                write!(source, "{}a{j}", if j == 0 { "" } else { "," }).unwrap();
            }
            writeln!(source, "}}; if(hirsel_core_test_run(ctx,run{},&args,&{variable},sizeof args,sizeof {variable})) return 10;", f.0).unwrap();
        } else {
            if let Some(ty) = s.dest {
                let hirsel_core_layout::Layout::Fixed(layout) = &layouts[ty.index()].layout else {
                    panic!("fixed fixture result");
                };
                writeln!(
                    source,
                    "_Alignas({}) uint8_t returned_place[{}];",
                    layout.align,
                    layout.size.max(1)
                )
                .unwrap();
            }
            write!(
                source,
                "{result}{}(ctx",
                symbol(&module.instances[function.instance.index()])
            )
            .unwrap();
            if s.dest.is_some() {
                source.push_str(", returned_place");
            }
            for j in 0..args.len() {
                write!(source, ", a{j}").unwrap();
            }
            source.push_str(",0);\n");
        }
        if !s.results.is_empty() {
            source.push_str("uint64_t status = value.status;\n");
        }
        if observing {
            let check = if !scheduled && !host_files && case.report.is_none() {
                format!(
                    "if(peak!=UINT64_C({})) {{ fprintf(stderr,\"peak mismatch: %llu expected {}\\n\",(unsigned long long)peak); return 17; }}",
                    report.peak_reserved, report.peak_reserved
                )
            } else {
                String::new()
            };
            writeln!(
                source,
                "uint64_t peak=hirsel_core_test_observe_end(observation); {check}"
            )
            .unwrap();
        }
        source.push_str("fwrite(&status,8,1,stdout); HirselCoreCauseCell *causes = 0; if (status) { causes = hirsel_core_causes_take(ctx); uint64_t count = 0; for(const HirselCoreCauseCell *c = causes; c; c = hirsel_core_cause_next(c)) count++; fwrite(&count,8,1,stdout); for(const HirselCoreCauseCell *c = causes; c; c = hirsel_core_cause_next(c)) { uint64_t kind = hirsel_core_cause_kind(hirsel_core_cause_value(c)); fwrite(&kind,8,1,stdout); emit_test_site(hirsel_core_cause_value(c)->site); } } else {\n");
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
        let primary_refund = if host_files {
            "hirsel_core_test_primary_refund(ctx)"
        } else {
            "0"
        };
        writeln!(source,"uint64_t used = UINT64_C({limit}) - hirsel_core_test_available(ctx); fwrite(&used,8,1,stdout); uint64_t nt=traces; fwrite(&nt,8,1,stdout); fwrite(trace,8,traces,stdout); uint64_t na=hirsel_core_test_attempts(ctx); fwrite(&na,8,1,stdout); uint64_t startup_refund={primary_refund}; hirsel_core_causes_drop(causes); if(status && hirsel_core_test_available(ctx) != UINT64_C({limit}) + startup_refund) return 9; hirsel_core_context_free(ctx);").unwrap();
        if host_files {
            // Context/run ownership belongs to hirsel_core_run in this path.
            let end = source.rfind("hirsel_core_context_free(ctx);").unwrap();
            source.replace_range(end..end + "hirsel_core_context_free(ctx);".len(), "");
            source.push_str("return 0; }\n");
        } else {
            source.push_str("}\n");
        }
    }
    if host_files {
        source.push_str("int main(void) { HirselCoreProvider other={0,wrong_provider,file_deliver}; HirselCoreProcessConfig config={.root_limit=1000000,.provider=&other,.diagnostic=diagnostic};\n");
        source.push_str(&process_calls);
    }
    source.push_str("return 0; }\n");
    let actual = link_run(&scratch, &source);
    // Decode each report independently so a red row cannot hide later rows in
    // this executable. The count words include either results or causes, then
    // retained charge, the drop trace, and admission attempts.
    fn take_report<'a>(words: &mut &'a [u64]) -> Option<&'a [u64]> {
        let count = usize::try_from(*words.get(1)?).ok()?;
        let payload = count.checked_mul(if *words.first()? == 0 { 1 } else { 7 })?;
        let trace_at = payload.checked_add(3)?;
        let trace = usize::try_from(*words.get(trace_at)?).ok()?;
        let end = trace_at.checked_add(trace)?.checked_add(2)?;
        let report = words.get(..end)?;
        *words = &words[end..];
        Some(report)
    }
    let (mut actual, mut expected) = (actual.as_slice(), expected.as_slice());
    let mut failures = Vec::new();
    for case in cases {
        let function = &case.function;
        let args = case.args;
        let got = take_report(&mut actual);
        let want = take_report(&mut expected).expect("the authored report is complete");
        if got != Some(want) {
            let name = &module.instances[module.functions[function.index()].instance.index()];
            failures.push(format!(
                "case {}({args:?}) limit {} injection {:?}: expected {want:?}, got {got:?}",
                name.definition, case.limit, case.injection
            ));
        }
    }
    assert!(
        actual.is_empty() && expected.is_empty(),
        "trailing report words: {actual:?} / {expected:?}"
    );
    assert!(
        failures.is_empty(),
        "optimize={optimize}\n{}\nCoreIR:\n{}",
        failures.join("\n"),
        text::print(module)
    );
}

fn link_program(scratch: &Scratch, source: &str) -> PathBuf {
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
    // Materialize the declared header data beside the embedded root header so
    // nested includes work in both the worker sandbox and the host checkout.
    for (name, text) in [
        (
            "hirsel_core_rt.h",
            include_str!("../../../hirsel-core-rt/include/hirsel_core_rt.h"),
        ),
        (
            "hirsel_core_rt_host.h",
            include_str!("../../../hirsel-core-rt/include/hirsel_core_rt_host.h"),
        ),
        (
            "hirsel_core_rt_sites.h",
            include_str!("../../../hirsel-core-rt/include/hirsel_core_rt_sites.h"),
        ),
        (
            "hirsel_core_rt_tasks.h",
            include_str!("../../../hirsel-core-rt/include/hirsel_core_rt_tasks.h"),
        ),
    ] {
        fs::write(scratch.0.join(name), text).unwrap();
    }
    let link = Command::new(c_compiler())
        .current_dir(&scratch.0)
        .args(["caller.c", "input.o", "-I"])
        .arg(&scratch.0)
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
    scratch.0.join("run")
}

fn link_run(scratch: &Scratch, source: &str) -> Vec<u64> {
    let run = native_command(&link_program(scratch, source))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "native execution: {:?}\n{}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let actual: Vec<_> = run
        .stdout
        .chunks_exact(8)
        .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
        .collect();
    actual
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

/// Run each hand-derived sample row, its explicit limits, and both refusal
/// injection modes at every admission, using the same oracle on either ISA.
fn check_sample_with_refusals(source: &str, optimize: bool) {
    let module = text::parse(source).unwrap();
    let mut runs: Vec<_> = sample_runs(&module, source)
        .into_iter()
        .map(|run| (run, 1_000_000))
        .collect();
    for line in source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("; run limit "))
    {
        let (limit, call) = line.split_once(": ").unwrap();
        runs.extend(
            sample_runs(&module, &format!("; run: {call}"))
                .into_iter()
                .map(|run| (run, limit.parse().unwrap())),
        );
    }
    let mut native = Vec::new();
    for (run, limit) in &runs {
        native.push(NativeCase {
            function: run.0,
            args: &run.1,
            limit: *limit,
            injection: None,
            report: None,
        });
        let baseline = eval::run(&module, run.0, &run.1, *limit);
        for n in 1..=baseline.admissions {
            for later in [false, true] {
                native.push(NativeCase {
                    function: run.0,
                    args: &run.1,
                    limit: *limit,
                    injection: Some((n, later)),
                    report: None,
                });
            }
        }
    }
    execute_cases(&module, optimize, &native);
}

fn check_front_end_run_cases() {
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
    let mut failures = Vec::new();
    for file in cases {
        let source = fs::read_to_string(&file).unwrap();
        if !source.lines().any(|l| l.starts_with("// expect: run ")) {
            continue;
        }
        println!("front-end fixture: {}", file.display());
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (module, functions) = front::lower(&root, &source);
            let mut runs = Vec::new();
            // A run marked `evaluator-only:..cranelift..` (SITES.md, NOT-BUILT) must
            // still disagree here, so the marker cannot outlive the missing emission.
            let mut pending = Vec::new();
            for line in source
                .lines()
                .filter_map(|l| l.strip_prefix("// expect: run "))
            {
                let marked = line.split_whitespace().last().is_some_and(|m| {
                    m.strip_prefix("evaluator-only:")
                        .is_some_and(|e| e.split(',').any(|e| e == "cranelift"))
                });
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
                if marked {
                    pending.push((line, (f, args)));
                    continue;
                }
                runs.push((f, args));
                count += 1;
            }
            execute(&module, &runs, false);
            for (line, run) in pending {
                let agrees = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    execute(&module, &[run], false)
                }))
                .is_ok();
                assert!(
                    !agrees,
                    "`{line}` now agrees on Cranelift: remove `cranelift` from its marker and its NOT-BUILT row"
                );
            }
        }));
        if let Err(error) = attempt {
            let message = error
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| error.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            // Assembly/link failures prevent every row of this fixture from
            // running; name all of them instead of silently dropping the file.
            let rows = source
                .lines()
                .filter_map(|l| l.strip_prefix("// expect: run "))
                .map(|run| format!("{}: {run}", file.display()))
                .collect::<Vec<_>>();
            failures.push(format!("{}\n{message}", rows.join("\n")));
        }
    }
    assert!(count > 0, "the front-end corpus must execute run cases");
    println!("front-end run cases selected: {count}");
    assert!(
        failures.is_empty(),
        "front-end failures:\n{}",
        failures.join("\n\n")
    );
}

fn input_path(variable: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(variable).unwrap_or_else(|| panic!("{variable} is explicit test data")),
    );
    let path = if path.is_absolute() {
        path
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    };
    fs::canonicalize(path).unwrap()
}
fn c_compiler() -> PathBuf {
    if NATIVE_TARGET == "aarch64-unknown-linux-gnu" {
        input_path("HIRSEL_TOOLCHAIN_ARM").join("bin/aarch64-none-linux-gnu-gcc")
    } else {
        PathBuf::from("cc")
    }
}
fn native_command(binary: &std::path::Path) -> Command {
    if NATIVE_TARGET == "aarch64-unknown-linux-gnu" {
        let mut command = Command::new(input_path("HIRSEL_TOOLCHAIN_QEMU").join("qemu-aarch64"));
        command
            .arg("-L")
            .arg(input_path("HIRSEL_TOOLCHAIN_ARM").join("aarch64-none-linux-gnu/libc"))
            .arg(binary);
        command
    } else {
        Command::new(binary)
    }
}

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
        if std::thread::panicking() {
            eprintln!("native failure artifacts: {}", self.0.display());
        } else {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

fn check_c_struct_calls() {
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

fn check_failure_sites() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../core/front");
    let source = fs::read_to_string(root.join("cases/body/failure-sites.hirsel")).unwrap();
    let (module, functions) = front::lower(&root, &source);
    let runs: Vec<_> = source
        .lines()
        .filter_map(|line| line.strip_prefix("// expect: run "))
        .map(|line| {
            let call = line.split_once(" = ").unwrap().0;
            let (name, args) = call.strip_suffix(')').unwrap().split_once('(').unwrap();
            let function = functions[name];
            let signature =
                &module.signatures[module.functions[function.index()].signature.index()];
            let args = signature
                .params
                .iter()
                .zip(args.split(',').filter(|a| !a.is_empty()))
                .map(|(p, a)| {
                    let Param::Value(t) = p else {
                        panic!("scalar run")
                    };
                    eval::scalar(*t, a.trim())
                })
                .collect();
            (function, args)
        })
        .collect();
    assert_eq!(runs.len(), 9);
    for optimize in [false, true] {
        execute(&module, &runs, optimize);
    }
}

fn check_relative_debug_rows() {
    use cranelift_object::object::{Object as _, ObjectSection as _};
    let mut module = text::parse(
        r#"core v1
item i0 = test@0/sites/rows
sig g0 = (u64) -> (u64)
inst n0 = test@0/sites/rows [] []
fn f0 = n0 g0 {
b0(v0: u64):
  v1: u64 = const 1
  v2: u64 = add v0, v1
  v3: u64 = add v2, v1
  return v3
}
"#,
    )
    .unwrap();
    let body = module.functions[0].body.as_mut().unwrap();
    body.inst_anchors = [1, 7, 27]
        .map(|start| {
            Some(Anchor {
                item: ItemId(0),
                start,
                end: start + 1,
            })
        })
        .to_vec();
    body.blocks[0].anchor = Some(Anchor {
        item: ItemId(0),
        start: 39,
        end: 40,
    });
    for optimize in [false, true] {
        // Two same-kind cold edges must retain their distinct raising anchors.
        execute(
            &module,
            &[(FuncId(0), vec![u64::MAX]), (FuncId(0), vec![u64::MAX - 1])],
            optimize,
        );
        let object = emit_object(
            &module,
            &Options {
                target: NATIVE_TARGET,
                optimize,
            },
        )
        .unwrap();
        let file = cranelift_object::object::File::parse(&*object.bytes).unwrap();
        let dwarf = gimli::Dwarf::load(|id| -> Result<_, gimli::Error> {
            Ok(gimli::EndianSlice::new(
                file.section_by_name(id.name())
                    .map(|s| s.data().unwrap())
                    .unwrap_or(&[]),
                gimli::LittleEndian,
            ))
        })
        .unwrap();
        let mut units = dwarf.units();
        let unit = dwarf
            .unit(units.next().unwrap().expect("a core compilation unit"))
            .unwrap();
        let mut lines = unit
            .line_program
            .clone()
            .expect("item-relative rows")
            .rows();
        let mut mapped = Vec::new();
        while let Some((header, row)) = lines.next_row().unwrap() {
            if row.end_sequence() {
                continue;
            }
            let path = dwarf
                .attr_string(&unit, row.file(header).unwrap().path_name())
                .unwrap()
                .to_string_lossy()
                .into_owned();
            mapped.push((path, row.line().unwrap().get(), row.column()));
        }
        for line in [8, 28] {
            assert!(
                mapped
                    .iter()
                    .any(|(path, l, col)| path == "test@0/sites/rows"
                        && *l == line
                        && *col
                            == gimli::ColumnType::Column(std::num::NonZeroU64::new(1).unwrap())),
                "{mapped:?}"
            );
        }
        let mut entries = unit.entries();
        let mut subprogram = false;
        while let Some(entry) = entries.next_dfs().unwrap() {
            if entry.tag() == gimli::DW_TAG_subprogram {
                let name = dwarf
                    .attr_string(&unit, entry.attr_value(gimli::DW_AT_linkage_name).unwrap())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                subprogram |= name == symbol(&module.instances[0]);
            }
        }
        assert!(subprogram, "the actual ELF function has a subprogram entry");
    }
}
