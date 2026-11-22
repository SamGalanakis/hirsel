use super::*;
use serde_json::json;

#[test]
fn rendered_edits_follow_the_current_index_on_warm_and_cold_checks() {
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
                let written = match rule {
                    "LANG-7" => "carth",
                    "LANG-9" => "grein",
                    _ => "Reedy",
                };
                let start = main.rfind(written).unwrap();
                let expected = expected.map_or_else(
                    || json!([]),
                    |name| {
                        json!([{
                            "applicability": "maybe",
                            "edits": [{
                                "module": "app@1/main",
                                "span": {"start": start, "end": start + written.len()},
                                "text": name,
                            }],
                        }])
                    },
                );
                let value = serde_json::to_value(row).unwrap();
                assert_eq!(value["fixes"], expected, "edit {step}, {rule}");
                assert!(value.get("hint").is_none());
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
                "a whole-request hit has no index from which to derive the edits",
            );
        }
    }
}

#[test]
fn unresolved_library_path_edits_name_existing_public_declarations() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn one() { let _ = time.from_unix_ms(5u64); }\n\
                  fn absent() { time.unknown() }\n\
                  fn listener(n: Net, a: Addr) waits { let _ = net.listen(n, a, 8u16); }\n";
    let mut app = request(source, dir.path()).inputs.packages.remove(0);
    app.dependencies.extend([package("core"), package("std")]);
    let mut request = CheckRequest::library_request(None, &dir.path().join("store")).unwrap();
    request.inputs.packages.push(app);
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let diagnostics = product.machine_diagnostics().unwrap();
    let fixes = |line: &str| {
        let start = source.find(line).unwrap() as u32;
        &diagnostics
            .iter()
            .find(|row| row.primary.module == "app@1/main" && row.primary.span.start == start)
            .unwrap()
            .fixes
    };
    for (written, replacement) in [
        ("time.from_unix_ms", "std.time.from_unix_ms"),
        ("net.listen", "host.listen"),
    ] {
        let [fix] = fixes(written).as_slice() else {
            panic!("expected a fix for {written}");
        };
        let [edit] = fix.edits.as_slice() else {
            panic!("expected one edit for {written}");
        };
        let start = source.find(written).unwrap();
        let mut fixed = source.to_owned();
        fixed.replace_range(edit.span.start as usize..edit.span.end as usize, &edit.text);
        assert_eq!(&fixed[start..start + replacement.len()], replacement);
    }
    assert!(fixes("time.unknown").is_empty());
}
