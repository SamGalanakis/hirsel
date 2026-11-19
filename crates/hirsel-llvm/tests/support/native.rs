trait NativeInput {
    fn module(&self) -> &Module;
    fn site_bindings(&self) -> String { String::new() }
    fn object(&self, target: Target, optimization: Optimization, observing: bool) -> Vec<u8>;
    fn verify(&self, target: Target, observing: bool) {
        verify_ir(&emit_ir_for_target(self.module(), target, observing).unwrap());
    }
}
impl NativeInput for Module {
    fn module(&self) -> &Module { self }
    fn object(&self, target: Target, optimization: Optimization, observing: bool) -> Vec<u8> {
        emit_object_for_target(self, target, optimization, observing).unwrap()
    }
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
fn execute(input: &impl NativeInput, cases: &[Case]) -> Vec<NativeReport> {
    execute_with_bindings(input, cases, "")
}
fn execute_with_bindings(input: &impl NativeInput, cases: &[Case], bindings: &str) -> Vec<NativeReport> {
    execute_with_configuration(input, cases, bindings, "")
}
fn execute_with_configuration(
    input: &impl NativeInput,
    cases: &[Case],
    bindings: &str,
    configuration: &str,
) -> Vec<NativeReport> {
    execute_optimized(input, cases, bindings, configuration, Optimization::None)
}
fn execute_optimized(
    input: &impl NativeInput,
    cases: &[Case],
    bindings: &str,
    configuration: &str,
    optimization: Optimization,
) -> Vec<NativeReport> {
    execute_object(input, cases, bindings, configuration, optimization, None)
}
fn execute_object(
    input: &impl NativeInput,
    cases: &[Case],
    bindings: &str,
    configuration: &str,
    optimization: Optimization,
    object: Option<Vec<u8>>,
) -> Vec<NativeReport> {
    execute_target_object(
        input,
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
    input: &impl NativeInput,
    cases: &[Case],
    bindings: &str,
    configuration: &str,
    optimization: Optimization,
    object: Option<Vec<u8>>,
    target: Target,
) -> Vec<NativeReport> {
    execute_target_object_checked(
        input,
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
    input: &impl NativeInput,
    cases: &[Case],
    bindings: &str,
    configuration: &str,
    optimization: Optimization,
    object: Option<Vec<u8>>,
    target: Target,
    memcheck: bool,
) -> Vec<NativeReport> {
    let module = input.module();
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
    input.verify(target, observing);
    let bytes = object.unwrap_or_else(|| {
        input.object(target, optimization, observing)
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
    if !bindings.contains("hirsel_core_site_") { c.push_str(&input.site_bindings()); }
    c.push_str(bindings);
    c.push_str(r#"
extern void hirsel_core_test_reset_limit(HirselCoreContext *, uint64_t);
static HirselCoreEvent arc_events[4096]; static size_t arc_count;
static void record_event(void *state, HirselCoreEvent e) {
    (void)state;
    if (e.kind>=HIRSEL_CORE_EVENT_ARC_NEW && e.kind<=HIRSEL_CORE_EVENT_ARC_UNWRAP) { assert(arc_count<4096); arc_events[arc_count++]=e; }
}
static size_t file_closes, file_deliveries;
static HirselCoreHostOutcome file_legacy_issue(void *state, HirselCoreHostRequest *r) {
    (void)state; HirselCoreHostOutcome out={0};
    if (r->operation==0) out.value=r->handle;
    else { assert(r->operation==3); file_closes++; hirsel_core_test_drop_trace(r->handle); out.status=1; out.error.tag=5; out.error.code=5; }
    return out;
}
static HirselCoreStep file_issue(void *state, HirselCoreHostRequest *r, uint64_t token) { (void)token; HirselCoreStep s={0}; s.outcome=file_legacy_issue(state,r); return s; }
static HirselCoreHostOutcome file_deliver(void *state, HirselCoreHostRequest *r, HirselCoreHostOutcome out) {
    (void)state; if(r->operation==3) file_deliveries++; return out;
}
static HirselCoreWait file_wait(void *s,HirselCoreHostRequest *r,uint64_t t) { (void)s;(void)r;(void)t; return (HirselCoreWait){0,0}; }
static HirselCoreStep file_resume(void *s,HirselCoreHostRequest *r,uint64_t t) { (void)s;(void)r;(void)t; __builtin_abort(); }
static HirselCoreHostOutcome file_abort(void *s,HirselCoreHostRequest *r,uint64_t t,uint8_t why) { (void)s;(void)r;(void)t;(void)why; __builtin_abort(); }
static void prepare_files(HirselCoreContext *ctx, uint64_t last) {
    HirselCoreProvider provider={.wait=file_wait,.issue=file_issue,.resume=file_resume,.abort=file_abort,.deliver=file_deliver};
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
            writeln!(c,"uint64_t b{k}_{j}=0; memcpy(&b{k}_{j},&r{k}.r{j},{}); printf(\" %llu\",(unsigned long long)b{k}_{j});",match t { Ty::Ptr => 8, Ty::Scalar(s) => s.size() as u64 }).unwrap();
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
    for symbol in ["hirsel_core_free_value", "hirsel_core_block_payer"] {
        if bindings.contains(&format!("__wrap_{symbol}")) {
            compiler.arg(format!("-Wl,--wrap={symbol}"));
        }
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
fn agree(input: &impl NativeInput, cases: &[(FuncId, Vec<u64>)]) {
    let cases = cases
        .iter()
        .map(|(function, args)| Case {
            function: *function,
            args: args.clone(),
            limit: u64::MAX,
            injection: None,
        })
        .collect::<Vec<_>>();
    agree_cases(input, &cases, false);
}
/// Every direct call's result extension is its callee's: LLVM leaves a
/// disagreement undefined (a caller relying on high bits its callee never zeroed).
fn call_results_match_callees(ir: &str) {
    let split = |line: &str, key: &str| -> Option<(String, bool, bool)> {
        let rest = &line[line.find(key)? + key.len()..];
        let at = rest.find(" @")?;
        let name = rest[at + 2..].split('(').next()?;
        let head = &rest[..at];
        (!name.contains([',', ')', ' '])).then(|| {
            (
                name.to_owned(),
                head.contains("zeroext"),
                head.contains("signext"),
            )
        })
    };
    let callees = ir
        .lines()
        .filter_map(|l| split(l, "define ").or_else(|| split(l, "declare ")))
        .map(|(name, z, s)| (name, (z, s)))
        .collect::<std::collections::HashMap<_, _>>();
    for line in ir.lines() {
        if let Some((name, z, s)) = split(line, "call ")
            && let Some(&callee) = callees.get(&name)
        {
            assert_eq!(
                (z, s),
                callee,
                "result extension of a call to @{name}: {line}"
            );
        }
    }
}
fn agree_cases(input: &impl NativeInput, cases: &[Case], memory: bool) {
    let module = input.module();
    call_results_match_callees(&emit_ir(module).unwrap());
    let got = execute(input, cases);
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
            assert_eq!(
                got.used,
                expected.used,
                "used f{} args {:?} limit {} injection {:?}; native trace {:?}, expected trace {:?}, native admissions {}, expected admissions {}",
                case.function.0,
                case.args,
                case.limit,
                case.injection,
                got.trace,
                expected.trace,
                got.admissions,
                expected.admissions
            );
            assert_eq!(got.trace, expected.trace, "trace");
            assert_eq!(got.admissions, expected.admissions, "admissions");
        }
    }
}

enum RelativeDebugRow {
    Authored {
        item: hirsel_core_ids::DefPath,
        start: u32,
    },
    Generated,
}

impl RelativeDebugRow {
    fn parse(path: &str, line: Option<std::num::NonZeroU64>) -> Option<Self> {
        if path == "<generated>" {
            return Some(Self::Generated);
        }
        let line = line?;
        let item = hirsel_core_ids::DefPath::from_full(path)
            .unwrap_or_else(|| panic!("not an item identity: {path}"));
        Some(Self::Authored {
            item,
            start: (line.get() - 1) as u32,
        })
    }
}

fn assert_relative_debug_rows(
    bytes: &[u8],
    module: &Module,
    expected_inline_sites: &[(hirsel_core_ids::DefPath, u32)],
) {
    use hirsel_core_ir::source::CodeRange;
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
    let debug = assert_native_debug_product(bytes);
    let file = object::File::parse(bytes).unwrap();
    let adapter_symbols = module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, function)| {
            let definition = &module.instances[function.instance.index()].definition;
            definition.0.name.contains("$callable[")
                && function.body.as_ref().is_some_and(|body| {
                    body.inst_anchors.iter().all(Option::is_none)
                        && body.blocks.iter().all(|block| block.anchor.is_none())
                })
        })
        .map(|(i, _)| symbol(module, FuncId(i as u32)))
        .collect::<std::collections::HashSet<_>>();
    let text_sections = file
        .sections()
        .filter(|section| section.kind() == object::SectionKind::Text)
        .map(|section| section.index())
        .collect::<Vec<_>>();
    let adapter_ranges: Vec<_> = file
        .symbols()
        .filter(|s| s.kind() == object::SymbolKind::Text && s.size() != 0 && !s.is_undefined())
        .filter(|s| adapter_symbols.contains(s.name().unwrap().split('.').next().unwrap()))
        .map(|s| CodeRange {
            section: text_sections
                .iter()
                .position(|&section| Some(section) == s.section_index())
                .unwrap() as u32,
            begin: s.address(),
            end: s.address() + s.size(),
        })
        .collect();
    // Relocated rows carry a section index. Raw rows retain the synthetic
    // filename, which the debug product represents as an absent source.
    let mut machine_rows = debug.sequences.iter().flat_map(|sequence| {
        sequence
            .rows
            .iter()
            .map(move |row| (sequence.range.section, row))
    });
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
            let (section, machine) = machine_rows.next().unwrap();
            let Some(entry) = row.file(header) else {
                assert!(row.line().is_none(), "debug row has no file");
                continue;
            };
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
            let (item, start) = match RelativeDebugRow::parse(&path, row.line()) {
                Some(RelativeDebugRow::Authored { item, start }) => (item, start),
                Some(RelativeDebugRow::Generated) => {
                    assert!(
                        adapter_ranges.iter().any(|range| {
                            range.section == section
                                && (range.begin..range.end).contains(&machine.offset)
                        }),
                        "generated row outside an adapter at {section}:{:#x}",
                        machine.offset
                    );
                    assert!(machine.source.is_none());
                    continue;
                }
                None => continue,
            };
            let item = module.items.iter().position(|p| *p == item).unwrap();
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
            let source = machine.source.unwrap();
            assert_eq!(
                (&debug.items[source.item as usize], source.start),
                (&module.items[item], start)
            );
            mapped += 1;
        }
        offset += 4 + length;
    }
    assert!(mapped > 0, "no item-relative machine rows");
    assert!(machine_rows.next().is_none(), "extra relocated debug rows");

    assert_adapter_inline_sites(&debug, &adapter_ranges, expected_inline_sites);
}

fn assert_adapter_inline_sites(
    debug: &hirsel_core_ir::source::NativeDebug,
    adapter_ranges: &[hirsel_core_ir::source::CodeRange],
    expected_inline_sites: &[(hirsel_core_ids::DefPath, u32)],
) {
    use hirsel_core_ir::source::ScopeKind;
    let mut inline_sites = std::collections::HashSet::new();
    for scope in debug
        .scopes
        .iter()
        .filter(|scope| scope.kind == ScopeKind::Inline)
    {
        let Some(declaration) = scope
            .origin
            .and_then(|origin| debug.scopes[origin as usize].declaration)
        else {
            continue;
        };
        let item = &debug.items[declaration.item as usize];
        for range in &scope.ranges {
            if !adapter_ranges.iter().any(|adapter| {
                adapter.section == range.section
                    && adapter.begin <= range.begin
                    && range.end <= adapter.end
            }) {
                continue;
            }
            // A row covers bytes up to the next row or the end of its sequence.
            let mut has_authored_row = false;
            for sequence in debug
                .sequences
                .iter()
                .filter(|sequence| sequence.range.section == range.section)
            {
                for (index, row) in sequence.rows.iter().enumerate() {
                    let end = sequence
                        .rows
                        .get(index + 1)
                        .map_or(sequence.range.end, |next| next.offset);
                    if row.offset.max(range.begin) < end.min(range.end)
                        && let Some(source) = row.source
                        && source.item == declaration.item
                    {
                        has_authored_row = true;
                        inline_sites.insert((item, source.start));
                    }
                }
            }
            assert!(
                has_authored_row,
                "inline range {range:?} in an adapter has no row for {item}"
            );
        }
    }
    for (item, start) in expected_inline_sites {
        assert!(
            inline_sites.contains(&(item, *start)),
            "lost inlined operation at {item}:{start}"
        );
    }
}

fn assert_native_debug_product(bytes: &[u8]) -> hirsel_core_ir::source::NativeDebug {
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
    debug
}
