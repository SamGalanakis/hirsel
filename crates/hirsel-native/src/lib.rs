//! Native stages: the frozen snapshot is
//! adapted into the plain program the native passes read, lowered stage by
//! stage with every stage validator invoked, and the Ready program is run by
//! the checked heap executor or emitted, linked and launched.
//!
//! A native failure is a tool outcome (`NativeError`), never a source verdict.
pub mod adapt;
pub mod ahead;
pub mod c_abi;
pub use ahead::{check_objects as check_ahead_objects, plan as ahead_plan};
pub mod continuation;
pub mod emit;
pub mod executable;
pub mod exit_report;
pub mod file_digest;
pub mod foreign;
pub mod kit;
pub mod link;
pub mod object_queries;
pub mod observe;
pub mod proofs;
pub mod receipt_source;
pub mod root_source;
pub mod runner;
pub mod runtime_cache;
pub use runtime_cache::RuntimeInputs;
pub mod stages;
pub mod timeout;
pub mod unit_bodies;

pub use adapt::{input_identity, manifest, manifest_for_operations, program};
pub use emit::{
    Backend, KIND_BUG, check_runtime_calls, emit, emit_clif, emit_glue, emit_ir, failure_codes,
    failure_rules,
};
pub use executable::{BuildOptions, BuildProfile, Scratch};
pub use hirsel_lir::emission::OptLevel;
pub use hirsel_lir::layout_types::TargetSpec;
pub use hirsel_lir::profile::{ProfileSource, ProfileSources};
pub use hirsel_llvm::c_call;
pub use hirsel_proof_export::sidecar::Checker;
pub use hirsel_run_schema::Inputs as LaunchInputs;
pub use link::{
    ExecutableLinkInputs, ExecutableLinkPlan, FrozenExecutableLinkInputs, Launched, Library,
    Observed, Partition, Runnable, RuntimeBuild, SMALL_UNIT_INSTRUCTIONS, Tape, Toolchain,
    emit_backend_objects, emit_objects, fixture_run_record, launch, launch_fixture,
    launch_runnable, launch_with, link, link_fixture_runnable, link_host, link_library,
    link_objects, link_profile, link_runnable, link_standalone_objects, observe, observe_run,
    partition_backend_objects, partition_backend_objects_with_jobs, partition_objects,
    plan_executable_inputs, plan_executable_link, plan_frozen_executable_inputs,
    plan_frozen_executable_link, run_host,
};
pub use link::{runtime_archive, selected_foreign_inputs};
pub use observe::{Failure, OBSERVATION_SCHEMA, Observation, Sites, site_codes};
pub use proofs::{Evidence, Proofs, parse as parse_sidecar};
pub use runner::{Counts, Execution, Image, Mode, Runner, counts, runner};
pub use stages::{NativeError, Options, ReadyProgram, StageName, Validation};

/// The pinned LLVM release emission binds, for reports and artifacts.
pub fn llvm_version() -> String {
    hirsel_llvm::PINNED_LLVM.to_string()
}

pub fn backend_version() -> hirsel_syntax::ContentHash {
    if hirsel_llvm::NATIVE_TSAN || hirsel_cranelift::NATIVE_TSAN {
        hirsel_syntax::ContentHash::of_parts(
            "hirsel.native-tsan.v1",
            [
                hirsel_compiler_identity::COMPILER_IDENTITY.as_slice(),
                &[
                    u8::from(hirsel_llvm::NATIVE_TSAN),
                    u8::from(hirsel_cranelift::NATIVE_TSAN),
                ],
            ],
        )
    } else {
        hirsel_syntax::ContentHash::of_bytes(&hirsel_compiler_identity::COMPILER_IDENTITY)
    }
}

/// Retain immutable source ownership for line/site profiling without querying a backend.
pub fn profile_sources(input: &hirsel_engine::NativeInput) -> ProfileSources {
    profile_sources_for_owners(
        &input.sources,
        input.source_owners.iter().map(|(path, owner)| {
            let anchors = input.definitions.get(path).map_or_else(
                || vec![owner.syntax.anchor],
                |body| hirsel_tir::source_anchors(body),
            );
            (path, owner, anchors)
        }),
    )
}

fn profile_sources_for_owners<'a>(
    sources: &hirsel_span::SourceMap,
    owners: impl Iterator<
        Item = (
            &'a hirsel_syntax::DefPath,
            &'a hirsel_engine::NativeSource,
            Vec<hirsel_span::Anchor>,
        ),
    >,
) -> ProfileSources {
    owners
        .filter_map(|(definition, owner, anchors)| {
            let (file, base) = sources.anchor(owner.syntax.anchor)?;
            let file = sources.file(file)?;
            Some((
                definition.clone(),
                ProfileSource {
                    path: file.path.clone(),
                    file: file.clone(),
                    base,
                    anchors: anchors
                        .into_iter()
                        .filter_map(|anchor| {
                            let (file, base) = sources.anchor(anchor)?;
                            Some((sources.file(file)?.clone(), base))
                        })
                        .collect(),
                },
            ))
        })
        .collect()
}

pub use link::{CapturedExecutableLinkInputs, plan_captured_executable_link};

pub mod request_observation;
pub use request_observation::NativeObservation;

#[cfg(test)]
hirsel_guard::install!();

pub mod fixture;
pub use fixture::{Fixture, HeapCampaignMeasure, HeapMeasure, HeapMeasures};

pub mod execution_objects;
pub mod execution_plan;
pub mod native_metadata;
pub mod query_model;

/// Pool partition of the independent heap oracle's campaign.
pub use hirsel_heapcheck::CampaignPartition as HeapCampaignPartition;
