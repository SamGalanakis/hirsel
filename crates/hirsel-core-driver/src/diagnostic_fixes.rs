use super::*;

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
        ("pub const cart: u64 = 1u64\n", &["cart"][..]),
        ("pub const carts: u64 = 1u64\n", &["carts"]),
        ("pub const wagon: u64 = 1u64\n", &[]),
        (
            "pub const wagon: u64 = 1u64\npub const carte: u64 = 2u64\n",
            &["carte"],
        ),
        (
            "pub const cart: u64 = 1u64\npub const carte: u64 = 2u64\n",
            &["cart", "carte"],
        ),
        (
            "const cart: u64 = 1u64\npub const carte: u64 = 2u64\n",
            &["carte"],
        ),
        ("const cart: u64 = 1u64\nconst carte: u64 = 2u64\n", &[]),
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
            for (rule, expected, signatures) in [
                ("LANG-7", expected, Vec::new()),
                ("LANG-9", &["grain"][..], vec!["grain: u64"]),
                ("LANG-11", &["Ready"][..], vec!["Ready"]),
            ] {
                let row = rows.iter().find(|row| row.rule == rule).unwrap();
                let written = match rule {
                    "LANG-7" => "carth",
                    "LANG-9" => "grein",
                    _ => "Reedy",
                };
                let start = main.rfind(written).unwrap() as u32;
                assert_eq!(row.fixes.len(), expected.len(), "edit {step}, {rule}");
                for (i, (fix, name)) in row.fixes.iter().zip(expected).enumerate() {
                    assert_eq!(fix.applicability, Applicability::Maybe);
                    let [edit] = fix.edits.as_slice() else {
                        panic!("expected one edit: {fix:?}");
                    };
                    assert_eq!(
                        (edit.module.as_str(), edit.text.as_str()),
                        ("app@1/main", *name)
                    );
                    assert_eq!(edit.span.start, start);
                    assert_eq!(edit.span.end, start + written.len() as u32);
                    let signature = signatures.get(i).map_or_else(
                        || format!("pub const {name}: u64"),
                        |signature| (*signature).to_owned(),
                    );
                    assert_eq!(fix.signature.as_deref(), Some(signature.as_str()));
                }
                let available: Vec<_> = row
                    .available_members
                    .iter()
                    .map(|member| member.name.as_str())
                    .collect();
                assert_eq!(
                    available,
                    if rule == "LANG-7" && step == 2 {
                        &["wagon"][..]
                    } else {
                        &[]
                    }
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
                  fn length(xs: List[u64]) -> u64 { string.len(xs) }\n\
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
        let fix = fixes(written).first().expect("expected a library fix");
        assert!(fix.signature.as_deref().unwrap().starts_with("pub fn "));
        let [edit] = fix.edits.as_slice() else {
            panic!("expected one edit for {written}");
        };
        let start = source.find(written).unwrap();
        let mut fixed = source.to_owned();
        fixed.replace_range(edit.span.start as usize..edit.span.end as usize, &edit.text);
        assert_eq!(&fixed[start..start + replacement.len()], replacement);
    }
    assert!(fixes("time.unknown").is_empty());
    let alternatives = fixes("string.len");
    assert_eq!(alternatives.len(), 3);
    assert!(alternatives.iter().any(|fix| fix.edits[0].text == "list"));
}

#[test]
fn installed_library_candidate_carries_its_signature() {
    let lib = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request("pub fn answer() -> u64 { 42 }\n", lib.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    let image = image.unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut request = request("fn caller() -> u64 { lib.asnwer() }\n", app.path());
    request.library = LibraryInput::Image(image.clone());
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let rows = product.machine_diagnostics().unwrap();
    let [row] = rows.as_slice() else {
        panic!("expected one unresolved name");
    };
    assert_eq!(row.rule, "LANG-7");
    let [fix] = row.fixes.as_slice() else {
        panic!("expected one installed library correction: {:?}", row.fixes);
    };
    assert_eq!(fix.edits[0].text, "answer");
    assert_eq!(fix.signature.as_deref(), Some("pub fn answer() -> u64"));
    assert_eq!(image.decoded_bodies(), 0);
}
