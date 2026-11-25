use super::*;

/// Loans (crate docs, "Loans"). Bugs: a producer passes one place as a `mut`
/// argument and as another argument, writes a place it holds as a `view` (both
/// unsound once an emitter marks `mut` noalias and `view` readonly), or a body
/// writes its own `view` parameter.
#[test]
fn loans_stop_producer_aliasing() {
    let module = |main: &str, callee: &str| {
        format!(
            "core v1\ntype t0 = own \"P\" {{ a: u64 }}\nsig g0 = () -> ()\nsig g1 = (&view t0, &mut t0) -> ()\nsig g2 = (&view t0, ptr) -> ()\nsig g3 = (&take t0, &mut t0) -> ()\nsig g4 = (&view t0, &take t0) -> ()\nsig g5 = (&take t0, ptr) -> ()\ninst n0 = p@0/m/main [] []\ninst n1 = p@0/m/callee [] []\nfn f0 = n0 g0 {{\n  slot s0 = t0\n  slot s1 = t0\n  addr a0 = s0 : t0\n  addr a1 = s1 : t0\n  addr a2 = s0 field 0 : u64\n  addr a3 = s1 field 0 : u64\nb0:\n  live s0\n  live s1\n  address a0\n  address a1\n  address a2\n  address a3\n  v0: u64 = const 0\n  store a2, v0\n  store a3, v0\n{main}  return\n}}\n{callee}",
            main = main.replace("v0", "v1")
        )
    };
    // the callee writes its `mut` place, or a place it was handed as a pointer
    let write = |sig: &str, base: &str| {
        format!(
            "fn f1 = n1 {sig} {{\n  addr a0 = {base} : t0\n  addr a1 = a0 field 0 : u64\nb0(v0: ptr, v1: ptr):\n  address a0\n  address a1\n  v2: u64 = const 1\n  store a1, v2\n  return\n}}\n"
        )
    };
    let (mutate, poke) = (write("g1", "v1"), write("g2", "v1"));
    // the evaluator alone, as the oracle for what wf cannot see
    let eval_only = |text: &str| {
        let m = parse(text).unwrap();
        std::panic::catch_unwind(|| {
            eval::run_nans(&m, FuncId(0), &[], u64::MAX, eval::Nans::Canonical)
        })
        .map(|(_, checked)| checked.loans)
        .map_err(|e| {
            e.downcast::<String>()
                .map_or_else(|_| String::new(), |s| *s)
        })
    };
    let wf_err = |text: &str| wf::module(&parse(text).unwrap()).expect_err(text);
    let run = |text: &str| {
        let m = parse(text).unwrap();
        wf::module(&m).unwrap();
        std::panic::catch_unwind(|| {
            eval::run_nans(&m, FuncId(0), &[], u64::MAX, eval::Nans::Canonical)
        })
        .map(|(_, checked)| checked.loans)
        .map_err(|e| {
            e.downcast::<String>()
                .map_or_else(|_| String::new(), |s| *s)
        })
    };
    assert_eq!(
        run(&module("  call f1 g1 (&a0, &a1)\n", &mutate)),
        Ok(2),
        "distinct places run, and both loans are checked"
    );
    // one place lent as mut and as a view or a take: wf refuses it, and the
    // evaluator faults it when wf is bypassed
    for (call, callee) in [
        ("  call f1 g1 (&a0, &a0)\n", mutate.clone()),
        ("  call f1 g3 (&a0, &a0)\n", write("g3", "v1")),
    ] {
        let text = module(call, &callee);
        let err = wf_err(&text);
        assert!(
            err.contains("a mut place overlaps another argument"),
            "got `{err}`"
        );
        let shared = eval_only(&text).unwrap_err();
        assert!(
            shared.contains("overlaps another argument's place"),
            "got `{shared}`"
        );
    }
    // a take lent from a place another argument views: nothing writes it
    let reader = "fn f1 = n1 g4 {\n  addr a0 = v0 : t0\n  addr a1 = a0 field 0 : u64\n  addr a2 = v1 : t0\n  addr a3 = a2 field 0 : u64\nb0(v0: ptr, v1: ptr):\n  address a0\n  address a1\n  address a2\n  address a3\n  v2: u64 = read a1\n  v3: u64 = read a3\n  return\n}\n";
    assert_eq!(run(&module("  call f1 g4 (&a0, &a0)\n", reader)), Ok(2));
    let poked = run(&module(
        "  v0: ptr = pointer a0\n  call f1 g2 (&a0, v0)\n",
        &poke,
    ))
    .unwrap_err();
    assert!(
        poked.contains("into a lent view or take place"),
        "got `{poked}`"
    );
    let taken = run(&module(
        "  v0: ptr = pointer a0\n  call f1 g5 (&a0, v0)\n",
        &write("g5", "v1"),
    ))
    .unwrap_err();
    assert!(
        taken.contains("into a lent view or take place"),
        "got `{taken}`"
    );
    // A value place (`&mut u64`, #88: a `mut` scalar or Region parameter) is lent
    // as any by-address place: its size comes from the place type, so the loan is
    // checked; the callee's write is the caller's (5 becomes 7).
    let cell = |call: &str| {
        format!(
            "core v1\nsig g0 = () -> (u64)\nsig g1 = (&mut u64, &mut u64) -> ()\ninst n0 = p@0/m/main [] []\ninst n1 = p@0/m/callee [] []\nfn f0 = n0 g0 {{\n  slot s0 = u64\n  slot s1 = u64\n  addr a0 = s0 : u64\n  addr a1 = s1 : u64\nb0:\n  live s0\n  live s1\n  address a0\n  address a1\n  v0: u64 = const 5\n  store a0, v0\n  store a1, v0\n  call f1 g1 ({call})\n  v1: u64 = read a0\n  dead s1\n  dead s0\n  return v1\n}}\nfn f1 = n1 g1 {{\n  addr a0 = v0 : u64\nb0(v0: ptr, v1: ptr):\n  address a0\n  v2: u64 = const 7\n  store a0, v2\n  return\n}}\n"
        )
    };
    let m = parse(&cell("&a0, &a1")).unwrap();
    wf::module(&m).unwrap();
    let (report, checked) = eval::run_nans(&m, FuncId(0), &[], u64::MAX, eval::Nans::Canonical);
    assert_eq!(
        (report.outcome, checked.loans),
        (eval::Outcome::Returned(vec![7]), 2)
    );
    let twice = cell("&a0, &a0");
    let err = wf_err(&twice);
    assert!(
        err.contains("a mut place overlaps another argument"),
        "got `{err}`"
    );
    let shared = eval_only(&twice).unwrap_err();
    assert!(
        shared.contains("overlaps another argument's place"),
        "got `{shared}`"
    );
    // wf: a body writes its own take parameter's incoming place
    let err = wf_err(&module("", &write("g5", "v0")));
    assert!(
        err.contains("writes a take parameter's place"),
        "got `{err}`"
    );
    // wf: a body writes its own view parameter, directly or through `pointer`
    for callee in [
        write("g2", "v0"),
        "fn f1 = n1 g2 {\n  addr a0 = v0 : t0\n  addr a1 = v2 : t0\n  addr a2 = a1 field 0 : u64\nb0(v0: ptr, v1: ptr):\n  address a0\n  v2: ptr = pointer a0\n  address a1\n  address a2\n  v3: u64 = const 1\n  store a2, v3\n  return\n}\n".into(),
    ] {
        let m = parse(&module("", &callee)).unwrap();
        let err = wf::module(&m).expect_err("a write into a view parameter");
        assert!(err.contains("writes a view parameter's place"), "got `{err}`");
    }
}
/// Storage roles (crate docs, "Roles"): the evaluator stops a run whose accesses
/// break a role's claim, and the derivation claims nothing a valid body breaks.
#[test]
fn roles_hold_or_stop_the_run() {
    let list = "core v1\ntype t0 = own \"List[u8]\" { items: seq u8 }\n";
    let run = |text: &str, f: u32, args: &[u64]| {
        let m = parse(&format!("{list}{text}")).unwrap();
        wf::module(&m).unwrap_or_else(|e| panic!("{e}"));
        std::panic::catch_unwind(|| eval::run(&m, FuncId(f), args, u64::MAX))
            .map(|r| r.outcome)
            .map_err(|e| {
                e.downcast::<String>()
                    .map_or_else(|_| String::new(), |s| *s)
            })
    };
    // Bug: a kernel points one carrier's data word at another carrier's place, so a
    // buffer store rewrites that carrier's length behind LLVM's alias scopes. The
    // store lands at s0 + 8, the bytes s0's length store touched first.
    let aliased = run(
        "sig g0 = () -> ()\ninst n0 = core@1/m/k [] []\nfn f0 = n0 g0 {\n  slot s0 = t0\n  slot s1 = t0\n  addr a0 = s0 byte v0 : u64\n  addr a1 = s0 : t0\n  addr a2 = s1 byte v1 : ptr\n  addr a3 = v3 index v0 : u8\nb0:\n  live s0\n  live s1\n  v0: u64 = const 8\n  v1: u64 = const 0\n  address a0\n  store a0, v0\n  address a1\n  v2: ptr = pointer a1\n  address a2\n  store a2, v2\n  v3: ptr = read a2\n  v4: u8 = const 1\n  address a3\n  store a3, v4\n  dead s1\n  dead s0\n  return\n}\n",
        0,
        &[],
    );
    let stop = aliased.expect_err("a buffer store over a carrier ran to its end");
    assert!(stop.contains("Buffers touches +8..+9"), "{stop}");
    // A caller with one carrier of length 3 in s0, lent as `&a0` to f0.
    let caller = |sig: &str, call: &str| {
        format!(
            "sig g0 = {sig} -> (u64)\nsig g1 = () -> (u64)\ninst n0 = core@1/m/f [] []\ninst n1 = core@1/m/main [] []\n{{f0}}fn f1 = n1 g1 {{\n  slot s0 = t0\n  addr a0 = s0 : t0\n  addr a1 = s0 byte v0 : ptr\n  addr a2 = s0 byte v1 : u64\n  addr a3 = s0 byte v2 : u64\nb0:\n  live s0\n  v0: u64 = const 0\n  v1: u64 = const 8\n  v2: u64 = const 16\n  v3: ptr = const 0x0\n  v4: u64 = const 3\n  address a0\n  address a1\n  address a2\n  address a3\n  store a1, v3\n  store a2, v4\n  store a3, v0\n  v5: u64 = {call}\n  dead s0\n  return v5\n}}\n"
        )
    };
    // Bug: a `view` parameter gets a carrier role. Two views of one place may be
    // passed together (OWN-8), so reading both is valid: 3 + 3.
    let views = caller("(&view t0, &view t0)", "call f0 g0 (&a0, &a0)").replace(
        "{f0}",
        "fn f0 = n0 g0 {\n  addr a0 = v0 byte v2 : u64\n  addr a1 = v1 byte v2 : u64\nb0(v0: ptr, v1: ptr):\n  v2: u64 = const 8\n  address a0\n  address a1\n  v3: u64 = read a0\n  v4: u64 = read a1\n  v5: u64 = add v3, v4\n  return v5\n}\n",
    );
    assert_eq!(run(&views, 1, &[]), Ok(eval::Outcome::Returned(vec![6])));
    // Bug: the entry block's parameter keeps its parameter's role after an edge
    // rebinds it. f0 reads the length through its parameter (3), sets its own
    // slot's length to 7, re-enters with that slot, and reads 7 through the same
    // address: the parameter role would name the slot's bytes.
    let reentry = caller("(&mut t0)", "call f0 g0 (&a0)").replace(
        "{f0}",
        "fn f0 = n0 g0 {\n  slot s0 = t0\n  addr a0 = v0 byte v1 : u64\n  addr a1 = s0 byte v1 : u64\n  addr a2 = s0 : t0\nb0(v0: ptr):\n  v1: u64 = const 8\n  v3: u64 = const 7\n  address a0\n  v2: u64 = read a0\n  v4: bool = eq v2, v3\n  branch v4, b2, b1\nb1:\n  live s0\n  address a1\n  store a1, v3\n  address a2\n  v5: ptr = pointer a2\n  jump b0(v5)\nb2:\n  return v2\n}\n",
    );
    assert_eq!(run(&reentry, 1, &[]), Ok(eval::Outcome::Returned(vec![7])));
    // Bug: each carrier's buffer gets its own role. A move hands the buffer to
    // another carrier: element 0 is written through s0's data and read back (5)
    // through s1's, the same bytes under two carriers.
    let moved = run(
        "sig g0 = () -> (u8)\ninst n0 = core@1/m/k [] []\nfn f0 = n0 g0 {\n  slot s0 = t0\n  slot s1 = t0\n  addr a0 = s0 byte v0 : ptr\n  addr a1 = s0 byte v1 : u64\n  addr a2 = s0 byte v2 : u64\n  addr a3 = v6 index v0 : u8\n  addr a4 = s1 : t0\n  addr a5 = s0 : t0\n  addr a6 = s1 byte v0 : ptr\n  addr a7 = v9 index v0 : u8\nb0:\n  live s0\n  live s1\n  v0: u64 = const 0\n  v1: u64 = const 8\n  v2: u64 = const 16\n  v3: u64 = const 1\n  v4: ptr, v5: u64 = alloc for seq u8 v1 align 8 charge v1\n  address a0\n  address a1\n  address a2\n  store a0, v4\n  store a1, v3\n  store a2, v1\n  v6: ptr = read a0\n  v7: u8 = const 5\n  address a3\n  store a3, v7\n  v8: u64 = const 24\n  address a4\n  address a5\n  copy a4, a5, v8\n  address a6\n  v9: ptr = read a6\n  address a7\n  v10: u8 = read a7\n  free v4 charge v1\n  dead s1\n  dead s0\n  return v10\n}\n",
        0,
        &[],
    );
    assert_eq!(moved, Ok(eval::Outcome::Returned(vec![5])));

    // f0(&mut a, &mut b) reads a[0], runs `mid`, then reads b[0] and returns the sum.
    // Its caller lends two carriers of length 1 whose buffers hold 5 and 6 (or, with
    // `static`, one static byte 5 for both), and frees what it allocated.
    let two = |head: &str, mid: &str, tail: &str, stat: bool| {
        let (setup, fini) = if stat {
            (
                "  v4: ptr = static d0\n  v5: u64 = const 0\n  v6: ptr = static d0\n  v7: u64 = const 0\n  v8: u8 = const 0\n  v9: u8 = const 0\n  v10: u64 = const 0\n",
                "",
            )
        } else {
            (
                "  v4: ptr, v5: u64 = alloc for seq u8 v1 align 8 charge v1\n  v6: ptr, v7: u64 = alloc for seq u8 v1 align 8 charge v1\n  v8: u8 = const 5\n  v9: u8 = const 6\n  address a8\n  store a8, v8\n  address a9\n  store a9, v9\n  v10: u64 = const 8\n",
                "  free v4 charge v1\n  free v6 charge v1\n",
            )
        };
        let text = format!(
            "static d0 = align 1 hex 05\nsig g0 = (&mut t0, &mut t0) -> (u8)\nsig g1 = () -> (u8)\nsig g2 = (&mut t0, &mut t0) -> ()\ninst n0 = core@1/m/f [] []\ninst n1 = core@1/m/main [] []\ninst n2 = core@1/m/swap [] []\n\
             fn f0 = n0 g0 {{\n  addr a0 = v0 byte v2 : ptr\n  addr a1 = v1 byte v2 : ptr\n  addr a2 = v3 index v2 : u8\n{head}b0(v0: ptr, v1: ptr):\n  v2: u64 = const 0\n  address a0\n  address a1\n  v3: ptr = read a0\n  address a2\n  v4: u8 = read a2\n{mid}{tail}}}\n\
             fn f1 = n1 g1 {{\n  slot s0 = t0\n  slot s1 = t0\n  addr a0 = s0 : t0\n  addr a1 = s0 byte v0 : ptr\n  addr a2 = s0 byte v1 : u64\n  addr a3 = s0 byte v2 : u64\n  addr a4 = s1 : t0\n  addr a5 = s1 byte v0 : ptr\n  addr a6 = s1 byte v1 : u64\n  addr a7 = s1 byte v2 : u64\n  addr a8 = v4 index v0 : u8\n  addr a9 = v6 index v0 : u8\nb0:\n  live s0\n  live s1\n  v0: u64 = const 0\n  v1: u64 = const 8\n  v2: u64 = const 16\n  v3: u64 = const 1\n{setup}  address a0\n  address a1\n  address a2\n  address a3\n  address a4\n  address a5\n  address a6\n  address a7\n  store a1, v4\n  store a2, v3\n  store a3, v10\n  store a5, v6\n  store a6, v3\n  store a7, v10\n  v11: u8 = call f0 g0 (&a0, &a4)\n{fini}  dead s1\n  dead s0\n  return v11\n}}\n\
             fn f2 = n2 g2 {{\n  slot s0 = t0\n  addr a0 = s0 : t0\n  addr a1 = v0 : t0\n  addr a2 = v1 : t0\nb0(v0: ptr, v1: ptr):\n  live s0\n  v2: u64 = const 24\n  address a0\n  address a1\n  address a2\n  copy a0, a1, v2\n  copy a1, a2, v2\n  copy a2, a0, v2\n  dead s0\n  return\n}}\n"
        );
        let r = run(&text, 1, &[]);
        (text, r)
    };
    let sum = |v: u32| {
        let (y, z) = (v + 1, v + 2);
        format!(
            "  v{v}: ptr = read a1\n  address a3\n  v{y}: u8 = read a3\n  v{z}: u8 = add v4, v{y}\n  return v{z}\n"
        )
    };
    let read_b = |v: u32| format!("  addr a3 = v{v} index v2 : u8\n");
    // Two sealed parameters (merge's `a` and `temp`) get two buffer roles, so LLVM
    // gives them two scopes; and they may read one static literal together.
    let (text, both) = two(&read_b(5), "", &sum(5), true);
    assert_eq!(both, Ok(eval::Outcome::Returned(vec![10])));
    let m = parse(&format!("{list}{text}")).unwrap();
    let r = roles::Facts::new(&m).roles(&m, FuncId(0));
    assert_eq!(
        (r[2], r[3]),
        (
            Some(roles::Role::Buffer(roles::Root::Param(0))),
            Some(roles::Role::Buffer(roles::Root::Param(1)))
        )
    );
    // Bug: a `mut` lend leaves the carrier sealed. The callee swaps a and b, so a's
    // buffer is read again through b: 5 + 5. The swap only moves bytes, so both
    // places stay blind (a stand-in copy would swap the same way): sealed and
    // place-blind are different facts.
    let (swap_text, swapped) = two(
        &format!("{}  addr a4 = v0 : t0\n  addr a5 = v1 : t0\n", read_b(5)),
        "  address a4\n  address a5\n  call f2 g2 (&a4, &a5)\n",
        &sum(5),
        false,
    );
    assert_eq!(swapped, Ok(eval::Outcome::Returned(vec![10])));
    let m = parse(&format!("{list}{swap_text}")).unwrap();
    let facts = roles::Facts::new(&m);
    assert_eq!(
        (facts.sealed(FuncId(0), 0), facts.place_blind(FuncId(0), 0)),
        (false, true),
        "the swap's caller"
    );
    // Bug: a lend to a function with no body here keeps a fact. Nothing says what
    // runs there.
    let external = swap_text.replace("call f2 g2", "call f3 g2")
        + "inst n3 = core@1/m/elsewhere [] []\nfn f3 = n3 g2 extern\n";
    let m = parse(&format!("{list}{external}")).unwrap();
    let facts = roles::Facts::new(&m);
    assert_eq!(
        (facts.sealed(FuncId(0), 0), facts.place_blind(FuncId(0), 0)),
        (false, false),
        "a lend to an external callee"
    );
    // A recursion lends both carriers on to itself, as merge does: each stays
    // sealed (and blind) through the lend. a[0] counts 5 down to 0, then 0 + b[0].
    let (text, recursive) = two(
        &format!("{}  addr a4 = v0 : t0\n  addr a5 = v1 : t0\n", read_b(10)),
        "  v5: u8 = const 0\n  v6: bool = eq v4, v5\n  branch v6, b2, b1\nb1:\n  v7: u8 = const 1\n  v8: u8 = sub v4, v7\n  store a2, v8\n  address a4\n  address a5\n  v9: u8 = call f0 g0 (&a4, &a5)\n  return v9\nb2:\n",
        &sum(10),
        false,
    );
    assert_eq!(recursive, Ok(eval::Outcome::Returned(vec![6])));
    let m = parse(&format!("{list}{text}")).unwrap();
    let facts = roles::Facts::new(&m);
    let r = facts.roles(&m, FuncId(0));
    assert_eq!(
        (r[2], r[3]),
        (
            Some(roles::Role::Buffer(roles::Root::Param(0))),
            Some(roles::Role::Buffer(roles::Root::Param(1)))
        ),
        "a lend to a sealed callee"
    );
    assert!(facts.place_blind(FuncId(0), 0), "a lend to a blind callee");
    // Bug: a body that compares its place's address with another is blind. A
    // stand-in copy changes what the comparison sees.
    let (text, _) = two(
        &format!("{}  addr a4 = v0 : t0\n  addr a5 = v1 : t0\n", read_b(8)),
        "  address a4\n  address a5\n  v5: ptr = pointer a4\n  v6: ptr = pointer a5\n  v7: bool = eq v5, v6\n",
        &sum(8),
        true,
    );
    let m = parse(&format!("{list}{text}")).unwrap();
    let facts = roles::Facts::new(&m);
    assert_eq!(
        (
            facts.place_blind(FuncId(0), 0),
            facts.place_blind(FuncId(0), 1)
        ),
        (false, false),
        "a compared place"
    );
    assert!(
        facts.sealed(FuncId(0), 0),
        "a comparison does not move a buffer"
    );
    // Bug: a carrier copied whole stays sealed. b := a, then b[0] is a's 5.
    let (_, copied) = two(
        &format!("{}  addr a4 = v0 : t0\n  addr a5 = v1 : t0\n", read_b(6)),
        "  address a4\n  address a5\n  v5: u64 = const 24\n  copy a5, a4, v5\n",
        &sum(6),
        false,
    );
    assert_eq!(copied, Ok(eval::Outcome::Returned(vec![10])));
    // Bug: a's data pointer stored into b's data word leaves either sealed.
    let (_, escaped) = two(&read_b(5), "  store a1, v3\n", &sum(5), false);
    assert_eq!(escaped, Ok(eval::Outcome::Returned(vec![10])));
    // Bug: one fresh block installed in both data words leaves both sealed: 7 is
    // written through a's data and read through b's, 5 + 7.
    let (_, shared) = two(
        &format!("{}  addr a4 = v8 index v2 : u8\n", read_b(10)),
        "  v5: u64 = const 8\n  v6: ptr, v7: u64 = alloc for seq u8 v5 align 8 charge v5\n  store a0, v6\n  store a1, v6\n  v8: ptr = read a0\n  address a4\n  v9: u8 = const 7\n  store a4, v9\n",
        "  v10: ptr = read a1\n  address a3\n  v11: u8 = read a3\n  free v6 charge v5\n  v12: u8 = add v4, v11\n  return v12\n",
        false,
    );
    assert_eq!(shared, Ok(eval::Outcome::Returned(vec![12])));
}
