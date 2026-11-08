//! Strict Reader vectors adapted from serde_json 20572a6d (MIT/Apache-2.0).
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

const SOURCE: &str = r###"
// serde_json 20572a6d src/read.rs:515-567; tests/test.rs:1086-1140,
// MIT/Apache-2.0. Adapt expected failures to Hirsel's existing byte offsets,
// whole-input UTF-8 precedence, and rejection of all decoded duplicate keys.
type X { x: f64 }
fn x_from_json_reader(mut cursor: json.Reader) -> Result[unique X, String] uses {} = comptime derive.from_json_reader(shape(X))
fn x_from_json_bytes(data: Bytes) -> Result[unique X, String] uses {} = comptime derive.from_json_bytes(shape(X))
type Point { x: f64, y: f64, z: f64 }
fn point_from_json_reader(mut cursor: json.Reader) -> Result[unique Point, String] uses {} = comptime derive.from_json_reader(shape(Point))
fn point_from_json_bytes(data: Bytes) -> Result[unique Point, String] uses {} = comptime derive.from_json_bytes(shape(Point))
type Label { label: String }
fn label_from_json_reader(mut cursor: json.Reader) -> Result[unique Label, String] uses {} = comptime derive.from_json_reader(shape(Label))
fn label_from_json_bytes(data: Bytes) -> Result[unique Label, String] uses {} = comptime derive.from_json_bytes(shape(Label))
// A nominal reader may replace the public cursor's input. Retained unknown
// names belong to the input at their read, even when later fields change it.
type Child { ok: bool }
fn child_from_json_reader(mut cursor: json.Reader) -> Result[unique Child, String] uses {} {
  derive.read_null(mut cursor)?
  cursor.data = b"{\"bravo\":0,\"child\":null,\"alpha\":0}"
  Ok(Child(ok: true))
}
type Parent { child: Child }
fn parent_from_json_reader(mut cursor: json.Reader) -> Result[unique Parent, String] uses {} = comptime derive.from_json_reader(shape(Parent))
fn parent_from_json_bytes(data: Bytes) -> Result[unique Parent, String] uses {} = comptime derive.from_json_bytes(shape(Parent))
fn validate(data: Bytes) -> Result[(), json.JsonError] uses {} {
  let mut cursor = json.reader(data)
  json.skip(mut cursor)?
  json.finish_reader(mut cursor)
}
fn syntax(t: Test, source: String, position: u64) -> () {
  test.equal(t, validate(string.as_bytes(source)), Error(Syntax(position)))
  let mut cursor = json.reader(string.as_bytes(source))
  test.equal(t, json.read_owned_text(mut cursor), Error(Syntax(position)))
}
pub fn main(t: Test) -> Result[(), ()] {
  syntax(t, "\"", 1)
  syntax(t, "\"lol", 4)
  syntax(t, "\"\\uD83C\\uFFFF\"", 9)
  syntax(t, "\"\n\"", 1)
  syntax(t, "\"\u{1f}\"", 1)
  syntax(t, "\"\\u003", 6)
  syntax(t, "\"\\uD800\"", 7)
  syntax(t, "\"\\uDC00\"", 3)
  syntax(t, "\"\\uD800\\u", 9)
  syntax(t, "\"\\q\"", 2)
  syntax(t, "\"plain\\", 7)
  test.equal(t, validate(b"\"lol\"a"), Error(Syntax(5)))
  let texts: List[(String, String)] = [
    ("\"\"", ""),
    ("\"plain\"", "plain"),
    ("\"é𝄞\"", "é𝄞"),
    ("\"\\u00e9\\uD834\\uDD1E\"", "é𝄞"),
    ("\"prefix\\\"\\\\suffix\"", "prefix\"\\suffix"),
    ("\"é\\n𝄞\"", "é\n𝄞")
  ]
  for (input, expected) in texts {
    let mut cursor = json.reader(string.as_bytes(input))
    let Ok(value) = json.read_text(mut cursor) else { return Error(()) }
    test.check(t, json.text_is(value, expected))
    test.equal(t, json.text_owned(value), expected)
    test.equal(t, json.finish_reader(mut cursor), Ok(()))
    let mut owned = json.reader(string.as_bytes(input))
    let Ok(owned_value) = json.read_owned_text(mut owned) else { return Error(()) }
    test.equal(t, owned_value, expected)
    test.equal(t, json.finish_reader(mut owned), Ok(()))
  }
  // Hand-derived IEEE encodings guard token boundaries, negative zero and
  // nearest-even rounding when numeric grammar and conversion share a scan.
  let numbers: List[(Bytes, u64, u32)] = [
    (b"0]", 0u64, 0u32),
    (b"-0,", 0x8000000000000000u64, 0x80000000u32),
    (b"0.125}", 0x3fc0000000000000u64, 0x3e000000u32),
    (b" 1.5e+2,", 0x4062c00000000000u64, 0x43160000u32),
    (b"-2.5e-1]", 0xbfd0000000000000u64, 0xbe800000u32),
    (b"0.1,", 0x3fb999999999999au64, 0x3dcccccdu32),
    (b"9007199254740993]", 0x4340000000000000u64, 0x5a000000u32),
    (b"2.4703282292062327e-324]", 0u64, 0u32),
    (b"1x", 0x3ff0000000000000u64, 0x3f800000u32)
  ]
  for (data, wide, narrow) in numbers {
    let mut cursor = json.reader(data)
    let Ok(value) = derive.read_json_f64(mut cursor) else { return Error(()) }
    test.equal(t, core@1/f64.bits(value), wide)
    test.equal(t, cursor.pos, bytes.len(data) - 1)
    let mut other = json.reader(data)
    let Ok(small) = derive.read_json_f32(mut other) else { return Error(()) }
    test.equal(t, core@1/f32.bits(small), narrow)
    test.equal(t, other.pos, bytes.len(data) - 1)
  }
  let malformed_numbers: List[(Bytes, u64)] = [
    (b"01,", 1), (b"-01]", 2), (b"00.1]", 1),
    (b"1.,", 2), (b"1e+]", 3), (b"1e-]", 3), (b"-]", 1)
  ]
  for (data, at) in malformed_numbers {
    let mut cursor = json.reader(data)
    test.equal(t, derive.read_json_f64(mut cursor), Error("not JSON"))
    test.equal(t, cursor.pos, at)
    let mut other = json.reader(data)
    test.equal(t, derive.read_json_f32(mut other), Error("not JSON"))
    test.equal(t, other.pos, at)
  }
  let Ok(p) = point_from_json_bytes(string.as_bytes("{\"x\":0.125,\"y\":0.5,\"z\":0.25,\"name\":\"é\",\"opts\":{\"1\":[1,true]}}")) else { return Error(()) }
  test.equal(t, p.x, 0.125)
  test.equal(t, p.y, 0.5)
  test.equal(t, p.z, 0.25)
  let Ok(x) = x_from_json_bytes(b"{\"\\u0078\":0.5,\"opts\":{},\"nested\":{\"x\":1}}") else { return Error(()) }
  test.equal(t, x.x, 0.5)
  let Ok(label) = label_from_json_bytes(b"{\"label\":\"\\u00e9\\uD834\\uDD1E\"}") else { return Error(()) }
  test.equal(t, label.label, "é𝄞")
  test.equal(t, x_from_json_bytes(b"{}"), Error("missing field `x`"))
  let Ok(reordered) = point_from_json_bytes(b"{\"z\":0.25, \"y\" :0.5,\"x\":0.125}") else { return Error(()) }
  test.equal(t, (reordered.x, reordered.y, reordered.z), (0.125, 0.5, 0.25))
  let duplicate_positions: List[(Bytes, u64)] = [
    (b"{\"x\":1,\"x\" ?}", 10),
    (b"{\"x\":1,\"\\u0078\" ?}", 15)
  ]
  for (input, position) in duplicate_positions {
    let mut duplicate_cursor = json.reader(input)
    test.equal(t, x_from_json_reader(mut duplicate_cursor), Error("not JSON"))
    test.equal(t, duplicate_cursor.pos, position)
  }
  test.equal(t, parent_from_json_bytes(b"{\"alpha\":0,\"child\":null,\"alpha\":0}"), Error("not JSON"))
  let duplicates: List[String] = [
    "{\"x\":1\"x\":2}",
    "{\"x\":1 \"a\":null}",
    "{\"x\":1,}",
    "{\"x\":1,\"x\":2}",
    "{\"x\":1,\"\\u0078\":2}",
    "{\"x\":1,\"a\":0,\"a\":1}",
    "{\"x\":1,\"a\":0,\"\\u0061\":1}",
    // Duplicates of either inline key, in both escaped/plain directions,
    // before and after spilling later keys into the hashed tracker.
    "{\"x\":1,\"a\":0,\"b\":0,\"a\":1}",
    "{\"x\":1,\"a\":0,\"b\":0,\"\\u0062\":1}",
    "{\"x\":1,\"\\u0061\":0,\"b\":0,\"a\":1}",
    "{\"x\":1,\"a\":0,\"\\u0062\":0,\"b\":1}",
    "{\"x\":1,\"é\":0,\"b\":0,\"\\u00e9\":1}",
    "{\"x\":1,\"a\":0,\"𝄞\":0,\"\\uD834\\uDD1E\":1}",
    "{\"x\":1,\"a\":0,\"b\":0,\"c\":0,\"d\":0,\"a\":1}",
    "{\"x\":1,\"a\":0,\"b\":0,\"c\":0,\"d\":0,\"b\":1}",
    "{\"x\":1,\"a\":0,\"b\":0,\"c\":0,\"d\":0,\"\\u0063\":1}",
    "{\"x\":1,\"a\":0,\"b\":0,\"c\":0,\"d\":0,\"\\u0064\":1}",
    "{\"x\":1,\"a\":0,\"b\":0,\"c\":0,\"c\" ?}",
    "{\"x\":1,\"opts\":{\"a\":0,\"\\u0061\":1}}",
    "{\"x\":1,\"opts\":{\"é\":0,\"\\u00e9\":1}}",
    "{\"x\":1,\"opts\":{\"𝄞\":0,\"\\uD834\\uDD1E\":1}}",
    "{\"x\":1,\"opts\":{\"a\":0,\"a\" ?}}",
    "{\"x\":1} trailing"
  ]
  for source in duplicates {
    match x_from_json_bytes(string.as_bytes(source)) {
      Error(e) => test.equal(t, e, "not JSON"),
      Ok(_) => return Error(()),
    }
  }
  test.equal(t, validate(b"{\"a\":0, \"\\u0061\" ?}"), Error(DuplicateKey(8)))
  test.equal(t, validate(b"{\"a\":0, \"a\":1}"), Error(DuplicateKey(8)))
  let different_keys: List[Bytes] = [
    b"{\"x\":1}",
    b"{\"x\":1,\"a\":null}",
    b"{\"x\":1,\"a\":null,\"b\":false}",
    b"{\"x\":1,\"a\":null,\"b\":false,\"c\":[]}",
    b"{\"x\":1,\"a\":null,\"b\":false,\"c\":[],\"d\":{\"a\":1,\"b\":2}}",
    b"{\"a\":{},\"b\":{},\"c\":{},\"x\":1}"
  ]
  for data in different_keys {
    let Ok(value) = x_from_json_bytes(data) else { return Error(()) }
    test.equal(t, value.x, 1.0)
  }
  let mut wide: List[u8] = []
  text.append(mut wide, "{\"x\":1")
  for i in 0u64..64 { text.append(mut wide, f",\"unknown{i}\":{i}") }
  text.append(mut wide, "}")
  let Ok(value) = x_from_json_bytes(bytes.from_list(copy wide)) else { return Error(()) }
  test.equal(t, value.x, 1.0)
  let size = list.len(wide)
  wide[size - 1] = 44
  text.append(mut wide, "\"unknown63\":0}")
  test.equal(t, x_from_json_bytes(bytes.from_list(take wide)), Error("not JSON"))
  test.equal(t, validate(b"{}"), Ok(()))
  test.equal(t, validate(b"{\"a\":{},\"b\":{\"a\":0}}"), Ok(()))
  let invalid: List[List[u8]] = [
    [34,159,146,150,34],
    [34,92,110,159,146,150,34],
    [34,92,117,250,48,51,48,34],
    [34,92,117,48,250,51,48,34],
    [34,92,117,48,48,250,48,34],
    [34,92,117,48,48,51,250,34],
    [34,192,175,34],
    [34,237,160,128,34],
    [34,244,144,128,128,34]
  ]
  for bad in invalid {
    let data = bytes.from_list(copy bad)
    test.equal(t, json.valid_utf8(data), false)
    match x_from_json_bytes(data) {
      Error(e) => test.equal(t, e, "not UTF-8"),
      Ok(_) => return Error(()),
    }
  }
  let mut nested: List[u8] = []
  for _ in 0u64..128 { text.append(mut nested, "[") }
  for _ in 0u64..128 { text.append(mut nested, "]") }
  test.equal(t, validate(bytes.from_list(copy nested)), Ok(()))
  let mut deeper: List[u8] = [91]
  bytes.append(mut deeper, bytes.from_list(take nested))
  list.push(mut deeper, 93)
  test.equal(t, validate(bytes.from_list(take deeper)), Error(Nesting(128)))
  Ok(())
}
"###;

#[test]
fn strict_vectors_on_h_llvm_and_cranelift() {
    with_compiler_stack(|| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../conformance/SEM/SEM-3-json-corpus-run.hir");
        let mut loaded =
            load_program_source(&path, Some(SOURCE)).unwrap_or_else(|error| source_error(error));
        loaded.check_for_execution().unwrap_or_else(|error| {
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
        });
        // Wide unknown-field fixtures exceed the corpus's 1M-step budget.
        loaded.engine.adapters.interpreter = Some(Box::new(InterpreterAdapter {
            limits: hirsel_interp::Limits {
                steps: Some(5_000_000),
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
            .prefix("json-reader-11-")
            .tempdir()
            .unwrap();
        let tools =
            hirsel_native_testing::toolchain_for_matrix(&scratch.path().join("runtime"), &configs)
                .unwrap();
        for (config, outcome) in hirsel_native_testing::differential(
            &validated,
            &loaded.entry,
            &expected,
            &configs,
            &tools,
            scratch.path(),
        ) {
            assert!(
                matches!(outcome, hirsel_native_testing::Outcome::Agree),
                "{config}: {outcome:?}"
            );
        }
        eprintln!("json_reader_11: strict vectors executed on H, LLVM and Cranelift");
    });
}
