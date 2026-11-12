//! Rust core f6c1107f dec2flt/parse.rs structure and coretests vectors (MIT).
//! Python Fraction oracle uses exact decimal rationals; Rust independently checks
//! both widths. Hirsel's stricter grammar and finite overflow errors are retained.
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_intern::jobs::with_compiler_stack;
use std::path::PathBuf;

fn source_error(error: ToolError) -> ! {
    if let ToolError::Query {
        error: QueryError::Diagnostics(diagnostics),
        sources,
        ..
    } = &error
    {
        for diagnostic in diagnostics {
            eprintln!("{}", diagnostic.render_human(sources));
        }
    }
    panic!("{error}")
}

#[test]
fn independent_bits_and_strict_grammar_on_h_llvm_and_cranelift() {
    with_compiler_stack(|| {
        let rows: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/float-parse-scan-13/bits.json"))
                .unwrap();
        let mut vectors = String::from("let vectors: List[(String, u64, u32)] = [\n");
        for row in rows.as_array().unwrap() {
            let text = row["text"].as_str().unwrap();
            let wide = row["f64"].as_u64().unwrap();
            let narrow = u32::try_from(row["f32"].as_u64().unwrap()).unwrap();
            assert_eq!(text.parse::<f64>().unwrap().to_bits(), wide, "f64 {text}");
            assert_eq!(text.parse::<f32>().unwrap().to_bits(), narrow, "f32 {text}");
            vectors += &format!("({text:?}, {wide}u64, {narrow}u32),\n");
        }
        vectors += r#"]
  for (text, wide, narrow) in vectors {
    if wide & 9218868437227405312u64 == 9218868437227405312u64 {
      test.equal(t, f64.parse(text), Error(OutOfRange))
      test.equal(t, f64.parse_bytes(string.as_bytes(text)), Error(OutOfRange))
    } else {
      let Ok(value) = f64.parse(text) else { return Error(()) }
      test.equal(t, core@1/f64.bits(value), wide)
      let Ok(borrowed) = f64.parse_bytes(string.as_bytes(text)) else { return Error(()) }
      test.equal(t, core@1/f64.bits(borrowed), wide)
    }
    if narrow & 2139095040u32 == 2139095040u32 {
      test.equal(t, f32.parse(text), Error(OutOfRange))
      test.equal(t, f32.parse_bytes(string.as_bytes(text)), Error(OutOfRange))
    } else {
      let Ok(value) = f32.parse(text) else { return Error(()) }
      test.equal(t, core@1/f32.bits(value), narrow)
      let Ok(borrowed) = f32.parse_bytes(string.as_bytes(text)) else { return Error(()) }
      test.equal(t, core@1/f32.bits(borrowed), narrow)
    }
  }
"#;
        let mut insertions = String::from("let invalid_insertions: List[String] = [\n");
        for text in [
            "123",
            "666.0",
            "0.1",
            "5e1",
            "7e-3",
            "0.0e+1",
            "123456789",
            "0.123456789012",
            "123456789012345678901",
        ] {
            for bad in ['r', ',', '?', '<', 'j', '/', ':', 'é'] {
                for at in 0..=text.len() {
                    let mut input = text.to_owned();
                    input.insert(at, bad);
                    insertions += &format!("{input:?},\n");
                }
            }
        }
        insertions += "]\nfor text in invalid_insertions { test.equal(t, f64.parse(text), Error(Invalid)); test.equal(t, f32.parse(text), Error(Invalid)) }\n";
        let source = include_str!("../fixtures/float-parse-scan-13/controls.hir")
            .replace("// BIT_VECTORS", &vectors)
            .replace("// INVALID_INSERTIONS", &insertions);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/std_numeric.hir");
        let mut loaded =
            load_program_source(&path, Some(&source)).unwrap_or_else(|error| source_error(error));
        loaded
            .check_for_execution()
            .unwrap_or_else(|error| source_error(error));
        // These fixed Rust-hosted vectors include several 1200-digit exact
        // fallbacks. Like reference_vectors, run this bounded batch without a
        // corpus interpreter step budget.
        loaded.engine.adapters.interpreter = Some(Box::new(InterpreterAdapter {
            limits: hirsel_interp::Limits {
                steps: None,
                call_depth: 256,
                source: None,
            },
            ..Default::default()
        }));
        let reference = run_observed_snapshot(
            &mut loaded.engine,
            &RunConfiguration {
                entry: loaded.entry.clone(),
                hash_seed: 0,
                inputs: vec![],
            },
            SimulationConfig::default(),
        )
        .unwrap();
        assert!(
            matches!(
                reference.outcome,
                Ok(RunOutcome::Returned(hirsel_run_schema::Returned::Ok))
            ),
            "{:?}",
            reference.outcome
        );
        let expected =
            hirsel_native::Observation::of_reference(reference.observation.as_ref().unwrap());
        let input = loaded.engine.export_native_input().unwrap();
        let validated = loaded.engine.import_native_input(input).unwrap();
        let options =
            hirsel_native::Options::new(hirsel_native::TargetSpec::x86_64_unknown_linux_gnu());
        let configs = vec![
            hirsel_native_testing::Configuration {
                options: options.clone(),
                engine: hirsel_native_testing::Engine::Native {
                    opt: hirsel_native::OptLevel::Speed,
                    partition: hirsel_native::Partition::Whole,
                    runtime: hirsel_native_testing::Runtime::Diagnostic,
                },
            },
            hirsel_native_testing::Configuration {
                options,
                engine: hirsel_native_testing::Engine::Cranelift {
                    partition: hirsel_native::Partition::Whole,
                    runtime: hirsel_native_testing::Runtime::Diagnostic,
                },
            },
        ];
        let scratch = tempfile::Builder::new()
            .prefix("float-parse-scan-13-")
            .tempdir()
            .unwrap();
        let tools =
            hirsel_native_testing::toolchain_for_matrix(&scratch.path().join("runtime"), &configs)
                .unwrap();
        for (config, outcome) in hirsel_native_testing::differential_ready(
            &mut *hirsel_requests::ready_supplier(&path, Some(&source), &loaded.entry, &tools)
                .unwrap(),
            &loaded.entry,
            &expected,
            &configs,
            &tools,
            scratch.path(),
            &hirsel_native::Fixture::default(),
        ) {
            assert!(
                matches!(outcome, hirsel_native_testing::Outcome::Agree),
                "{config}: {outcome:?}"
            );
        }
        eprintln!(
            "float_parse_scan_13: {} independent numeric vectors, both widths, H/LLVM/Cranelift",
            rows.as_array().unwrap().len()
        );
    });
}
