//! Source-to-runtime tests. Values and charges are hand-derived from CONC-4/5,
//! MEM-24 and the declaration order of Result, Closed and SendError.
use super::*;

fn canonical(source: &str) -> (NameIndex, Vec<ModuleInput>) {
    inputs_with_core(
        source,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/core"),
    )
}

#[test]
fn core_operation_declarations_do_not_drift() {
    let (index, modules) = canonical("");
    let p = Program::declare(&index, &modules);
    crate::body::core_ops::assert_declarations(&p);
}

#[test]
fn cap6_ambient_kernels_take_authority() {
    let (index, modules) = canonical("");
    let p = Program::declare(&index, &modules);
    crate::body::core_ops::assert_no_ambient_input(&p);
}
