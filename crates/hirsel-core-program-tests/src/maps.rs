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

/// The independent PROG-11 fixture exercises the cap, the one promotion, and
/// the strong-mode probe lengths on the actual emitted Map operations.
#[test]
fn grouped_map_probe_cap_and_promotion() {
    use std::fmt::Write as _;

    let keys: Vec<u64> = include_str!("../../../core/front/fixtures/map-flood-keys.txt")
        .lines()
        .map(|line| line.parse().unwrap())
        .collect();
    assert_eq!(
        (keys.len(), keys[128], keys[9999]),
        (10_000, 8543169, 655975409)
    );
    let mut encoded = String::new();
    for key in keys {
        write!(&mut encoded, "{key:08x}").unwrap();
    }
    let source = r#"
fn key_at(xs: Bytes, i: u64) -> u64 {
    let mut key: u64 = 0
    for j in 0u64 .. 8u64 {
        let c = option.unwrap(list.get(xs, i * 8 + j))
        let digit: u8 = if c < 97 { c - 48 } else { c - 87 }
        let widened: u64 = int.widen(digit)
        key = key * 16 + widened
    }
    key
}
fn probe_keys(n: u64, clone_rebuild: bool) -> u64 {
    let encoded_keys = string.as_bytes("@KEYS@")
    let mut m: Map[u64, u64] = map.new()
    if clone_rebuild {
        map.reserve(mut m, 1048576)
    }
    for i in 0u64 .. n {
        map.insert(mut m, key_at(encoded_keys, i), i + 1)
    }
    if clone_rebuild {
        let copied: Map[u64, u64] = clone(m)
        return tally(copied, encoded_keys, n)
    }
    tally(m, encoded_keys, n)
}
fn tally(m: Map[u64, u64], encoded_keys: Bytes, n: u64) -> u64 {
    let mut sum: u64 = 0
    for i in 0u64 .. n {
        match map.get(m, key_at(encoded_keys, i)) {
            Some(found) => { sum += found },
            None => { return 0 },
        }
    }
    sum
}
"#
    .replace("@KEYS@", &encoded);
    let (program, functions) = crate::SourceProgram::lower(&source, None);
    let run = |n, clone_rebuild| {
        let limit = if clone_rebuild { 1 << 26 } else { 1 << 22 };
        eval::run_map_observed(
            &program,
            functions["probe_keys"],
            &[n, u64::from(clone_rebuild)],
            limit,
            [0; 16],
        )
    };

    let (at_cap, checked) = run(128, false);
    assert_eq!(at_cap.outcome, eval::Outcome::Returned(vec![8256]));
    let stats = checked.maps.unwrap();
    assert_eq!((stats.fast_peak, stats.promotions), (8, 0));
    assert!(!stats.last_word_strong);

    let (after, checked) = run(10_000, false);
    assert_eq!(after.outcome, eval::Outcome::Returned(vec![50_005_000]));
    assert_eq!(after.used, 0);
    let stats = checked.maps.unwrap();
    assert_eq!(
        (stats.fast_peak, stats.promotions, stats.rebuild_promotions),
        (8, 1, 0)
    );
    assert!(stats.rebuilds > 0);
    assert!(stats.last_word_strong);
    assert!(stats.strong_searches >= 10_000);
    assert!((1..=4).contains(&stats.strong_peak));
    assert!(stats.strong_groups * 10 < stats.strong_searches * 11);
    println!("{stats:?}");

    // The source has 2^17 groups and these keys use two disjoint windows
    // there. The clone has just 32 groups, where all 129 share a home.
    let (cloned, checked) = run(129, true);
    assert_eq!(cloned.outcome, eval::Outcome::Returned(vec![8385]));
    let stats = checked.maps.unwrap();
    assert_eq!((stats.promotions, stats.rebuild_promotions), (1, 1));
    assert_eq!(stats.resumed_installs, 0);
    assert!(stats.last_word_strong);
}
