//! The recipes whose cleanup glue a field's cleanup reaches, and whether
//! each is reached as counted cleanup.
use hirsel_core_layout::{FieldTy, Handle};

pub(super) fn holder_drop_symbol(handle: Handle) -> &'static str {
    match handle {
        Handle::Held => "hirsel_core_held_drop",
        Handle::Task => "hirsel_core_task_drop",
        _ => "hirsel_core_receiver_drop",
    }
}

pub(super) fn holder_release_symbol(handle: Handle) -> &'static str {
    match handle {
        Handle::Held => "hirsel_core_held_release",
        Handle::Task => "hirsel_core_task_release",
        _ => "hirsel_core_receiver_release",
    }
}

pub(super) fn runtime_call_cleanup(
    callee: hirsel_core_ir::Callee,
    values: &mut std::collections::BTreeSet<usize>,
    pending: &mut Vec<(usize, bool)>,
) {
    use hirsel_core_ir::{Callee, RtOp};
    match callee {
        Callee::Runtime(RtOp::Spawn { x, r } | RtOp::SpawnUnit { x, r }) => {
            values.insert(r.index());
            pending.push((x.index(), false));
            pending.push((r.index(), false));
        }
        Callee::Runtime(RtOp::ResourceOrigin { item }) => pending.push((item.index(), false)),
        Callee::Runtime(
            RtOp::ChannelNew { item } | RtOp::LatestNew { item } | RtOp::HeldNew { item },
        ) => {
            values.insert(item.index());
            pending.push((item.index(), false));
        }
        _ => {}
    }
}

pub(super) fn cleanup_dependencies(
    field: &FieldTy,
    counted: bool,
    pending: &mut Vec<(usize, bool)>,
) {
    match field {
        FieldTy::Inline(t) => pending.push((t.index(), counted)),
        FieldTy::Box(t) => {
            pending.push((t.index(), counted));
            pending.push((t.index(), true));
        }
        FieldTy::Rc(t) | FieldTy::Arc(t) => pending.push((t.index(), true)),
        // Seq and Map storage is released by the drain from its buffer recipe
        // (LANG-19), so its elements are counted cleanup wherever it is held.
        FieldTy::Map(key, value) => {
            cleanup_dependencies(key, true, pending);
            cleanup_dependencies(value, true, pending);
        }
        FieldTy::Seq(element) | FieldTy::Columns(element) => {
            cleanup_dependencies(element, true, pending)
        }
        FieldTy::Array(element, _) => cleanup_dependencies(element, counted, pending),
        _ => {}
    }
}
