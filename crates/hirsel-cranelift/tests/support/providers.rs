//! Run FORM's provider sample and the closed operation ABI through C callbacks
//! beneath the real runtime adapters. No provider export is mocked.
use super::*;

struct Run {
    function: FuncId,
    args: Vec<u64>,
    argv: Vec<String>,
    limit: u64,
    injection: Option<(usize, bool)>,
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
    }
    out.extend([run.report.used, run.report.admissions as u64]);
    for bytes in [&run.report.stdout, &run.report.stderr] {
        out.push(bytes.len() as u64);
        out.extend(bytes.iter().map(|&b| u64::from(b)));
    }
}
fn execute_runs(module: &c::Module, runs: &[Run], optimize: bool) {
    let scratch = Scratch::new();
    let layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let object = emit_object_for_test(
        module,
        &layouts,
        &Options {
            target: NATIVE_TARGET,
            optimize,
        },
    )
    .unwrap();
    fs::write(scratch.0.join("input.o"), object.bytes).unwrap();
    let mut source = include_str!("providers.c").to_owned();
    declare_functions(&mut source, module);
    let mut main =
        String::from("int main(void) { HirselCoreProvider provider={&world,issue,deliver};\n");
    let mut want = Vec::new();
    for (i, run) in runs.iter().enumerate() {
        expected(run, &mut want);
        let f = &module.functions[run.function.index()];
        let sig = &module.signatures[f.signature.index()];
        writeln!(source,"static uint8_t entry{i}(HirselCoreContext *ctx,const HirselCoreProcessConfig *cfg,HirselCoreMainResult *out) {{ world.ctx=ctx; world.limit=UINT64_C({}); hirsel_core_test_reset_limit(ctx,world.limit);",run.limit).unwrap();
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
                HostOp::Read | HostOp::Write | HostOp::Close | HostOp::TryLock | HostOp::Unlock
            ) {
                source.push_str("world.seeding=1; HirselCoreHostRequest seed={.operation=0}; HirselCoreHostOutcome opened=hirsel_core_host_file_open(ctx,cfg->provider,&seed); require(!opened.error.tag && !opened.status); world.seeding=0;\n");
                "opened.value"
            } else {
                "1"
            };
            let mut args = vec![handle.to_owned()];
            if sig.params.len() >= 2 {
                source.push_str("HirselCoreBuffer operand={operand_bytes,3,64};\n");
                args.push("&operand".into());
            }
            if sig.params.len() == 3 {
                args.push("3".into());
            }
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
        writeln!(
            source,
            "struct r{} value={} (ctx,{},0); uint64_t values[{}]={{0}}; if(!value.status) {{",
            f.signature.0,
            symbol(&module.instances[f.instance.index()]),
            args.join(","),
            sig.results.len()
        )
        .unwrap();
        for j in 0..sig.results.len() {
            writeln!(source, "values[{j}]=(uint64_t)value.v{j};").unwrap();
        }
        source.push_str("}\n");
        if let Some((op, _)) = run.matrix
            && sig.params.len() >= 2
        {
            let len = if matches!(op, HostOp::Read | HostOp::Mkdtemp) {
                4
            } else {
                3
            };
            writeln!(source, "require(operand.data==operand_bytes && operand.capacity==64 && operand.len=={len});").unwrap();
        }
        writeln!(
            source,
            "emit(ctx,value.status,values,{}); return 0; }}",
            sig.results.len()
        )
        .unwrap();
        main.push_str(
            "{ memset(&world,0,sizeof world); HirselCoreEntryGrant grants[1]={{0,&provider,7}};\n",
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
        writeln!(main,"HirselCoreProcessConfig cfg={{.root_limit=1000000,.args=argv,.args_len={},.provider=&provider,.diagnostic=diagnostic,.capabilities=grants,.capabilities_len=1}}; require(hirsel_core_run(&cfg,entry{i})==0); }}",run.argv.len()).unwrap();
    }
    main.push_str("return 0; }\n");
    source.push_str(&main);
    assert_eq!(link_run(&scratch, &source), want, "optimize={optimize}");
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
        let admissions = report.admissions;
        runs.push(Run {
            function: f,
            args: args.clone(),
            argv,
            limit,
            injection: None,
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
fn every_host_row_preserves_c_fields_and_wait_status() {
    let mut source = String::from("core v1\n");
    for (i, op) in HostOp::ALL.into_iter().enumerate() {
        let s = RtOp::Host(op).signature();
        let ty = |t: Ty| match t {
            Ty::Ptr => "ptr",
            Ty::Scalar(Scalar::U64) => "u64",
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
        writeln!(
            source,
            "b0({}):\n  {} = call rt.{} g{i} ({}){}\n  return {}",
            input.join(","),
            output.join(","),
            op.name(),
            arguments.join(","),
            if op.waits() { " fail b1" } else { "" },
            returned.join(",")
        )
        .unwrap();
        if op.waits() {
            source.push_str("b1:\n  propagate\n");
        }
        source.push_str("}\n");
    }
    let module = text::parse(&source).unwrap();
    wf::module(&module).unwrap();
    let mut runs = Vec::new();
    for (i, op) in HostOp::ALL.into_iter().enumerate() {
        for failing in [false, true] {
            if !op.waits() && failing {
                continue;
            }
            let outcome = if failing {
                Outcome::Failed(vec!["Cancelled".into()])
            } else {
                Outcome::Returned(if op.waits() {
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
                report: eval::Report {
                    outcome,
                    used: 0,
                    peak_reserved: 0, // This independent fixture specifies outcomes/ledger, not its high water.
                    trace: vec![],
                    admissions: 0,
                    stdout: vec![],
                    stderr: vec![],
                    sites: vec![],
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
