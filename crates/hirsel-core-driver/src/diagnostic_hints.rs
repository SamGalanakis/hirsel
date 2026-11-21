use super::*;
use serde_json::json;

#[test]
fn rendered_hints_follow_the_current_index_on_warm_and_cold_checks() {
    let dir = tempfile::tempdir().unwrap();
    let warm_store = dir.path().join("warm");
    let mut driver = Driver::default();
    let main = "type Other = Reedy()\n\
                fn member() -> u64 { helpers.carth }\n\
                fn field(p: helpers.Record) -> u64 { p.grein }\n\
                fn variant() -> helpers.Choice { Reedy }\n";
    let common = "pub type Darth { n: u64 }\n\
                  pub type Record { grain: u64 }\n\
                  pub type Choice = Ready | Waiting\n";
    // Each visible correction costs one edit. Darth is a type, so it
    // cannot tie with a candidate for the value position in helpers.carth.
    let edits = [
        ("pub const cart: u64 = 1u64\n", Some("cart")),
        ("pub const carts: u64 = 1u64\n", Some("carts")),
        ("pub const wagon: u64 = 1u64\n", None),
        (
            "pub const wagon: u64 = 1u64\npub const carte: u64 = 2u64\n",
            Some("carte"),
        ),
        (
            "pub const cart: u64 = 1u64\npub const carte: u64 = 2u64\n",
            None,
        ),
        (
            "const cart: u64 = 1u64\npub const carte: u64 = 2u64\n",
            Some("carte"),
        ),
        ("const cart: u64 = 1u64\nconst carte: u64 = 2u64\n", None),
    ];
    for (step, (edit, expected)) in edits.into_iter().enumerate() {
        let helpers = format!("{common}{edit}");
        let warm_request = modules_request(
            &[("main", main), ("helpers", &helpers)],
            dir.path(),
            &warm_store,
        );
        let warm = driver
            .check(warm_request.clone(), &mut Meter::new(false))
            .unwrap();
        if step > 0 {
            // The exact lookup of carth still fails, so this body is reused.
            let member = DefPath::from_full("app@1/main/member").unwrap();
            assert!(!warm.rechecked.contains(&member));
        }
        let disk = Driver::default()
            .check(warm_request.clone(), &mut Meter::new(false))
            .unwrap();
        assert!(disk.rechecked.is_empty());
        let cold = Driver::default()
            .check(
                CheckRequest {
                    store: dir.path().join(format!("cold-{step}")),
                    ..warm_request
                },
                &mut Meter::new(false),
            )
            .unwrap();
        for product in [warm, disk, cold] {
            let rows = product.machine_diagnostics().unwrap();
            assert_eq!(rows.len(), 3, "edit {step}");
            for (rule, expected) in [
                ("LANG-7", expected),
                ("LANG-9", Some("grain")),
                ("LANG-11", Some("Ready")),
            ] {
                let row = rows.iter().find(|row| row.rule == rule).unwrap();
                let value = serde_json::to_value(row).unwrap();
                assert_eq!(
                    value.get("hint"),
                    expected
                        .map(|name| json!({"kind": "did_you_mean", "replacement": name}))
                        .as_ref(),
                    "edit {step}, {rule}",
                );
                assert_eq!(
                    row.hint.as_ref().map(ToString::to_string),
                    expected.map(|name| format!("did you mean `{name}`?")),
                );
            }
            assert!(
                crate::request_record::Value::of(crate::request_record::Answer {
                    product: &product,
                    diagnostics: &rows,
                    incomplete: &[],
                    resources: &[],
                    reach: None,
                    native: None,
                })
                .is_none(),
                "a whole-request hit has no index from which to render the hints",
            );
        }
    }
}
