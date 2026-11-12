//! Retained diagnostic dispatch storage, excluding producer-owned shared data.
use hirsel_query_products::retained::MetadataWeight as Weight;

pub(crate) fn address(w: &mut Weight, value: &super::super::InstanceAddress) {
    w.path(value.instance.definition());
    w.add(value.instance.retained_encoding_bytes());
}
pub(crate) fn component(w: &mut Weight, value: &super::super::ComponentKey) {
    w.vec(&value.members, address);
    w.flat_vec(&value.edges);
}
pub(crate) fn target(w: &mut Weight, value: &hirsel_lir::TargetSpec) {
    w.add(value.triple.capacity());
    w.add(value.cpu.capacity());
    w.add(value.llvm_data_layout.capacity());
    w.vec(&value.features, |w, feature| w.add(feature.capacity()));
}
pub(crate) fn operations(value: &mut hirsel_layout::operation::OperationRegistry) -> u64 {
    let mut w = Weight::default();
    w.add(size_of_val(value) + 2 * size_of::<usize>());
    w.vec(&value.invocation_bodies, |w, row| w.path(&row.definition));
    w.packed_map(&mut value.invocations, |w, path, rows| {
        w.path(path);
        w.vec(rows, |w, row| {
            if let Some(op) = &row.primitive {
                w.path(&op.definition);
            }
            if let Some(path) = &row.callee {
                w.path(path);
            }
        });
    });
    w.packed_map(&mut value.signatures, |w, operation, _| {
        w.path(&operation.definition)
    });
    w.packed_map(&mut value.type_declarations, |_, _, _| ());
    w.packed_map(&mut value.owners.nominals, |w, _, value| {
        if let hirsel_layout::layout::OwnerRepresentation::Defined(record) = value {
            w.representation(record);
        }
    });
    w.packed_map(&mut value.owners.containers, |w, _, record| {
        w.representation(record)
    });
    w.bytes()
}
pub(crate) fn manifest(w: &mut Weight, value: &hirsel_core_contract::Manifest) {
    use hirsel_core_contract::*;
    fn cleanup(w: &mut Weight, value: &CleanupEffect) {
        if let CleanupEffect::DependsOn(values) = value {
            w.flat_vec(values);
        }
    }
    // The fixed sorted row set owns one exact-size boxed slice, with no
    // independently allocated map keys or spare node slots.
    w.add(value.len().saturating_mul(size_of::<OperationRow>()));
    for row in value.rows() {
        w.path(&row.id.definition);
        if let Some(own) = &row.ownership {
            w.vec(&own.parameters, |w, p| cleanup(w, &p.cleanup));
            cleanup(w, &own.result_cleanup);
        }
        for disposition in [
            &row.dispositions.reference,
            &row.dispositions.native,
            &row.dispositions.heap_check,
        ] {
            if let Disposition::Instruction(name) | Disposition::RuntimePrimitive(name) =
                disposition
            {
                w.add(name.capacity());
            }
        }
        w.vec(&row.vectors, |w, vector| w.add(vector.0.capacity()));
        if let Some(rule) = &row.failure {
            w.add(rule.0.capacity());
        }
    }
}

pub(crate) fn emission(w: &mut Weight, value: &super::super::products::EmissionOptions) {
    w.map(&value.foreign, |w, path, _| w.path(path));
    w.tree::<hirsel_syntax::ContentHash, ()>(value.inline_instances.len());
}
pub(crate) fn options(w: &mut Weight, value: &hirsel_native::Options) {
    target(w, &value.target);
    if let Some(scheduler) = &value.launch.scheduler {
        w.add(scheduler.algorithm.capacity());
    }
    if let Some(faults) = &value.launch.fixture_file_faults {
        w.map(faults, |w, _, queue| {
            w.add(
                queue
                    .capacity()
                    .saturating_mul(size_of::<hirsel_run_schema::FileFault>()),
            )
        });
    }
    w.map(&value.discharge.0, |w, path, sites| {
        w.path(path);
        w.tree::<hirsel_lir::discharge::Discharged, ()>(sites.len());
        for site in sites {
            match &site.form {
                hirsel_lir::discharge::Form::Check(rule)
                | hirsel_lir::discharge::Form::Projection { rule, .. } => w.add(rule.capacity()),
                _ => (),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dispatch_retention_counts_launch_and_emission_spare_capacity() {
        let mut launch =
            hirsel_native::Options::new(hirsel_lir::TargetSpec::x86_64_unknown_linux_gnu());
        let mut w = Weight::default();
        options(&mut w, &launch);
        let baseline = w.bytes();
        let mut algorithm = String::with_capacity(8192);
        algorithm.push_str("fifo");
        launch.launch.scheduler = Some(hirsel_run_schema::Scheduler {
            algorithm,
            version: 1,
            seed: 0,
        });
        let mut queue = std::collections::VecDeque::with_capacity(64);
        queue.push_back(hirsel_run_schema::FileFault {
            requested: None,
            error: None,
            partial: 0,
        });
        let retained = queue.capacity() * size_of::<hirsel_run_schema::FileFault>();
        launch.launch.fixture_file_faults = Some(std::collections::BTreeMap::from([(
            hirsel_run_schema::FileOperation::Open,
            queue,
        )]));
        let mut w = Weight::default();
        options(&mut w, &launch);
        assert!(w.bytes() >= baseline + 8192 + retained as u64);
        let mut request = crate::products::EmissionOptions::request(
            hirsel_lir::stage::InputIdentity {
                digest: hirsel_syntax::ContentHash::of_bytes(b"dispatch"),
                interface_version: 1,
            },
            hirsel_lir::emission::OptLevel::None,
            false,
            Default::default(),
            true,
            false,
        );
        let mut path = hirsel_syntax::DefPath::core("foreign");
        path.0.name.reserve(4096);
        let spare = path.0.name.capacity();
        request.foreign.insert(path, 1);
        let mut w = Weight::default();
        emission(&mut w, &request);
        assert!(w.bytes() >= spare as u64);
    }
}
