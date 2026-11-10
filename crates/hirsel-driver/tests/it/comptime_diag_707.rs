use hirsel_driver::session::{Policy, Session};
use hirsel_engine::{CheckOutput, QueryError, ToolError};
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_query_products::LoweringPolicy;
use hirsel_requests::{CompileOperation, CompileResult};

fn check(case: &str) -> Result<CheckOutput, ToolError> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(format!("conformance/CT/CT-5-generated-{case}-bad.hir"));
    check_path(&path)
}

fn check_path(path: &std::path::Path) -> Result<CheckOutput, ToolError> {
    let mut session = Session::open(path, Policy::default());
    match session.request(CompileOperation::Check {
        lowering: LoweringPolicy::Reachable,
    }) {
        Ok(output) => {
            let CompileResult::Check(checked) = &output.result else {
                panic!("expected Check")
            };
            Ok(checked.clone())
        }
        Err(ToolError::Query {
            error: QueryError::Diagnostics(diagnostics),
            sources,
            ..
        }) => Ok(CheckOutput {
            diagnostics,
            sources,
            completion: hirsel_engine::CheckCompletion::Complete,
            unsafe_ffi: vec![],
        }),
        Err(error) => Err(error),
    }
}

#[test]
fn generated_private_type_refusals_retain_the_selecting_source_and_text() {
    with_compiler_stack(|| {
        for (case, call) in [("client-private", "gen.width"), ("opaque-shape", "width")] {
            let checked = check(case).unwrap_or_else(|error| panic!("{case}: {error}"));
            let diagnostic = checked
                .diagnostics
                .iter()
                .find(|d| d.rule.as_str() == "PROG-2")
                .unwrap_or_else(|| panic!("{case}: {:?}", checked.diagnostics));
            assert_eq!(
                checked.sources.slice(diagnostic.primary),
                Some(call),
                "{}",
                diagnostic.render_human(&checked.sources)
            );
            assert!(
                diagnostic
                    .notes
                    .iter()
                    .any(|note| note.contains("generated text span"))
            );
            let rendered = diagnostic.render_human(&checked.sources);
            assert!(!rendered.contains("<unavailable>"), "{case}: {rendered}");
            assert!(
                rendered.contains(&format!("CT-5-generated-{case}-bad.hir:")),
                "{rendered}"
            );
        }
    });
}

#[test]
fn generated_text_failures_keep_the_selecting_call_through_every_front_phase() {
    with_compiler_stack(|| {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        for (expression, rule) in [
            ("if", "LANG-1"),
            ("missing", "LANG-4"),
            ("false", "LANG-7"),
            ("let _ = take _t.key\\n7", "OWN-8"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let source = r#"type Point { key: String }
fn body[T](s: TypeShape[T]) -> Result[Body[fn(take T) -> u64 uses {}], String] uses {} {
  let name = type_shape.info(s).name
  Ok(codegen.body(f"fn(take _t: {name}) -> u64 uses {{}} {{ GENERATED_EXPRESSION }}"))
}
fn generated[T](take value: T) -> u64 uses {} = comptime body(shape(T))
pub fn main() -> u8 {
  let _ = generated(Point(key: "x"))
  0u8
}
"#
            .replace("GENERATED_EXPRESSION", expression);
            std::fs::write(dir.path().join("app.hir"), source).unwrap();
            let manifest = serde_json::json!({
                "schema":1,"package":{"name":"diagnostic","version":"0"},
                "sources":[{"path":"app.hir","module":[]}],
                "entry":{"module":[],"name":"main"},
                "dependencies":[{"manifest":root.join("stdlib/std@1/hirsel.json"),
                    "package":{"name":"std","version":"1"},"expose":[]}]
            });
            std::fs::write(dir.path().join("hirsel.json"), manifest.to_string()).unwrap();
            let checked = check_path(&dir.path().join("hirsel.json"))
                .unwrap_or_else(|error| panic!("{expression}: {error}"));
            let diagnostic = checked
                .diagnostics
                .iter()
                .find(|d| d.rule.as_str() == rule)
                .unwrap_or_else(|| panic!("{expression}: {:?}", checked.diagnostics));
            let rendered = diagnostic.render_human(&checked.sources);
            assert_eq!(
                checked.sources.slice(diagnostic.primary),
                Some("generated"),
                "{rendered}"
            );
            assert!(
                diagnostic
                    .notes
                    .iter()
                    .any(|note| note.contains("generated") && note.contains("span")),
                "{rendered}"
            );
            assert!(
                diagnostic
                    .secondary
                    .iter()
                    .any(|label| label.message.contains("generated by")),
                "{rendered}"
            );
            assert!(!rendered.contains("<unavailable>"), "{rendered}");
        }
    });
}
