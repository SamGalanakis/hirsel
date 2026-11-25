//! The recipes whose cleanup glue a field's cleanup reaches, and whether
//! each is reached as counted cleanup.
use hirsel_core_layout::FieldTy;

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
