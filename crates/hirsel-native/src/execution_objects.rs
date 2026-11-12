use crate::NativeError;
use crate::ReadyProgram;
use crate::StageName;
use crate::ahead::body_frame;
use crate::ahead::inputs;
use crate::ahead::plan;
use crate::ahead::unknown_machine_calls;
use hirsel_lir::{ahead::AheadPlan, continuation::ContinuationPlans};
use hirsel_syntax::ContentHash;

/// One owner for each alternate definition and immutable root record, independent
/// of ordinary partitions, cached unit hits, and installed standard images.
fn ahead_objects(
    program: &ReadyProgram,
    backend: crate::Backend,
    opt: hirsel_lir::emission::OptLevel,
    registry: &hirsel_lir::descriptor_registry::DescriptorRegistry,
) -> Result<Vec<hirsel_lir::stage::ObjectArtifact>, NativeError> {
    use std::collections::BTreeMap;
    let plan = plan(program);
    if plan.roots.is_empty() {
        return Ok(Vec::new());
    }
    let all: Vec<_> = program.units.iter().collect();
    let units: Vec<_> = all
        .into_iter()
        .filter(|u| plan.bodies.contains_key(&u.unit().header.instance))
        .collect();
    let plans = crate::continuation::plans(program);
    let inputs = inputs(program, &plan, &plans, opt);
    let failed = |message: String| NativeError::new(StageName::Emission, message);
    let emitted = match backend {
        crate::Backend::Llvm => {
            hirsel_llvm::emit_ahead_object(&units, &inputs).map_err(|e| e.to_string())
        }
        crate::Backend::Cranelift => {
            hirsel_cranelift::emit_ahead_object(&units, &inputs).map_err(|e| e.to_string())
        }
    };
    let (mut object, frames) = emitted.map_err(failed)?;
    let frames: BTreeMap<_, _> = plan
        .bodies
        .keys()
        .filter_map(|id| Some((*id, body_frame(&plan, &plans, *id, &frames)?)))
        .collect();
    // Backend-created memcpy/division/outlined helpers have no private-work
    // summary. Refuse publication even if their ordinary use is permitted.
    let machine_helpers = unknown_machine_calls(&object, &plan, &plans, program);
    let bounds: BTreeMap<_, _> = plan
        .roots
        .iter()
        .filter_map(|(id, root)| {
            if machine_helpers {
                return None;
            }
            let [startup, getter, handoff, depth] = crate::continuation::AHEAD_HELPER_BYTES;
            hirsel_lir::ahead::stack_bound_with_depth(
                &root.graph,
                &frames,
                startup,
                getter,
                root.handoff_bytes(handoff),
                depth,
            )
            .map(|n| (*id, n))
        })
        .collect();
    let mut metadata = hirsel_cranelift::emit_ahead_metadata(&inputs, &plan, &bounds, registry)
        .map_err(|e| failed(e.to_string()))?;
    let evidence = hirsel_lir::ahead::MachineEvidence {
        plan: plan.identity,
        object: ContentHash::of_bytes(&object.bytes),
        options: object.reproducibility.options,
        frames: frames.into_iter().collect(),
        bounds: bounds.into_iter().collect(),
    };
    object.ahead = Some(evidence.clone());
    metadata.ahead = Some(evidence);
    Ok(vec![object, metadata])
}

/// Every resumable plan's state alternates and dispatcher, in one object.
fn continuation_objects(
    program: &ReadyProgram,
    backend: crate::Backend,
    opt: hirsel_lir::emission::OptLevel,
    measurement: bool,
) -> Result<Vec<hirsel_lir::stage::ObjectArtifact>, NativeError> {
    let plans = crate::continuation::plans(program);
    if !plans.resumable() {
        return Ok(Vec::new());
    }
    // Entered-runtime certificates include incoming callable provenance from
    // callers that may themselves remain stackful. Emitters check this full
    // set, then emit alternates only for Resumable bodies.
    let units = program.units.iter().collect::<Vec<_>>();
    let mut ahead = crate::ahead::plan(program);
    if !hirsel_lir::ahead::validate_cpu_workers(&units, &program.table, &program.target, &ahead) {
        return Err(NativeError::new(
            StageName::Emission,
            "invalid CPU ahead certificate",
        ));
    }
    ahead
        .spawns
        .retain(|_, root| ahead.roots.get(root).is_some_and(|root| root.cpu.is_some()));
    static EMPTY: std::collections::BTreeSet<ContentHash> = std::collections::BTreeSet::new();
    let inputs = hirsel_lir::emission::EmitInputs {
        ahead: Some(&ahead),
        continuation: Some(&plans),
        alternate: false,
        index: None,
        input: &program.input,
        target: &program.target,
        layouts: &program.table,
        manifest: &program.manifest,
        statics: &program.statics,
        inline_instances: &program.inline_instances,
        available_externally: &EMPTY,
        callbacks: !program.callbacks.is_empty(),
        tracking: program.tracking(),
        foreign: &program.foreign_dispatch,
        opt,
        measurement,

        verify_backend: program.validation == crate::Validation::EveryStage,
    };
    let object = match backend {
        crate::Backend::Cranelift => hirsel_cranelift::emit_resume_object(&units, &inputs, &plans)
            .map_err(|e| NativeError::new(StageName::Emission, e.to_string()))?,
        crate::Backend::Llvm => hirsel_llvm::emit_resume_object(&units, &inputs, &plans)
            .map_err(|e| NativeError::new(StageName::Emission, e.to_string()))?,
    };
    Ok(vec![object])
}

/// Shared execution-object producer. Ordinary batch diagnostics use these same
/// certified objects; only the request graph owns production planning and reuse.
pub fn objects(
    program: &ReadyProgram,
    backend: crate::Backend,
    opt: hirsel_lir::emission::OptLevel,
    measurement: bool,
    registry: &hirsel_lir::descriptor_registry::DescriptorRegistry,
) -> Result<Vec<hirsel_lir::stage::ObjectArtifact>, NativeError> {
    let mut objects = ahead_objects(program, backend, opt, registry)?;
    objects.extend(continuation_objects(program, backend, opt, measurement)?);
    Ok(objects)
}
