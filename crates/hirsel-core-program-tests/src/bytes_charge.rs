#[test]
fn channel_charge_matches_admission() {
    let path = crate::repo().join("core/front/cases/body/bytes-charge.hirsel");
    let failures = crate::cases::run_case(&path);
    assert!(failures.is_empty(), "{failures:#?}");
}
