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

fn apply_json_fix(source: &str, fix: &serde_json::Value) -> String {
    let mut edits: Vec<(usize, usize, String)> = fix["edits"]
        .as_array()
        .expect("fix edits array")
        .iter()
        .map(|edit| {
            let start = edit["span"]["start"].as_u64().unwrap() as usize;
            let end = edit["span"]["end"].as_u64().unwrap() as usize;
            let text = edit["text"].as_str().unwrap().to_owned();
            (start, end, text)
        })
        .collect();
    edits.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in edits.windows(2) {
        assert!(
            pair[0].1 <= pair[1].0,
            "overlapping edits in one fix: {edits:?}"
        );
    }
    let mut out = source.to_owned();
    for (start, end, text) in edits.into_iter().rev() {
        out.replace_range(start..end, &text);
    }
    out
}

fn core_image() -> Arc<CoreImage> {
    static IMAGE: std::sync::OnceLock<Arc<CoreImage>> = std::sync::OnceLock::new();
    Arc::clone(IMAGE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let req = CheckRequest {
            manifest: None,
            inputs: CorpusManifest {
                core: package("core"),
                packages: vec![PackageProduct::bundled_manifest(Path::new("core/core"))],
            },
            store: dir.path().join("store"),
            library: LibraryInput::Sources,
            entry: None,
        };
        let (_, image) = Driver::default()
            .produce_image(req, &mut Meter::new(false))
            .unwrap();
        image.expect("core image")
    }))
}

fn core_request(source: &str, dir: &Path) -> CheckRequest {
    let mut req = request(source, dir);
    req.inputs.packages[0].dependencies.push(package("core"));
    req.library = LibraryInput::Image(core_image());
    req
}

fn check_single_source(source: &str) -> (Arc<CheckProduct>, Vec<serde_json::Value>) {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(core_request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let diagnostics = product.machine_diagnostics().unwrap();
    let json = serde_json::to_value(&diagnostics).unwrap();
    let rows = json.as_array().unwrap().clone();
    (product, rows)
}

fn assert_fix_compiles(source: &str, rule: &str, expected_applicability: &str) -> (String, String) {
    let (product, rows) = check_single_source(source);
    let row = rows
        .iter()
        .find(|row| row["rule"].as_str() == Some(rule))
        .unwrap_or_else(|| panic!("expected refusal {rule}, got: {rows:?}"));
    let fixes = row["fixes"].as_array().expect("fixes array");
    assert!(
        !fixes.is_empty(),
        "expected at least one fix for {rule} in:\n{source}\ndiagnostic: {row}"
    );
    let fix = &fixes[0];
    assert_eq!(
        fix["applicability"].as_str(),
        Some(expected_applicability),
        "unexpected applicability for {rule}: {fix}"
    );
    let fixed = apply_json_fix(source, fix);
    let (rechecked, remaining) = check_single_source(&fixed);
    assert!(
        rechecked.accepted() && remaining.is_empty(),
        "applying {rule} fix did not compile cleanly!\nbefore:\n{source}\nafter:\n{fixed}\nremaining: {remaining:?}"
    );
    let refusal = product.diagnostics.iter().find(|r| r.rule == rule).unwrap();
    let rendered = product.render_refusal(refusal).unwrap();
    (fixed, rendered)
}

#[test]
fn own7_fixes_apply_and_compile_cleanly() {
    // Missing `take` at a move parameter -> `safe`.
    let (fixed, rendered) = assert_fix_compiles(
        "fn eat(take xs: List[u64]) {}\nfn f(take xs: List[u64]) { eat(xs) }\n",
        "OWN-7",
        "safe",
    );
    assert!(fixed.contains("eat(take xs)"), "got: {fixed}");
    assert!(rendered.contains("insert \"take \""), "got:\n{rendered}");

    // Missing `mut` at a `mut` parameter on a writable place -> `safe`.
    let (fixed, rendered) = assert_fix_compiles(
        "fn f(mut xs: List[u64]) { list.clear(xs) }\n",
        "OWN-7",
        "safe",
    );
    assert!(fixed.contains("list.clear(mut xs)"), "got: {fixed}");
    assert!(rendered.contains("insert \"mut \""), "got:\n{rendered}");

    // Extra `mut` on a view parameter -> `safe` deletion.
    let (fixed, rendered) = assert_fix_compiles(
        "fn f(mut xs: List[u64]) -> u64 { list.len(mut xs) }\n",
        "OWN-7",
        "safe",
    );
    assert!(fixed.contains("list.len(xs)"), "got: {fixed}");
    assert!(rendered.contains("delete "), "got:\n{rendered}");

    // Missing `bytes` mark -> `safe`.
    let (fixed, _) = assert_fix_compiles(
        "fn reserve(bytes n: u64) {}\nfn f() { reserve(64) }\n",
        "OWN-7",
        "safe",
    );
    assert!(fixed.contains("reserve(bytes 64)"), "got: {fixed}");

    // Moving a view parameter -> `maybe` fixes (both `take` param and `clone` work).
    let view_param_src = "fn f(s: String) -> String { s }\n";
    let (_, rows) = check_single_source(view_param_src);
    let fixes = rows[0]["fixes"].as_array().unwrap();
    assert_eq!(
        fixes.len(),
        2,
        "expected param `take` and `clone` fixes: {fixes:?}"
    );
    for fix in fixes {
        assert_eq!(fix["applicability"].as_str(), Some("maybe"));
        let fixed = apply_json_fix(view_param_src, fix);
        let (rechecked, remaining) = check_single_source(&fixed);
        assert!(
            rechecked.accepted() && remaining.is_empty(),
            "view-param OWN-7 fix failed:\n{fixed}\nremaining: {remaining:?}"
        );
    }

    // Passing a record field to a `take` parameter must NOT suggest invalid `take m.items` (OWN-14).
    let (fixed, _) = assert_fix_compiles(
        "type Inbox { items: List[String] }\nfn eat(take xs: List[String]) {}\nfn f(m: Inbox) { eat(m.items) }\n",
        "OWN-7",
        "maybe",
    );
    assert!(fixed.contains("eat(clone(m.items))"), "got: {fixed}");

    // Missing `take` on a view parameter updates both parameter and argument (or clones).
    let (fixed, _) = assert_fix_compiles(
        "fn eat(take xs: List[u64]) {}\nfn f(xs: List[u64]) { eat(xs) }\n",
        "OWN-7",
        "maybe",
    );
    assert!(
        fixed.contains("fn f(take xs: List[u64]) { eat(take xs) }"),
        "got: {fixed}"
    );

    // Missing `mut` on an immutable local updates both `let` and call argument.
    let (fixed, _) = assert_fix_compiles(
        "fn f() { let xs: List[u64] = list.new(); list.clear(xs) }\n",
        "OWN-7",
        "maybe",
    );
    assert!(
        fixed.contains("let mut xs: List[u64] = list.new(); list.clear(mut xs)"),
        "got: {fixed}"
    );
}

#[test]
fn own5_fixes_apply_and_compile_cleanly() {
    // Use after move in `let`: `let t = s; s` -> `let t: String = clone(s); s`.
    let (fixed, rendered) = assert_fix_compiles(
        "fn g(take s: String) -> String { let t = s; s }\n",
        "OWN-5",
        "maybe",
    );
    assert!(fixed.contains("let t: String = clone(s)"), "got: {fixed}");
    assert!(rendered.contains("moved here:"), "got:\n{rendered}");
    assert!(rendered.contains("clone(s)"), "got:\n{rendered}");

    // Use after move at a call site: `eat(take xs); list.len(xs)` -> `eat(clone(xs))`.
    let (fixed, _) = assert_fix_compiles(
        "fn eat(take xs: List[u64]) {}\nfn f(take xs: List[u64]) -> u64 { eat(take xs); list.len(xs) }\n",
        "OWN-5",
        "maybe",
    );
    assert!(fixed.contains("eat(clone(xs))"), "got: {fixed}");

    // Use after move where later use is only a read offers both `clone` and `view`.
    let read_after_move = "fn f(take s: String) -> u64 { let t = s; string.byte_len(s) }\n";
    let (_, rows) = check_single_source(read_after_move);
    let fixes = rows[0]["fixes"].as_array().unwrap();
    assert_eq!(fixes.len(), 2, "expected clone and view fixes: {fixes:?}");
    for fix in fixes {
        assert_eq!(fix["applicability"].as_str(), Some("maybe"));
        let fixed = apply_json_fix(read_after_move, fix);
        let (rechecked, remaining) = check_single_source(&fixed);
        assert!(
            rechecked.accepted() && remaining.is_empty(),
            "OWN-5 fix failed:\n{fixed}\nremaining: {remaining:?}"
        );
    }

    // Immutable local lent `mut` -> `safe` insertion of `mut ` in `let`.
    let (fixed, _) = assert_fix_compiles(
        "fn f() { let xs: List[u64] = list.new(); list.push(mut xs, 1) }\n",
        "OWN-5",
        "safe",
    );
    assert!(fixed.contains("let mut xs: List[u64]"), "got: {fixed}");

    // View parameter lent `mut` -> `maybe` insertion of `mut ` on parameter.
    let (fixed, _) = assert_fix_compiles(
        "fn f(xs: List[u64]) { list.clear(mut xs) }\n",
        "OWN-5",
        "maybe",
    );
    assert!(fixed.contains("fn f(mut xs: List[u64])"), "got: {fixed}");
}

#[test]
fn own10_fixes_apply_and_compile_cleanly() {
    // View freezes mutation (unused view after mutation): scope-limiting block AND clone both compile.
    let source = "type Inbox { items: List[String] }\n\
                  fn h(mut m: Inbox) {\n\
                    let items = view m.items\n\
                    list.clear(mut m.items)\n\
                  }\n";
    let (product, rows) = check_single_source(source);
    let row = rows.iter().find(|r| r["rule"] == "OWN-10").unwrap();
    let fixes = row["fixes"].as_array().unwrap();
    assert_eq!(
        fixes.len(),
        2,
        "expected block-scope and clone fixes: {fixes:?}"
    );
    for fix in fixes {
        assert_eq!(fix["applicability"].as_str(), Some("maybe"));
        let fixed = apply_json_fix(source, fix);
        let (rechecked, remaining) = check_single_source(&fixed);
        assert!(
            rechecked.accepted() && remaining.is_empty(),
            "OWN-10 fix failed:\n{fixed}\nremaining: {remaining:?}"
        );
    }
    let rendered = product.render_refusal(&product.diagnostics[0]).unwrap();
    assert!(rendered.contains("viewed here:"), "got:\n{rendered}");
    assert!(rendered.contains("insert \"{ \""), "got:\n{rendered}");

    // When the viewed binding IS used after the mutation, do not break scope; emit `clone` fix.
    let used_after = "type Inbox { items: List[String] }\n\
                      fn h(mut m: Inbox) -> u64 {\n\
                        let items = view m.items\n\
                        list.clear(mut m.items)\n\
                        list.len(items)\n\
                      }\n";
    let (fixed, _) = assert_fix_compiles(used_after, "OWN-10", "maybe");
    assert!(
        fixed.contains("let items: List[String] = clone(m.items)"),
        "got: {fixed}"
    );

    // Same-line semicolon-separated `view` freezing a tail move.
    let (fixed, _) = assert_fix_compiles(
        "type Inbox { items: List[String] }\nfn f(take m: Inbox) -> Inbox { let v = view m.items; m }\n",
        "OWN-10",
        "maybe",
    );
    assert!(
        fixed.contains("{ let v = view m.items }; m"),
        "got: {fixed}"
    );
}

#[test]
fn cap1_fixes_apply_and_compile_cleanly() {
    // Empty parameter list: inserts `c: Clock` and passes `c`.
    let (fixed, rendered) =
        assert_fix_compiles("fn stamp() -> u64 { host.mono_ns() }\n", "CAP-1", "maybe");
    assert_eq!(fixed, "fn stamp(c: Clock) -> u64 { host.mono_ns(c) }\n");
    assert!(
        rendered.contains("insert \"c: Clock\"") && rendered.contains("; insert \"c\""),
        "got:\n{rendered}"
    );

    // Non-empty parameter list + name collision (`c` already taken): picks `clock` and adds `, `.
    let (fixed, _) = assert_fix_compiles(
        "fn stamp(c: u64) -> u64 { c + host.mono_ns() }\n",
        "CAP-1",
        "maybe",
    );
    assert_eq!(
        fixed,
        "fn stamp(c: u64, clock: Clock) -> u64 { c + host.mono_ns(clock) }\n"
    );

    // Enclosing function already has `Clock`: passes existing parameter without duplicating it.
    let (fixed, _) = assert_fix_compiles(
        "fn stamp(clk: Clock) -> u64 { host.mono_ns() }\n",
        "CAP-1",
        "maybe",
    );
    assert_eq!(fixed, "fn stamp(clk: Clock) -> u64 { host.mono_ns(clk) }\n");

    // Helper function requiring `Dir` as trailing argument after an existing argument.
    let (fixed, _) = assert_fix_compiles(
        "fn touch(p: String, d: Dir) -> Result[HostFile, IoError] waits { host.open(d, p, Read) }\n\
         fn run_touch(p: String) -> Result[HostFile, IoError] waits { touch(p) }\n",
        "CAP-1",
        "maybe",
    );
    assert!(
        fixed.contains("fn run_touch(p: String, dir: Dir)") && fixed.contains("touch(p, dir)"),
        "got: {fixed}"
    );

    // Missing `Net` and `Env` capabilities insert idiomatic `net: Net` / `env: Env`.
    let (fixed, _) = assert_fix_compiles(
        "fn open_port(n: Net) {}\nfn start() { open_port() }\n",
        "CAP-1",
        "maybe",
    );
    assert!(
        fixed.contains("fn start(net: Net) { open_port(net) }"),
        "got: {fixed}"
    );
    let (fixed, _) = assert_fix_compiles(
        "fn read_var(e: Env) {}\nfn config() { read_var() }\n",
        "CAP-1",
        "maybe",
    );
    assert!(
        fixed.contains("fn config(env: Env) { read_var(env) }"),
        "got: {fixed}"
    );
}

#[test]
fn lang11_untyped_literal_fixes_apply_and_compile_cleanly() {
    // Plain `let n = 7` -> annotates `: u64` (and `: i64` as alternative).
    let let_src = "fn f() -> u64 { let n = 7; n }\n";
    let (fixed, rendered) = assert_fix_compiles(let_src, "LANG-11", "maybe");
    assert_eq!(fixed, "fn f() -> u64 { let n: u64 = 7; n }\n");
    assert!(rendered.contains("insert \": u64\""), "got:\n{rendered}");

    // Return-type hint (`u8`) picks `: u8` first so the function compiles cleanly!
    let (fixed, _) =
        assert_fix_compiles("fn f() -> u8 {\n  let x = 1\n  x\n}\n", "LANG-11", "maybe");
    assert!(fixed.contains("let x: u8 = 1"), "got: {fixed}");

    // Negated integer literal picks signed `: i64` (never invalid `: u64`).
    let (fixed, _) = assert_fix_compiles("fn f() { let n = -7; let _ = n }\n", "LANG-11", "maybe");
    assert!(fixed.contains("let n: i64 = -7"), "got: {fixed}");

    // Float literal picks `: f64`.
    let (fixed, _) =
        assert_fix_compiles("fn f() { let x = 3.14; let _ = x }\n", "LANG-11", "maybe");
    assert!(fixed.contains("let x: f64 = 3.14"), "got: {fixed}");

    // Literal comparison operand `1 < 2` suffixes the literal `1u64 < 2`.
    let (fixed, _) = assert_fix_compiles("fn g() -> bool { 1 < 2 }\n", "LANG-11", "maybe");
    assert_eq!(fixed, "fn g() -> bool { 1u64 < 2 }\n");

    // Generic call literal argument `id(7)` suffixes `id(7u64)`.
    let (fixed, _) = assert_fix_compiles(
        "fn id[T](take x: T) -> T { x }\nfn g() { let _ = id(7) }\n",
        "LANG-11",
        "maybe",
    );
    assert!(fixed.contains("id(7u64)"), "got: {fixed}");
}

#[test]
fn report_probe_program_emits_fixes_for_all_refusal_classes_and_caches_identically() {
    let source = "\
type Inbox { items: List[String] }
fn eat(take xs: List[u64]) {}
fn f(take xs: List[u64]) { eat(xs) }
fn g(take s: String) -> String { let t = s; s }
fn h(mut m: Inbox) {
  let items = view m.items
  list.clear(mut m.items)
}
fn stamp() -> u64 { host.mono_ns() }
fn literal() -> u64 { let n = 7; n }
fn twice(x: u64) -> u64 waits { x * 2 }
";
    let dir = tempfile::tempdir().unwrap();
    let req = core_request(source, dir.path());
    let product = Driver::default()
        .check(req.clone(), &mut Meter::new(false))
        .unwrap();
    let rows = product.machine_diagnostics().unwrap();
    for rule in ["OWN-7", "OWN-5", "OWN-10", "CAP-1", "LANG-11", "LANG-8"] {
        let row = rows
            .iter()
            .find(|r| r.rule == rule)
            .unwrap_or_else(|| panic!("missing {rule}"));
        assert!(
            !row.fixes.is_empty(),
            "expected non-empty fixes for {rule}: {:?}",
            row.message
        );
    }
    // Verify request_record accepts the answer and disk-cached checks return identical fixes.
    assert!(
        crate::request_record::Value::of(crate::request_record::Answer {
            product: &product,
            diagnostics: &rows,
            incomplete: &[],
            resources: &[],
            reach: None,
            native: None,
        })
        .is_some(),
        "refusals without unresolved names are cacheable"
    );
    let disk = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(disk.rechecked.is_empty());
    assert_eq!(
        serde_json::to_value(&rows).unwrap(),
        serde_json::to_value(disk.machine_diagnostics().unwrap()).unwrap()
    );
}

#[test]
fn front_refusal_corpus_cases_emit_compiling_fixes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cases = [
        ("core/front/cases/CAP-1-explicit-authority.hirsel", "CAP-1"),
        ("core/front/cases/own/moved-read.hirsel", "OWN-5"),
        ("core/front/cases/own/view-freezes-loan.hirsel", "OWN-10"),
        ("core/front/cases/own/view-freezes-move.hirsel", "OWN-10"),
        ("core/front/cases/own/mut-needed.hirsel", "OWN-7"),
        ("core/front/cases/own/view-param-move.hirsel", "OWN-7"),
        ("core/front/cases/body/unsuffixed-let.hirsel", "LANG-11"),
    ];
    for (relative, rule) in cases {
        let source = std::fs::read_to_string(root.join(relative)).unwrap();
        let (_, rows) = check_single_source(&source);
        let row = rows
            .iter()
            .find(|r| r["rule"].as_str() == Some(rule))
            .unwrap_or_else(|| panic!("{relative}: missing {rule} in {rows:?}"));
        let fixes = row["fixes"].as_array().unwrap();
        assert!(
            !fixes.is_empty(),
            "{relative}: expected fixes for {rule}: {row}"
        );
        let fixed = apply_json_fix(&source, &fixes[0]);
        let (rechecked, remaining) = check_single_source(&fixed);
        assert!(
            rechecked.accepted() && remaining.is_empty(),
            "{relative}: fix did not compile cleanly!\nafter:\n{fixed}\nremaining: {remaining:?}"
        );
    }
}
