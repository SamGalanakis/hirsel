use crate::cases::inputs_with_core;
use hirsel_core_front::Program;
use hirsel_core_ir::eval;
fn core_dir() -> std::path::PathBuf {
    crate::repo().join("core/core")
}

/// Capacity and charge stay bounded by peak live entries even after mass removal.
/// This overlay only exposes the private capacity word to the law's source.
#[test]
fn churn_capacity_and_order() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    for file in std::fs::read_dir(core_dir()).unwrap() {
        let path = file.unwrap().path();
        if path.extension().is_some_and(|e| e == "hirsel") {
            std::fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
        }
    }
    let map = dir.join("map.hirsel");
    let mut source = std::fs::read_to_string(&map).unwrap();
    source.push_str(include_str!(
        "../../../core/front/fixtures/map-test-capacity.hirsel"
    ));
    std::fs::remove_file(&map).unwrap();
    std::fs::write(map, source).unwrap();
    let (index, inputs) = inputs_with_core(
        include_str!("../../../core/front/fixtures/map-churn.hirsel"),
        dir,
    );
    let p = Program::declare(&index, &inputs);
    assert!(
        p.refusals.is_empty() && p.gaps.is_empty(),
        "{:?} {:?}",
        p.refusals,
        p.gaps
    );
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &inputs, |d| d.package_is("app", "1"));
    for (peak, live) in [(1, 1), (3, 3), (8, 8), (33, 33), (33, 1)] {
        let report = eval::run(&module, functions["churn"], &[peak, live], 1 << 20);
        assert_eq!(
            report.outcome,
            eval::Outcome::Returned(vec![live]),
            "peak{peak} live{live}"
        );
        assert_eq!(report.used, 0);
        // At most the power-of-two growth sequence through max(4,4*peak).
        let max_admissions = 2 + (64 - (4 * peak).max(4).leading_zeros()) as usize;
        assert!(
            report.admissions <= max_admissions,
            "admissions{} bound{}",
            report.admissions,
            max_admissions
        );
    }
    let report = eval::run(&module, functions["no_room"], &[], 1 << 20);
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![134]));
    assert_eq!(report.used, 0);
    assert_eq!(
        report.admissions, 2,
        "only the bound and initial four entries admit; compaction admits nothing"
    );
}
