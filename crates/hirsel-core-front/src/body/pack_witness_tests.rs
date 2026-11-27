use super::*;

fn push_len_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&u32::try_from(s.len()).unwrap().to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn push_str_list(out: &mut Vec<u8>, list: &[&str]) {
    out.extend_from_slice(&u32::try_from(list.len()).unwrap().to_le_bytes());
    for s in list {
        push_len_str(out, s);
    }
}

fn encode_graph_node(
    spelling: &str,
    closure: Option<ContentHash>,
    args: &[&str],
    params: &[&str],
    results: &[&str],
    fields: &[&str],
) -> Vec<u8> {
    let mut node = Vec::new();
    push_len_str(&mut node, spelling);
    match closure {
        Some(h) => {
            node.push(1);
            node.extend_from_slice(&h.0);
        }
        None => node.push(0),
    }
    push_str_list(&mut node, args);
    push_str_list(&mut node, params);
    push_str_list(&mut node, results);
    push_str_list(&mut node, fields);
    node
}

fn encode_graph(mut nodes: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    nodes.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut out = Vec::new();
    out.extend_from_slice(&u32::try_from(nodes.len()).unwrap().to_le_bytes());
    for (_, bytes) in nodes {
        out.extend_from_slice(&bytes);
    }
    out
}

#[test]
fn fixed_recursive_and_export_vectors_match_spec_framing_and_survive_renumbering() {
    let source = concat!(
        "type Tree { visit: fn(Tree) }\n",
        "type Unrelated { number: u64 }\n",
        "type Left { n: u64 }\n",
        "type Right { n: u64 }\n",
        "fn api_take(take x: u64) {}\n",
        "fn api_view(x: u64) -> u64 { x }\n",
        "fn api_left(x: Left) -> u64 { x.n }\n",
        "fn api_right(x: Right) -> u64 { x.n }\n",
        "fn api_sync() -> u64 { 0 }\n",
        "fn api_waits() -> u64 waits { 0 }\n",
        "fn api_region(r: Region) -> Text in r { core.finish(result.unwrap(core.build_in(r, bytes 64, scoped fn(mut d: Dest[Text]) -> Result[Child[Text], BuildError] { core.emit(mut d, \"\") }))) }\n",
        "fn rows(tree: Tree, padding: Unrelated, left: Left, right: Right) {}\n",
    );
    let (index, modules) = crate::tests::program_inputs(source);
    let p = Program::declare(&index, &modules);
    assert!(
        p.refusals.is_empty() && p.gaps.is_empty(),
        "{:?} {:?}",
        p.refusals,
        p.gaps
    );
    let find_fn = |name: &str| p.fns.iter().find(|fd| fd.path.0.name == name).unwrap();
    let api_take = find_fn("api_take");
    let api_view = find_fn("api_view");
    let api_left = find_fn("api_left");
    let api_right = find_fn("api_right");
    let api_sync = find_fn("api_sync");
    let api_waits = find_fn("api_waits");
    let api_region = find_fn("api_region");
    let rows = find_fn("rows");

    // Independent spec-derived oracle for scalar/unit constants (PACK-16).
    let c_u64 = ContentHash::of_parts("hirsel.core-closure.v1", [b"u64".as_slice()]);
    let c_unit = ContentHash::of_parts("hirsel.core-closure.v1", [b"()".as_slice()]);

    let expected_held_u64 = {
        let g = encode_graph(vec![(
            "u64",
            encode_graph_node("u64", Some(c_u64), &[], &[], &[], &[]),
        )]);
        ContentHash::of_parts("hirsel.held-type.v1", [b"u64".as_slice(), &c_u64.0, &g])
    };
    let expected_held_unit = {
        let g = encode_graph(vec![(
            "()",
            encode_graph_node("()", Some(c_unit), &[], &[], &[], &[]),
        )]);
        ContentHash::of_parts("hirsel.held-type.v1", [b"()".as_slice(), &c_unit.0, &g])
    };
    // Adapter signature bytes for `fn(take u64)->()`:
    // params=[Value(U64)] => 01 00 00 09, results=[] => 00, dest=None => 00, never=false => 00.
    let expected_sig_u64_to_unit = ContentHash::of_parts(
        "hirsel.code-signature.v2",
        [&[0x01, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00][..]],
    );
    let expected_api_take = ContentHash::of_parts(
        "hirsel.pack-export.v1",
        [
            b"fn(take u64)->()".as_slice(),
            &expected_held_u64.0,
            &expected_held_unit.0,
            &expected_sig_u64_to_unit.0,
        ],
    );

    let mut region_hashes = Vec::new();
    let mut view_hashes = Vec::new();
    for preallocate in [false, true] {
        let mut e = Emitter::new();
        let mut t = Local::new(&p.types);
        if preallocate {
            e.type_row(&p, &mut t, rows.params[1].ty).unwrap();
        }
        let tree_row = e.type_row(&p, &mut t, rows.params[0].ty).unwrap();
        let c_tree = e.module.closure(tree_row).hash;
        let expected_tree = {
            let g = encode_graph(vec![
                (
                    "()",
                    encode_graph_node("()", Some(c_unit), &[], &[], &[], &[]),
                ),
                (
                    "app@1/main/Tree",
                    encode_graph_node(
                        "app@1/main/Tree",
                        Some(c_tree),
                        &[],
                        &[],
                        &[],
                        &["fn(app@1/main/Tree)->()"],
                    ),
                ),
                (
                    "fn(app@1/main/Tree)->()",
                    encode_graph_node(
                        "fn(app@1/main/Tree)->()",
                        None,
                        &[],
                        &["app@1/main/Tree"],
                        &["()"],
                        &[],
                    ),
                ),
            ]);
            ContentHash::of_parts(
                "hirsel.held-type.v1",
                [b"app@1/main/Tree".as_slice(), &c_tree.0, &g],
            )
        };
        let got_tree = e.held_type_hash(&p, &mut t, rows.params[0].ty).unwrap();
        assert_eq!(got_tree, expected_tree);
        assert_eq!(
            got_tree.to_string(),
            "110855aa341534bb035f9a810d7c138f4ad2ce11b4ff9f885bdae7525b6df0a0"
        );

        let got_api_take = e.pack_export_hash(&p, &mut t, api_take.callable).unwrap();
        assert_eq!(got_api_take, expected_api_take);
        assert_eq!(
            got_api_take.to_string(),
            "fa62576dde7e904e223a2de4f2fc56fbd89e84d9c4734ed7b68314187f865e60"
        );

        // Scalar view parameter lowers to Param::Value(u64), and scalar result uses dest.
        let got_api_view = e.pack_export_hash(&p, &mut t, api_view.callable).unwrap();
        view_hashes.push(got_api_view);
        assert_ne!(got_api_view, got_api_take);

        // Same-layout distinct nominal types produce distinct PACK-16 and PACK-14 digests.
        let h_left = e.held_type_hash(&p, &mut t, rows.params[2].ty).unwrap();
        let h_right = e.held_type_hash(&p, &mut t, rows.params[3].ty).unwrap();
        assert_ne!(h_left, h_right);
        assert_ne!(
            e.pack_export_hash(&p, &mut t, api_left.callable).unwrap(),
            e.pack_export_hash(&p, &mut t, api_right.callable).unwrap(),
        );

        // `waits` flag changes the PACK-14 export digest.
        assert_ne!(
            e.pack_export_hash(&p, &mut t, api_sync.callable).unwrap(),
            e.pack_export_hash(&p, &mut t, api_waits.callable).unwrap(),
        );

        // Region-anchored exports produce stable PACK-14 digests across row renumbering.
        region_hashes.push(e.pack_export_hash(&p, &mut t, api_region.callable).unwrap());
    }
    assert_eq!(view_hashes[0], view_hashes[1]);
    assert_eq!(region_hashes[0], region_hashes[1]);
}

#[test]
fn nested_callback_variant_order_changes_the_pack_witness() {
    fn hashes(declaration: &str) -> (ContentHash, ContentHash, ContentHash) {
        let source = format!(
            "type Event = {declaration}\ntype Holder {{ cb: fn(Event) -> u64 }}\nfn api(cb: fn(Event) -> u64) -> u64 {{ 0 }}\nfn direct(event: Event, holder: Holder) {{}}"
        );
        let (index, modules) = crate::tests::program_inputs(&source);
        let p = Program::declare(&index, &modules);
        assert!(index.diagnostics.is_empty(), "{:?}", index.diagnostics);
        assert!(
            p.refusals.is_empty() && p.gaps.is_empty(),
            "{:?} {:?}",
            p.refusals,
            p.gaps
        );
        let api = p.fns.iter().find(|fd| fd.path.0.name == "api").unwrap();
        let direct = p.fns.iter().find(|fd| fd.path.0.name == "direct").unwrap();
        let mut types = Local::new(&p.types);
        let mut e = Emitter::new();
        let callback = e.pack_export_hash(&p, &mut types, api.callable).unwrap();
        let event = e
            .held_type_hash(&p, &mut types, direct.params[0].ty)
            .unwrap();
        let holder = e
            .held_type_hash(&p, &mut types, direct.params[1].ty)
            .unwrap();
        (callback, event, holder)
    }
    let (first_callback, first_event, first_holder) = hashes("A(u64) | B");
    let (second_callback, second_event, second_holder) = hashes("B | A(u64)");
    assert_ne!(first_event, second_event, "changed native variant tags");
    assert_ne!(
        first_callback, second_callback,
        "callback signature must bind its input's native variant tags"
    );
    assert_ne!(
        first_holder, second_holder,
        "record containing callback must bind callback parameter's native variant tags"
    );
}

#[test]
fn held_send_spells_canonically_in_pack_witnesses() {
    let source = "fn keep(h: Held[send]) -> Held[send] { h }\n";
    let (index, modules) = crate::tests::program_inputs(source);
    let p = Program::declare(&index, &modules);
    assert!(
        p.refusals.is_empty() && p.gaps.is_empty(),
        "{:?} {:?}",
        p.refusals,
        p.gaps
    );
    let keep = p.fns.iter().find(|fd| fd.path.0.name == "keep").unwrap();
    let t = Local::new(&p.types);
    assert_eq!(
        closed_type_text(&p.decls, &t, keep.params[0].ty).as_deref(),
        Some("core@1/core/Held[send]")
    );
}

#[test]
fn packing_operations_check_source_arguments_and_authority() {
    let headers = concat!(
        "type Left { n: u64 }\n",
        "type Right { n: u64 }\n",
        "fn entry(x: Left) -> u64 { x.n }\n",
        "fn apply(cb: fn(Left) -> u64) {}\n",
        "fn id[T](take x: T) -> T { x }\n",
        "fn waiter(cb: fn() waits) waits { cb() }\n",
    );
    let cases = [
        // Valid named entries, handles, slots, Held, and StaticFn reflection.
        (
            "fn use_entry() -> Code[fn(Left) -> u64] { code.bind(entry) }",
            None,
        ),
        (
            "fn use_generic() -> Code[fn(take u64) -> u64] { code.bind(id) }",
            None,
        ),
        (
            "fn call(c: Code[fn(Left) -> u64], x: Left) -> u64 { c(x) }",
            None,
        ),
        (
            "fn call(s: CodeSlot[fn(Left) -> u64], x: Left) -> Result[u64, code.DispatchError] { code.dispatch(s, x) }",
            None,
        ),
        (
            "fn wait_dispatch(s: CodeSlot[fn() waits]) -> Result[(), code.DispatchError] waits { code.dispatch(s) }",
            None,
        ),
        (
            "fn admit(l: Loader, k: Inline[u8, 32]) -> Result[Code[fn(Left) -> u64], code.LoadError] { code.load(l, k, k) }",
            None,
        ),
        (
            "fn slot(l: Loader) -> CodeSlot[fn()] { code.slot(l) }",
            None,
        ),
        (
            "fn install(mut s: CodeSlot[fn()], c: Code[fn()]) -> Result[u64, code.ActivateError] { code.activate(mut s, 0, c) }",
            None,
        ),
        (
            "fn dup(c: Code[fn()]) -> Code[fn()] { code.share(c) }",
            None,
        ),
        ("fn keep(n: u64) -> Held { code.hold(n) }", None),
        ("fn keep_send(n: u64) -> Held[send] { code.hold(n) }", None),
        (
            "fn restore(take h: Held) -> Result[u64, (code.TakeError, Held)] { code.take_as(take h) }",
            None,
        ),
        (
            "fn restore_send(take h: Held[send]) -> Result[u64, (code.TakeError, Held[send])] { code.take_as(take h) }",
            None,
        ),
        (
            "fn id_code(c: Code[fn()]) -> View[Bytes] in c { code.identity(c) }",
            None,
        ),
        (
            "fn rev_code(c: Code[fn()]) -> Option[View[Bytes]] in c { code.revision(c) }",
            None,
        ),
        (
            "fn id_static(c: StaticFn[fn()]) -> View[Bytes] in c { code.identity(c) }",
            None,
        ),
        (
            "fn rev_static(c: StaticFn[fn()]) -> Option[View[Bytes]] in c { code.revision(c) }",
            None,
        ),
        (
            "fn static_call(s: StaticFn[fn(Left) -> u64], x: Left) -> u64 { let f = code.callable(s); f(x) }",
            None,
        ),
        // Forged handles & authority refusals.
        ("fn fake_loader() -> Loader { Loader() }", Some("PROG-2")),
        ("fn fake_code() -> Code[fn()] { Code() }", Some("PROG-2")),
        (
            "fn fake_slot() -> CodeSlot[fn()] { CodeSlot() }",
            Some("PROG-2"),
        ),
        ("fn fake_held() -> Held { Held() }", Some("PROG-2")),
        (
            "fn peek_held(h: Held[send]) -> u64 { h.word }",
            Some("LANG-9"),
        ),
        (
            "fn no_loader() -> CodeSlot[fn()] { code.slot() }",
            Some("CAP-1"),
        ),
        (
            "fn leak_callable(c: Code[fn()]) -> fn() { code.callable(c) }",
            Some("LANG-11"),
        ),
        (
            "fn bind_closure() -> Code[fn()] { let f = fn() {}; code.bind(f) }",
            Some("PACK-14"),
        ),
        ("fn bad_carrier(c: Code[u64]) {}", Some("PACK-14")),
        ("fn bad_own_carrier(c: Code[own fn()]) {}", Some("PACK-14")),
        (
            "fn first_class_load() { let _ = code.load; }",
            Some("PACK-14"),
        ),
        (
            "fn bad_take_non_held(take x: u64) -> Result[u64, (code.TakeError, u64)] { code.take_as(take x) }",
            Some("PACK-16"),
        ),
        // Same-layout wrong nominal type refuses.
        (
            "fn wrong_nominal() -> Code[fn(Right) -> u64] { code.bind(entry) }",
            Some("LANG-11"),
        ),
        // Nested changed callback signature refuses.
        (
            "fn wrong_nested_cb() -> Code[fn(fn(Right) -> u64)] { code.bind(apply) }",
            Some("LANG-11"),
        ),
        // Escaping view, region-backed, uncounted code refs, and !send in Held[send] refuse.
        (
            "fn escape_hold(x: View[Bytes]) -> Held { code.hold(take x) }",
            Some("PACK-16"),
        ),
        (
            "fn escape_take(take h: Held) -> Result[View[Bytes], (code.TakeError, Held)] { code.take_as(take h) }",
            Some("PACK-16"),
        ),
        (
            "fn escape_region(r: Region, x: Text in r) -> Held { code.hold(take x) }",
            Some("PACK-16"),
        ),
        (
            "fn escape_static_fn() -> Held { code.hold(code.describe(entry)) }",
            Some("PACK-16"),
        ),
        (
            "fn escape_fn() -> Held { code.hold(entry) }",
            Some("PACK-16"),
        ),
        (
            "fn escape_id(c: Code[fn()]) -> View[Bytes] { code.identity(c) }",
            Some("OWN-20"),
        ),
        (
            "fn non_send_held(take x: Rc[u64]) -> Held[send] { code.hold(take x) }",
            Some("LANG-12"),
        ),
        (
            "fn promote_held(take h: Held) -> Held[send] { h }",
            Some("LANG-11"),
        ),
        // waits-in-Call and waits mismatches refuse.
        (
            "fn waiting_call(c: Code[fn() waits]) waits { c() }",
            Some("PACK-14"),
        ),
        (
            "fn wait_dispatch_in_sync(s: CodeSlot[fn() waits]) -> Result[(), code.DispatchError] { code.dispatch(s) }",
            Some("LANG-8"),
        ),
        (
            "fn bind_wait_mismatch() -> Code[fn(fn() waits)] { code.bind(waiter) }",
            Some("LANG-11"),
        ),
        (
            "fn bind_sync_as_wait() -> Code[fn() waits] { code.bind(entry) }",
            Some("LANG-11"),
        ),
        // Open type parameters refuse at witness uses.
        (
            "fn open_load[T](l: Loader, k: Inline[u8, 32]) -> Result[Code[fn(T)], code.LoadError] { code.load(l, k, k) }",
            Some("PACK-14"),
        ),
        (
            "fn open_hold[T](take x: T) -> Held { code.hold(take x) }",
            Some("PACK-16"),
        ),
        (
            "fn open_take[T](take h: Held) -> Result[T, (code.TakeError, Held)] { code.take_as(take h) }",
            Some("PACK-16"),
        ),
    ];
    let mut failures = Vec::new();
    for (case, expected) in cases {
        let (index, modules) = crate::tests::program_inputs(&format!("{headers}{case}"));
        let p = Program::declare(&index, &modules);
        let mut refusals = p.refusals.clone();
        let mut gaps = p.gaps.clone();
        for (i, fd) in p
            .fns
            .iter()
            .enumerate()
            .filter(|(_, fd)| fd.path.package_is("app", "1"))
        {
            let mut e = Emitter::new();
            let c = p.check_lower(i, &mut e);
            refusals.extend(c.refusals);
            gaps.extend(c.gaps);
            if expected.is_none()
                && let Err(error) = hirsel_core_ir::wf::module(&e.module)
            {
                failures.push(format!("{}: {error}", fd.path));
            }
        }
        if !gaps.is_empty()
            || expected.map_or(!refusals.is_empty(), |rule| {
                !refusals.iter().any(|r| r.rule == rule)
            })
        {
            failures.push(format!("{case}: {refusals:?} {gaps:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn unbuilt_packing_operations_explicitly_refuse_execution_while_static_fn_stays_executable() {
    let source = concat!(
        "fn entry(x: u64) -> u64 { x }\n",
        "fn run_bind() -> Code[fn(u64) -> u64] { code.bind(entry) }\n",
        "fn run_hold(x: u64) -> Held { code.hold(x) }\n",
        "fn run_id(c: Code[fn(u64) -> u64]) -> View[Bytes] in c { code.identity(c) }\n",
        "fn run_static() -> u64 { let s = code.describe(entry); let _ = code.identity(s); let f = code.callable(s); f(7) }\n",
    );
    let (index, modules) = crate::tests::program_inputs(source);
    let p = Program::declare(&index, &modules);
    assert!(
        p.refusals.is_empty() && p.gaps.is_empty(),
        "{:?} {:?}",
        p.refusals,
        p.gaps
    );

    // Unbuilt helpers in core.code explicitly lower to `target.unsupported(...)`.
    for (helper, prefix) in [
        ("code.export_unbuilt", "PACK-14"),
        ("code.loaded_identity", "PACK-14"),
        ("code.loaded_revision", "PACK-14"),
    ] {
        let i = p.function_id(&DefPath::core(helper)).unwrap();
        let mut e = Emitter::new();
        let c = p.check_lower(i, &mut e);
        assert!(
            c.refusals.is_empty() && c.gaps.is_empty(),
            "{helper}: {c:?}"
        );
        hirsel_core_ir::wf::module(&e.module).unwrap();
        let func = c.function.unwrap();
        let reason = hirsel_core_ir::unsupported_reason(&e.module, func)
            .unwrap_or_else(|| panic!("{helper} must refuse execution with target.unsupported"));
        assert!(
            reason.starts_with(prefix),
            "{helper}: unexpected reason {reason}"
        );
    }

    // Callers of unbuilt packing ops route to those unsupported helpers, whereas
    // StaticFn reflection does not.
    let calls_of = |name: &str| -> Vec<String> {
        let i = p
            .fns
            .iter()
            .position(|fd| fd.path.package_is("app", "1") && fd.path.0.name == name)
            .unwrap();
        let mut e = Emitter::new();
        let c = p.check_lower(i, &mut e);
        assert!(c.refusals.is_empty() && c.gaps.is_empty(), "{name}: {c:?}");
        hirsel_core_ir::wf::module(&e.module).unwrap();
        e.module
            .instances
            .iter()
            .map(|inst| inst.definition.to_string())
            .collect()
    };
    assert!(
        !calls_of("run_bind")
            .iter()
            .any(|s| s.ends_with("export_unbuilt"))
    );
    assert!(
        !calls_of("run_hold")
            .iter()
            .any(|s| s.ends_with("held_unbuilt"))
    );
    assert!(
        !calls_of("run_id")
            .iter()
            .any(|s| s.ends_with("loaded_identity"))
    );
    let static_calls = calls_of("run_static");
    assert!(
        !static_calls
            .iter()
            .any(|s| s.contains("unbuilt") || s.contains("loaded_"))
    );
}
