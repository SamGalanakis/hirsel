//! `hirsel run --native`: the checked program, optimized, run as an image in the host's
//! runner (or, when it calls C, as an executable).
use super::NativeError;
use hirsel_engine::{RunOutcome, ToolError};
use std::io::Write;
use std::path::Path;

/// Output destinations for a user launch. Inheriting the two OS streams preserves
/// the program's cross-stream write order; custom writers receive each stream.
pub enum Output<'a> {
    Inherit {
        json: bool,
    },
    Streams {
        stdout: &'a mut dyn Write,
        stderr: &'a mut dyn Write,
    },
}

impl Output<'_> {
    /// The same destinations, for one launch.
    pub(crate) fn reborrow(&mut self) -> Output<'_> {
        match self {
            Output::Inherit { json } => Output::Inherit { json: *json },
            Output::Streams { stdout, stderr } => Output::Streams {
                stdout: &mut **stdout,
                stderr: &mut **stderr,
            },
        }
    }
}

/// Check `path` exactly as `run` does (the same first static diagnostics),
/// then build it for x86-64 Linux with every optimization, link it against
/// the in-tree runtime and launch it under the empty-grant record, as a launch
/// without a record. The executable's output is written to `stdout` and its
/// failure chain to `stderr`.
///
/// The outer error is a source or tool error of the checker; the inner one is
/// a native tool outcome (a stage does not support the program yet), which is
/// never a verdict about the source.
pub fn run_path_native(
    path: &Path,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<Result<RunOutcome, NativeError>, ToolError> {
    run_path_native_with_config(
        path,
        crate::HostLaunch::empty(),
        None,
        Output::Streams { stdout, stderr },
    )
}

/// `run_path_native` with a host launch and PS-12 evidence; the proof report goes to
/// `stderr` before the program runs. Record and replay pass to the executable as
/// `--hirsel-record` and `--hirsel-replay`; the driver binds the run first, so a
/// recording of other code refuses before building.
pub fn run_path_native_with_config(
    path: &Path,
    launch: crate::HostLaunch<'_>,
    evidence: Option<&super::Evidence>,
    output: Output<'_>,
) -> Result<Result<RunOutcome, NativeError>, ToolError> {
    run_path_native_backend(
        path,
        launch,
        evidence,
        super::Backend::Llvm,
        output,
        crate::session::Policy::default(),
    )
}

/// `run_path_native_with_config` with `backend`: LLVM builds at O2, Cranelift
/// unoptimized, for a fast debug loop.
pub fn run_path_native_backend(
    path: &Path,
    launch: crate::HostLaunch<'_>,
    evidence: Option<&super::Evidence>,
    backend: super::Backend,
    output: Output<'_>,
    policy: crate::session::Policy,
) -> Result<Result<RunOutcome, NativeError>, ToolError> {
    run_path_native_backend_with_validation(
        path,
        launch,
        evidence,
        backend,
        output,
        policy,
        super::Validation::Ready,
    )
}

/// Run with an explicit LIR validation policy. Harnesses use `EveryStage`.
pub fn run_path_native_backend_with_validation(
    path: &Path,
    launch: crate::HostLaunch<'_>,
    evidence: Option<&super::Evidence>,
    backend: super::Backend,
    output: Output<'_>,
    policy: crate::session::Policy,
    validation: super::Validation,
) -> Result<Result<RunOutcome, NativeError>, ToolError> {
    run_path_native_backend_with_trace_events(
        path, launch, evidence, backend, output, policy, validation, None,
    )
}

/// Native launch with a host-owned canonical Ordered trace destination. The
/// runtime validates qualification before entry and writes only complete evidence.
pub fn run_path_native_backend_with_trace_events(
    path: &Path,
    launch: crate::HostLaunch<'_>,
    evidence: Option<&super::Evidence>,
    backend: super::Backend,
    output: Output<'_>,
    policy: crate::session::Policy,
    validation: super::Validation,
    trace_events: Option<&Path>,
) -> Result<Result<RunOutcome, NativeError>, ToolError> {
    run_native(
        path,
        launch,
        evidence,
        backend,
        output,
        policy,
        validation,
        trace_events,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_native(
    path: &Path,
    launch: crate::HostLaunch<'_>,
    evidence: Option<&super::Evidence>,
    backend: super::Backend,
    output: Output<'_>,
    policy: crate::session::Policy,
    validation: super::Validation,
    trace_events: Option<&Path>,
) -> Result<Result<RunOutcome, NativeError>, ToolError> {
    crate::session::with_session(path, policy, |session| {
        crate::program_record::run(
            session,
            path,
            policy,
            crate::program_record::Run {
                launch,
                evidence,
                backend,
                validation,
                output,
                trace_events,
            },
        )
    })
}

#[cfg(test)]
#[path = "concurrent_tests.rs"]
mod concurrent_tests;

#[cfg(test)]
pub(super) fn before_request_link(path: &Path) {
    concurrent_tests::before_link(path);
}
