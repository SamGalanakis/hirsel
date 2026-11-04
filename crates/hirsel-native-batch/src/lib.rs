use hirsel_core_contract::Manifest;
use hirsel_engine::ValidatedNativeInput;
use hirsel_intern::timings::{self, Interval, Phase};
use hirsel_native::NativeError;
use hirsel_native::Options;
use hirsel_native::ReadyProgram;
use hirsel_native::StageName;
use hirsel_native::adapt;
use hirsel_native::*;
use hirsel_rc::instance::Program;
pub fn build(
    validated: &ValidatedNativeInput,
    options: &Options,
) -> Result<ReadyProgram, NativeError> {
    let plan = (
        timings::interval(Interval::Plan),
        timings::phase(Phase::Plan),
    );
    let manifest = adapt::manifest(validated.snapshot())
        .map_err(|e| NativeError::new(StageName::Manifest, e))?;
    let program = adapt::program(validated);
    drop(plan);
    let mut ready = build_program_source(
        &program,
        manifest,
        options,
        &[],
        validated.original_reader(),
    )?;
    let _plan = (
        timings::interval(Interval::Plan),
        timings::phase(Phase::Plan),
    );
    ready.bind_revisions(validated)?;
    Ok(ready)
}

/// `build` over an already adapted program.
pub fn build_program(
    program: &Program,
    manifest: Manifest,
    options: &Options,
) -> Result<ReadyProgram, NativeError> {
    build_program_roots(program, manifest, options, &[])
}

pub fn build_program_roots(
    program: &Program,
    manifest: Manifest,
    options: &Options,
    roots: &[hirsel_rc::library::Request],
) -> Result<ReadyProgram, NativeError> {
    build_program_source(program, manifest, options, roots, None)
}

fn build_program_source(
    program: &Program,
    manifest: Manifest,
    options: &Options,
    roots: &[hirsel_rc::library::Request],
    reader: Option<&hirsel_engine::NativeOriginalReader>,
) -> Result<ReadyProgram, NativeError> {
    hirsel_native::stages::build_program_with_sources(
        program,
        manifest,
        options,
        roots,
        &mut |instances, layouts| {
            hirsel_native_queries::batch_sources::capture(reader, program, instances, layouts)
        },
    )
}

pub mod std_surface;
pub use std_surface::StdSurface;
#[cfg(test)]
hirsel_guard::install!();

#[cfg(test)]
fn fixture_product(
    store: &dyn hirsel_engine::store::ResultStore,
    address: hirsel_engine::store::Address,
    action: hirsel_engine::store::Action,
    reference: hirsel_engine::store::Reference,
) -> hirsel_engine::runtime::ProductRef {
    let budget = hirsel_engine::runtime::ProcessBudget::new(64 << 20);
    let mut allowance = budget.reserve(64 << 20).unwrap();
    let archive =
        hirsel_engine::runtime::ProductArchive::open(store, reference, &mut allowance).unwrap();
    hirsel_engine::runtime::ProductRef::import_archive(address, action, archive)
}

#[cfg(test)]
#[path = "../tests/native/object_queries.rs"]
mod object_query_tests;
#[cfg(test)]
#[path = "../tests/native/root_source.rs"]
mod root_source_tests;
#[cfg(test)]
#[path = "../tests/native/scoped_profiles.rs"]
mod scoped_profile_tests;
#[cfg(test)]
#[path = "../tests/native/stages.rs"]
mod stage_tests;

#[cfg(test)]
use hirsel_engine::store::Reference;
#[cfg(test)]
use hirsel_syntax::ContentHash;
#[cfg(test)]
use std::sync::Arc;
