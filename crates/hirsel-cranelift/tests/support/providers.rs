//! Run FORM's provider sample and the closed operation ABI through C callbacks
//! beneath the real runtime adapters. No provider export is mocked.
use super::*;

struct Run {
    function: FuncId,
    args: Vec<u64>,
    argv: Vec<String>,
    limit: u64,
    injection: Option<(usize, bool)>,
    step_limit: Option<u64>,
    report: eval::Report,
    matrix: Option<(HostOp, bool)>,
}
fn expected(run: &Run, out: &mut Vec<u64>) {
    match &run.report.outcome {
        Outcome::Returned(values) => {
            out.extend([0, values.len() as u64]);
            out.extend(values);
        }
        Outcome::Failed(causes) => {
            out.extend([1, causes.len() as u64]);
            out.extend(causes.iter().map(|s| match s.as_str() {
                "Bounds" => 1,
                "Unwrap" => 3,
                "Cancelled" => 5,
                _ => panic!("unexpected provider cause {s}"),
            }));
        }
        Outcome::Stopped(name) => {
            out.extend([2, name.len() as u64]);
            out.extend(name.bytes().map(u64::from));
            return;
        }
    }
    out.extend([run.report.used, run.report.admissions as u64]);
    for bytes in [&run.report.stdout, &run.report.stderr] {
        out.push(bytes.len() as u64);
        out.extend(bytes.iter().map(|&b| u64::from(b)));
    }
}
fn execute_runs(module: &c::Module, runs: &[Run], optimize: bool) {
    let scratch = NativeArtifacts::new();
    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let object = emit_object_for_test(
        module,
        &layouts,
        &Options {
            target: NATIVE_TARGET,
            optimize,
            debug_lines: false,
        },
        hirsel_core_ids::ContentHash([0x90; 32]),
    )
    .unwrap();
    fs::write(scratch.path().join("input.o"), object.bytes).unwrap();
    let mut source = String::from("#include <unistd.h>\n#include <sys/wait.h>\n");
    source.push_str(include_str!("providers.c"));
    declare_functions(&mut source, module, &c::source::module_sites(module));
    let mut main = String::from(
        "int main(void) { HirselCoreProvider provider={&world,world_wait,issue,world_resume,world_abort,deliver};\nfor (int repeat=0;repeat<3;repeat++) {\n",
    );
    let mut want = Vec::new();
    for (i, run) in runs.iter().enumerate() {
        let mut output = Vec::new();
        expected(run, &mut output);
        want.push(output);
        let f = &module.functions[run.function.index()];
        let sig = &module.signatures[f.signature.index()];
        writeln!(source,"static uint8_t entry{i}(HirselCoreContext *ctx,const HirselCoreProcessConfig *cfg,HirselCoreMainResult *out) {{ world.ctx=ctx; world.limit=UINT64_C({}); hirsel_core_test_reset_limit(ctx,world.limit);",run.limit).unwrap();
        writeln!(
            source,
            "hirsel_core_test_step_observe(ctx,{},UINT64_C({}));",
            u8::from(run.step_limit.is_some()),
            run.step_limit.unwrap_or(0)
        )
        .unwrap();
        if let Some((n, later)) = run.injection {
            writeln!(
                source,
                "hirsel_core_test_inject(ctx,{n},{});",
                u8::from(later)
            )
            .unwrap();
        }
        let args = if let Some((op, failing)) = run.matrix {
            writeln!(
                source,
                "world.matrix=1; world.operation={}; world.failing={};",
                op.code(),
                u8::from(failing)
            )
            .unwrap();
            let handle = if matches!(
                op,
                HostOp::Read
                    | HostOp::Write
                    | HostOp::Close
                    | HostOp::TryLock
                    | HostOp::Unlock
                    | HostOp::Sync
                    | HostOp::NextEntries
                    | HostOp::StatFile
                    | HostOp::ReadAt
                    | HostOp::WriteAt
            ) {
                let operation = if op == HostOp::NextEntries {
                    HostOp::Entries.code()
                } else {
                    HostOp::Open.code()
                };
                let mode = if op == HostOp::WriteAt { 1 } else { 0 };
                writeln!(source,"world.seeding=1; HirselCoreHostRequest seed={{.operation={operation},.mode={mode}}}; HirselCoreHostOutcome opened=hirsel_core_host_file_open(ctx,cfg->provider,&seed); require(!opened.error.tag && !opened.status); world.seeding=0;").unwrap();
                "opened.value"
            } else {
                "1"
            };
            let mut args = vec![handle.to_owned()];
            if sig.params.contains(&Param::Value(Ty::Ptr)) {
                let cap = if op == HostOp::NextEntries { 300 } else { 64 };
                writeln!(
                    source,
                    "HirselCoreBuffer operand={{operand_bytes,3,{cap}}};"
                )
                .unwrap();
            }
            let extra: &[&str] = match op {
                HostOp::Open => &["&operand", "3"],
                HostOp::Rename => &["&operand", "&operand"],
                HostOp::SleepUntil | HostOp::SleepUntilWall => &["UINT64_C(3)"],
                HostOp::ReadAt | HostOp::WriteAt => &["UINT64_C(0x0102030405060708)", "&operand"],
                _ if sig.params.contains(&Param::Value(Ty::Ptr)) => &["&operand"],
                _ => &[],
            };
            args.extend(extra.iter().map(|arg| (*arg).to_owned()));
            args
        } else {
            run.args
                .iter()
                .enumerate()
                .map(|(j, n)| {
                    // Evaluator handles are opaque designations. Bind them to real
                    // process grants retaining the same provider-local designation.
                    if j == 0 || (run.function == FuncId(0) && j == 1) {
                        "1".into()
                    } else {
                        format!("UINT64_C({n})")
                    }
                })
                .collect()
        };
        if sig.results.is_empty() {
            writeln!(
                source,
                "uint8_t status={} (ctx,{},0); uint64_t values[0]={{0}};",
                module.identity(f.instance).symbol(),
                args.join(",")
            )
            .unwrap();
        } else {
            writeln!(
                source,
                "struct r{} value={} (ctx,{},0); uint64_t values[{}]={{0}}; if(!value.status) {{",
                f.signature.0,
                module.identity(f.instance).symbol(),
                args.join(","),
                sig.results.len()
            )
            .unwrap();
            for j in 0..sig.results.len() {
                writeln!(source, "values[{j}]=(uint64_t)value.v{j};").unwrap();
            }
            source.push_str("}\n");
        }
        if let Some((op, failing)) = run.matrix
            && sig.params.contains(&Param::Value(Ty::Ptr))
        {
            let len = match op {
                HostOp::Random if !failing => 64,
                HostOp::Read | HostOp::Mkdtemp => 4,
                _ => 3,
            };
            let cap = if op == HostOp::NextEntries { 300 } else { 64 };
            writeln!(source, "require(operand.data==operand_bytes && operand.capacity=={cap} && operand.len=={len});").unwrap();
            if op == HostOp::Random && !failing {
                source.push_str("require(operand_bytes[0]==0xa5 && operand_bytes[63]==0xa5); memcpy(operand_bytes, \"abc\", 3);\n");
            }
        }
        if sig.results.is_empty() {
            writeln!(source, "emit(ctx,status,values,0); return 0; }}").unwrap();
        } else {
            writeln!(
                source,
                "emit(ctx,value.status,values,{}); return 0; }}",
                sig.results.len()
            )
            .unwrap();
        }
        main.push_str(
            "{ memset(&world,0,sizeof world); HirselCoreEntryGrant grants[1]={{0,&provider,7,false}};\n",
        );
        if run.matrix.is_none() && run.function == FuncId(0) {
            main.push_str("grants[0].kind=4; grants[0].handle=0;\n");
        }
        writeln!(
            main,
            "HirselCoreBuffer argv[{}]={{0}};",
            run.argv.len().max(1)
        )
        .unwrap();
        for (j, arg) in run.argv.iter().enumerate() {
            let bytes = arg
                .bytes()
                .map(|b| format!("\\x{b:02x}"))
                .collect::<String>();
            writeln!(
                main,
                "argv[{j}]=(HirselCoreBuffer){{(uint8_t*)\"{bytes}\",{},0}};",
                arg.len()
            )
            .unwrap();
        }
        if matches!(run.report.outcome, Outcome::Stopped(_)) {
            begin_stop(&mut main);
        }
        writeln!(main,"HirselCoreProcessConfig cfg={{.root_limit=1000000,.args=argv,.args_len={},.provider=&provider,.diagnostic=diagnostic,.capabilities=grants,.capabilities_len=1}}; require(hirsel_core_run(&cfg,entry{i})==0);",run.argv.len()).unwrap();
        if matches!(run.report.outcome, Outcome::Stopped(_)) {
            end_stop(&mut main);
        }
        main.push_str("}\n");
    }
    main.push_str("} return 0; }\n");
    source.push_str(&main);
    let output = link_run(&scratch, &source);
    let mut output = output.as_slice();
    let mut metering = vec![None; runs.len()];
    for _ in 0..3 {
        for (i, (run, want)) in runs.iter().zip(&want).enumerate() {
            let (actual, rest) = output.split_at(want.len());
            assert_eq!(actual, want, "case {i}, optimize={optimize}");
            output = rest;
            if matches!(run.report.outcome, Outcome::Stopped(_)) {
                continue;
            }
            let (&[steps, refusal], rest) = output.split_first_chunk::<2>().unwrap();
            output = rest;
            assert!(
                steps >= run.report.step_floor,
                "case {i}, optimize={optimize}"
            );
            if run.step_limit.is_some() {
                assert_ne!(refusal, 0, "case {i} must refuse at a source point");
            }
            assert_eq!(
                *metering[i].get_or_insert((steps, refusal)),
                (steps, refusal),
                "case {i} varies within optimize={optimize}"
            );
        }
    }
    assert!(output.is_empty());
}

#[test]
fn whole_sample_in_dev_and_speed_matches_evaluator() {
    let source = include_str!("../../../hirsel-core-ir/samples/providers.core");
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    let mut runs = Vec::new();
    for line in source.lines().filter_map(|l| l.strip_prefix("; run")) {
        let (head, call) = line.split_once(':').unwrap();
        let (head, argv) = head
            .split_once("args")
            .map_or((head, Vec::new()), |(h, a)| {
                (h, a.split_whitespace().map(str::to_owned).collect())
            });
        let limit = head
            .trim()
            .strip_prefix("limit")
            .map_or(1_000_000, |n| n.trim().parse().unwrap());
        let (f, args) = sample_runs(&module, &format!("; run: {}", call.trim()))
            .pop()
            .unwrap();
        let borrowed = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let report = eval::run_args(&module, f, &args, limit, &borrowed);
        let counts = [report.steps, report.step_floor];
        for _ in 1..3 {
            let again = eval::run_args(&module, f, &args, limit, &borrowed);
            assert_eq!(counts, [again.steps, again.step_floor]);
        }
        assert!(report.steps >= report.step_floor);
        if f == FuncId(3) {
            assert_eq!(report.step_floor, args[1]);
            let meter = Some(eval::StepMeter { allowance: Some(1) });
            let refusal = eval::run_args_with_meter(&module, f, &args, limit, &borrowed, meter);
            for _ in 1..3 {
                let again = eval::run_args_with_meter(&module, f, &args, limit, &borrowed, meter);
                assert_eq!(
                    (again.steps, again.step_refusal),
                    (refusal.steps, refusal.step_refusal)
                );
            }
            assert!(refusal.step_refusal.is_some());
            runs.push(Run {
                function: f,
                args: args.clone(),
                argv: argv.clone(),
                limit,
                injection: None,
                step_limit: Some(1),
                report: refusal,
                matrix: None,
            });
        }
        let admissions = report.admissions;
        runs.push(Run {
            function: f,
            args: args.clone(),
            argv,
            limit,
            injection: None,
            step_limit: None,
            report,
            matrix: None,
        });
        for n in 1..=admissions {
            for later in [false, true] {
                let injection = Some((n, later));
                runs.push(Run {
                    function: f,
                    args: args.clone(),
                    argv: vec![],
                    limit,
                    injection,
                    step_limit: None,
                    report: eval::run_injected(&module, f, &args, limit, injection),
                    matrix: None,
                });
            }
        }
    }
    // Full-width indices and a non-ASCII argv carrier are outside the small sample.
    for (index, argv) in [(u64::MAX, vec!["prog", "hello"]), (1, vec!["", "α"])] {
        let args = vec![0, 0, index];
        let report = eval::run_args(&module, FuncId(0), &args, 1000, &argv);
        runs.push(Run {
            function: FuncId(0),
            args,
            argv: argv.into_iter().map(str::to_owned).collect(),
            limit: 1000,
            injection: None,
            step_limit: None,
            report,
            matrix: None,
        });
    }
    println!("provider sample runs per profile: {}", runs.len());
    for optimize in [false, true] {
        execute_runs(&module, &runs, optimize);
    }
}

#[test]
fn every_host_row_preserves_c_fields_and_status() {
    let mut source = String::from("core v1\n");
    // FORM #50's file, directory, stream, random and clock C callbacks.
    // The capability-specific adapters have separate fixtures and cannot be
    // driven by the mock Dir grant and common request below.
    const CALLBACK_ROWS: &[HostOp] = &[
        HostOp::Open,
        HostOp::Read,
        HostOp::Write,
        HostOp::Close,
        HostOp::TryLock,
        HostOp::Unlock,
        HostOp::Mkdir,
        HostOp::Mkdtemp,
        HostOp::Mkstemp,
        HostOp::Remove,
        HostOp::RemoveTree,
        HostOp::WallMs,
        HostOp::MonoNs,
        HostOp::Stdout,
        HostOp::Stderr,
        HostOp::Rename,
        HostOp::SleepUntil,
        HostOp::SleepUntilWall,
        HostOp::Sync,
        HostOp::SyncDir,
        HostOp::Random,
        HostOp::Stopped,
        HostOp::OpenDir,
        HostOp::Entries,
        HostOp::NextEntries,
        HostOp::Stat,
        HostOp::StatFile,
        HostOp::ReadAt,
        HostOp::WriteAt,
    ];
    for (i, op) in CALLBACK_ROWS.iter().copied().enumerate() {
        let s = RtOp::Host(op).signature();
        let ty = |t: Ty| match t {
            Ty::Ptr => "ptr",
            Ty::Scalar(Scalar::U64) => "u64",
            Ty::Scalar(Scalar::I64) => "i64",
            Ty::Scalar(Scalar::U32) => "u32",
            Ty::Scalar(Scalar::U8) => "u8",
            _ => unreachable!(),
        };
        let params = s
            .params
            .iter()
            .map(|p| match p {
                Param::Value(t) => ty(*t),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        let results = s.results.iter().map(|&t| ty(t)).collect::<Vec<_>>();
        writeln!(source,"sig g{i} = ({}) -> ({})\ninst n{i} = test@0/provider_abi/{} [] []\nfn f{i} = n{i} g{i} {{",params.join(","),results.join(","),op.name()).unwrap();
        let input = params
            .iter()
            .enumerate()
            .map(|(j, t)| format!("v{j}: {t}"))
            .collect::<Vec<_>>();
        let output = results
            .iter()
            .enumerate()
            .map(|(j, t)| format!("v{}: {t}", params.len() + j))
            .collect::<Vec<_>>();
        let arguments = (0..params.len())
            .map(|j| format!("v{j}"))
            .collect::<Vec<_>>();
        let returned = (0..results.len())
            .map(|j| format!("v{}", params.len() + j))
            .collect::<Vec<_>>();
        let call = if results.is_empty() {
            format!(
                "call rt.{} g{i} ({}){}",
                op.name(),
                arguments.join(","),
                " fail b1"
            )
        } else {
            format!(
                "{} = call rt.{} g{i} ({}){}",
                output.join(","),
                op.name(),
                arguments.join(","),
                " fail b1"
            )
        };
        let return_line = if returned.is_empty() {
            "return".to_owned()
        } else {
            format!("return {}", returned.join(","))
        };
        writeln!(
            source,
            "b0({}):\n  {}\n  {}",
            input.join(","),
            call,
            return_line
        )
        .unwrap();
        source.push_str("b1:\n  propagate\n");
        source.push_str("}\n");
    }
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let mut runs = Vec::new();
    for (i, op) in CALLBACK_ROWS.iter().copied().enumerate() {
        for failing in [false, true] {
            // The dedicated sleep export is a replay-delivered cancellation
            // point; this generic provider matrix injects only host_op rows.
            if op == HostOp::SleepUntil && failing {
                continue;
            }
            let outcome = if failing {
                Outcome::Failed(vec!["Cancelled".into()])
            } else {
                Outcome::Returned(if matches!(op, HostOp::Random | HostOp::Stopped) {
                    vec![]
                } else if matches!(op, HostOp::Stat | HostOp::StatFile) {
                    vec![0x1011121314151617, 5, 0x89abcdef, 0xa122232425262728, 0x9a]
                } else if op.waits() {
                    vec![0x1011121314151617, 5, 0x89abcdef, 0x2122232425262728]
                } else {
                    vec![0x1011121314151617]
                })
            };
            runs.push(Run {
                function: FuncId(i as u32),
                args: vec![],
                argv: vec![],
                limit: 1000,
                injection: None,
                step_limit: None,
                report: eval::Report {
                    steps: 0,
                    step_floor: 0,
                    step_refusal: None,
                    outcome,
                    used: 0,
                    peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
                    trace: vec![],
                    admissions: 0,
                    stdout: vec![],
                    stderr: vec![],
                    sites: vec![],
                    events: vec![],
                },
                matrix: Some((op, failing)),
            });
        }
    }
    println!("provider ABI runs per profile: {}", runs.len());
    for optimize in [false, true] {
        execute_runs(&module, &runs, optimize);
    }
}
