//! Independent RFC7396 vectors and depth/ownership controls on all three engines.
use hirsel_driver::*;
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fixtures/json-merge-patch-8-{name}.hir"))
}

#[test]
fn interpreter() {
    with_compiler_stack(|| {
        for name in ["vectors", "controls"] {
            let outcome = run_path_with_step_budget(
                &fixture(name),
                SimulationConfig::default(),
                Some(20_000_000),
                hirsel_driver::session::Policy::default(),
                &mut Vec::new(),
                &mut Vec::new(),
            )
            .unwrap_or_else(|error| panic!("{error}"));
            assert!(
                matches!(
                    outcome,
                    RunOutcome::Returned(hirsel_run_schema::Returned::Ok)
                ),
                "{name}: {outcome:?}"
            );
        }
    });
}

fn native(backend: hirsel_native::Backend) {
    with_compiler_stack(|| {
        for name in ["vectors", "controls"] {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let outcome = native::run_path_native_backend_with_validation(
                &fixture(name),
                HostLaunch::empty(),
                None,
                backend,
                native::Output::Streams {
                    stdout: &mut stdout,
                    stderr: &mut stderr,
                },
                hirsel_driver::session::Policy::default(),
                hirsel_native::Validation::EveryStage,
            )
            .unwrap_or_else(|error| panic!("{error}"))
            .unwrap_or_else(|error| panic!("{error}"));
            assert!(
                matches!(
                    outcome,
                    RunOutcome::Returned(hirsel_run_schema::Returned::Ok)
                ),
                "{name} {backend:?}: {outcome:?}; {}",
                String::from_utf8_lossy(&stderr)
            );
            assert!(stdout.is_empty());
        }
    });
}

#[test]
fn llvm() {
    native(hirsel_native::Backend::Llvm);
}
#[test]
fn cranelift() {
    native(hirsel_native::Backend::Cranelift);
}
