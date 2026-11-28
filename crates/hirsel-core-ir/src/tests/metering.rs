use super::*;

#[test]
fn metering_requires_a_step_on_each_source_loop_path() {
    // Both paths advance the induction variable. Only one records its iteration.
    let source = r#"core v1
sig g0 = (bool) -> ()
inst n0 = sample@1/count/two [] []
fn f0 = n0 g0 {
b0(v0: bool):
  v1: u64 = const 0
  v2: u64 = const 2
  v3: u64 = const 1
  jump b1(v1)
b1(v4: u64):
  v5: bool = lt v4, v2
  branch v5, b2, b5
b2:
  v6: u64 = add v4, v3
  branch v0, b3, b4
b3:
  step
  jump b1(v6)
b4:
  jump b1(v6)
b5:
  return
}
"#;
    let missing = load("unpaid iteration", source);
    let result = wf::module_with_budget(&missing, &decode::Debit::unlimited());
    assert!(matches!(result, Err(wf::Error::Invalid(_))), "{result:?}");
    let paid = source.replace("b4:\n  jump", "b4:\n  step\n  jump");
    wf::module(&load("paid iterations", &paid)).unwrap();
}

#[test]
fn metering_requires_a_step_before_recursive_descent() {
    let source = r#"core v1
sig g0 = (u64) -> ()
inst n0 = sample@1/count/down [] []
fn f0 = n0 g0 {
b0(v0: u64):
  v1: u64 = const 0
  v2: bool = eq v0, v1
  branch v2, b2, b1
b1:
  v3: u64 = const 1
  v4: u64 = sub v0, v3
  call f0 g0 (v4)
  jump b2
b2:
  return
}
"#;
    let missing = load("unpaid call", source);
    let result = wf::module_with_budget(&missing, &decode::Debit::unlimited());
    assert!(matches!(result, Err(wf::Error::Invalid(_))), "{result:?}");
    let paid = source.replace("  call f0", "  step\n  call f0");
    wf::module(&load("paid call", &paid)).unwrap();
}

#[test]
fn metering_checks_the_bound_and_precharge_of_a_scalar_repeat() {
    let source = r#"core v1
type t0 = own "sample@1/count/Array" { items: array 513 u64 }
sig g0 = () -> () dest t0
sig g1 = (u64, u64, u64) -> ()
inst n0 = sample@1/count/fill [] []
fn f0 = n0 g0 {
  addr a0 = v0 : t0
  addr a1 = a0 field 0 element v7 : u64
b0(v0: ptr):
  address a0
  v1: u64 = const 4104
  v2: u64 = const 12
  v3: u64 = const 0
  call rt.work_steps g1 (v1, v2, v3)
  v4: u64 = const 0
  v5: u64 = const 513
  v6: u64 = const 1
  jump b1(v4)
b1(v7: u64):
  v8: bool = lt v7, v5
  branch v8, b2, b3
b2:
  address a1
  store a1, v3
  v9: u64 = add v7, v6
  jump b1(v9)
b3:
  return
}
"#;
    for (old, new) in [
        ("const 4104", "const 4096"),
        ("const 12", "const 13"),
        ("  call rt.work_steps g1 (v1, v2, v3)\n", ""),
        // The debit describes 513 stores, while the induction limit claims 514.
        ("const 513", "const 514"),
    ] {
        let bad = load("repeat", &source.replace(old, new));
        assert!(
            matches!(
                wf::module_with_budget(&bad, &decode::Debit::unlimited()),
                Err(wf::Error::Invalid(_))
            ),
            "{old}"
        );
    }
    for paid in [
        source.to_owned(),
        source.replace(" = add ", " = wrapping_add "),
    ] {
        let valid = load("paid scalar repeat", &paid);
        for verify in [wf::metering_with_budget, wf::metering_strict_with_budget] {
            verify(&valid, &decode::Debit::unlimited()).unwrap();
            let short = load(
                "underpaid scalar repeat",
                &paid.replace("const 4104", "const 4096"),
            );
            assert!(
                matches!(verify(&short, &decode::Debit::unlimited()), Err(wf::Error::Invalid(ref why)) if why.contains("precharge"))
            );
        }
    }
}

#[test]
fn sum_to_requires_its_user_loop_charge() {
    let sample = include_str!("../../samples/control.core");
    wf::module(&load("sum_to", sample)).unwrap();
    let unpaid = load("sum_to", &sample.replace("  step\n", ""));
    let result = wf::module_with_budget(&unpaid, &decode::Debit::unlimited());
    assert!(matches!(result, Err(wf::Error::Invalid(_))), "{result:?}");
}

#[test]
fn strict_accepts_a_byte_fill_bounded_by_the_prepaid_ssa_count() {
    let source = r#"core v1
sig g0 = (ptr, u64, u8) -> ()
sig g1 = (u64, u64, u64) -> ()
inst n0 = sample@1/count/bytes [] []
fn f0 = n0 g0 {
  addr a0 = v0 byte v8 : u8
b0(v0: ptr, v1: u64, v2: u8):
  v3: u64 = const 0
  v4: u64 = const 12
  v5: u64 = const 1
  call rt.work_steps g1 (v1, v4, v3)
  jump b1(v3)
b1(v6: u64):
  v7: bool = lt v6, v1
  branch v7, b2, b3
b2:
  v8: u64 = wrapping_mul v6, v5
  address a0
  store a0, v2
  v9: u64 = wrapping_add v6, v5
  jump b1(v9)
b3:
  return
}
"#;
    let verify = |s: &str| {
        wf::metering_strict_with_budget(&load("byte fill", s), &decode::Debit::unlimited())
    };
    verify(source).unwrap();
    assert!(matches!(
        verify(&source.replace("(v1, v4, v3)", "(v3, v4, v3)")),
        Err(wf::Error::Invalid(_))
    ));
    // A count read from memory bounds the loop only through the charge that
    // names that very read: the debit pays for whatever the read returned.
    let loaded = source
        .replace("sig g0", "type t0 = own \"sample@1/count/Header\" { word: u64, capacity: u64 }\nsig g0")
        .replace("  addr a0", "  slot s0 = t0\n  addr a0")
        .replace("b0(v0:", "  addr a1 = s0 byte v10 : u64\nb0(v0:")
        .replace("  v3: u64 = const 0", "  live s0\n  v10: u64 = const 8\n  address a1\n  store a1, v1\n  v11: u64 = read a1\n  v3: u64 = const 0")
        .replace("(v1, v4, v3)", "(v11, v4, v3)")
        .replace("lt v6, v1", "lt v6, v11");
    let fixture = load("locally read count", &loaded);
    wf::specialized_module(&fixture).unwrap();
    wf::metering_strict_with_budget(&fixture, &decode::Debit::unlimited()).unwrap();
    for (forged, why) in [
        // The same word, read again after the charge, can hold anything.
        (
            loaded
                .replace(
                    "  jump b1(v3)",
                    "  store a1, v4\n  v12: u64 = read a1\n  jump b1(v3)",
                )
                .replace("lt v6, v11", "lt v6, v12"),
            "re-read",
        ),
        // Paying for the value stored does not pay for the value read back.
        (loaded.replace("(v11, v4, v3)", "(v1, v4, v3)"), "stored"),
    ] {
        let fixture = load(why, &forged);
        wf::specialized_module(&fixture).unwrap();
        // Admission refuses the unpaid element loop too, though its latch
        // stores and so leaves the scalar cycle rule.
        for verify in [wf::metering_with_budget, wf::metering_strict_with_budget] {
            assert!(
                matches!(verify(&fixture, &decode::Debit::unlimited()), Err(wf::Error::Invalid(ref e)) if e.contains("precharge")),
                "{why}"
            );
        }
    }
}

#[test]
fn aggregate_repeat_pays_for_the_logical_size_of_each_copied_element() {
    let source = r#"core v1
type t0 = own "sample@1/count/Pair" { x: u64, y: u64 }
type t1 = own "sample@1/count/Pairs" { items: array 513 t0 }
sig g0 = (ptr, ptr) -> ()
sig g1 = (u64, u64, u64) -> ()
inst n0 = sample@1/count/fill [] []
fn f0 = n0 g0 {
  addr a0 = v0 : t1
  addr a1 = a0 field 0 element v7 : t0
  addr a2 = v1 : t0
b0(v0: ptr, v1: ptr):
  address a0
  v2: u64 = const 8208
  v3: u64 = const 12
  v4: u64 = const 0
  call rt.work_steps g1 (v2, v3, v4)
  v5: u64 = const 513
  v6: u64 = const 1
  jump b1(v4)
b1(v7: u64):
  v8: bool = lt v7, v5
  branch v8, b2, b3
b2:
  address a1
  address a2
  v9: u64 = held_size t0
  copy a1, a2, v9
  v10: u64 = wrapping_add v7, v6
  jump b1(v10)
b3:
  return
}
"#;
    let strict = |s: &str| {
        let module = load("aggregate repeat", s);
        wf::specialized_module(&module).unwrap();
        wf::metering_strict_with_budget(&module, &decode::Debit::unlimited())
    };
    strict(source).unwrap();
    // 513 pairs of 16 logical bytes; one element short of the debit.
    assert!(matches!(
        strict(&source.replace("8208", "8192")),
        Err(wf::Error::Invalid(_))
    ));
}

#[test]
fn a_dynamic_fill_is_paid_by_a_saturating_charge_on_its_count() {
    let source = r#"core v1
type t0 = own "sample@1/count/Pair" { x: u64, y: u64 }
sig g0 = (ptr, ptr, u64) -> ()
sig g1 = (u64, u64, u64) -> ()
inst n0 = sample@1/count/fill [] []
fn f0 = n0 g0 {
  addr a0 = v0 byte v10 : t0
  addr a1 = v1 : t0
b0(v0: ptr, v1: ptr, v2: u64):
  v3: u64 = const 16
  v4: u64 = saturating_mul v2, v3
  v5: u64 = const 12
  v6: u64 = const 0
  call rt.work_steps g1 (v4, v5, v6)
  v7: u64 = const 1
  jump b1(v6)
b1(v8: u64):
  v9: bool = lt v8, v2
  branch v9, b2, b3
b2:
  v10: u64 = wrapping_mul v8, v3
  address a0
  address a1
  v11: u64 = held_size t0
  copy a0, a1, v11
  v12: u64 = wrapping_add v8, v7
  jump b1(v12)
b3:
  return
}
"#;
    let strict = |s: &str| {
        let module = load("dynamic fill", s);
        wf::specialized_module(&module).unwrap();
        wf::metering_strict_with_budget(&module, &decode::Debit::unlimited())
    };
    strict(source).unwrap();
    for forged in [
        // 2^60 pairs wrap a 16-byte product to a zero charge.
        source.replace("saturating_mul v2, v3", "wrapping_mul v2, v3"),
        // A charge for 16 bytes per element does not pay for 32-byte elements.
        source.replace("{ x: u64, y: u64 }", "{ x: u64, y: u64, z: u64, w: u64 }"),
    ] {
        assert!(matches!(strict(&forged), Err(wf::Error::Invalid(_))));
    }
}

#[test]
fn a_clear_consumes_one_successful_copy_precharge() {
    let source = r#"core v1
sig g0 = (ptr, u64) -> ()
sig g1 = (u64, u64, u64) -> ()
sig g2 = (ptr, ptr, u64) -> ()
inst n0 = sample@1/count/clear [] []
fn f0 = n0 g0 {
b0(v0: ptr, v1: u64):
  v2: u64 = const 0
  v3: u64 = const 12
  call rt.work_steps g1 (v1, v3, v2)
  call rt.zero_bytes g2 (v0, v0, v1)
  return
}
"#;
    for verify in [wf::metering_with_budget, wf::metering_strict_with_budget] {
        verify(&load("clear", source), &decode::Debit::unlimited()).unwrap();
        for bad in [
            source.replace("  call rt.work_steps g1 (v1, v3, v2)\n", ""),
            source.replace("  return", "  call rt.zero_bytes g2 (v0, v0, v1)\n  return"),
            source.replace("(v1, v3, v2)", "(v2, v3, v2)"),
        ] {
            assert!(matches!(
                verify(&load("clear", &bad), &decode::Debit::unlimited()),
                Err(wf::Error::Invalid(_))
            ));
        }
    }
}

#[test]
fn strict_checks_mutual_calls_across_function_bodies() {
    let source = r#"core v1
sig g0 = (bool) -> ()
inst n0 = sample@1/count/first [] []
inst n1 = sample@1/count/second [] []
fn f0 = n0 g0 {
b0(v0: bool):
  branch v0, b1, b2
b1:
  v1: bool = const false
  call f1 g0 (v1)
  jump b2
b2:
  return
}
fn f1 = n1 g0 {
b0(v0: bool):
  call f0 g0 (v0)
  return
}
"#;
    let verify = |s: &str| {
        wf::metering_strict_with_budget(&load("mutual calls", s), &decode::Debit::unlimited())
    };
    assert!(matches!(verify(source), Err(wf::Error::Invalid(_))));
    verify(&source.replace("  call f0", "  step\n  call f0")).unwrap();
}

#[test]
fn strict_checks_sha_block_bytes_even_in_a_stepped_body() {
    let source = r#"core v1
sig g0 = (ptr, ptr) -> ()
sig g1 = (u64, u64, u64) -> ()
sig g2 = (ptr, ptr, u64) -> ()
inst n0 = sample@1/count/kernel [] []
fn f0 = n0 g0 {
b0(v0: ptr, v1: ptr):
  step
  v2: u64 = const 5
  v3: u64 = const 320
  v4: u64 = const 8
  v5: u64 = const 0
  call rt.work_steps g1 (v3, v4, v5)
  call rt.sha1_compress g2 (v0, v1, v2)
  return
}
"#;
    let verify = |s: &str| {
        wf::metering_strict_with_budget(&load("SHA-1 block count", s), &decode::Debit::unlimited())
    };
    verify(source).unwrap();
    for invalid in [
        source.replace("const 320", "const 256"),
        source.replace("const 8", "const 12"),
    ] {
        assert!(
            matches!(verify(&invalid), Err(wf::Error::Invalid(ref reason)) if reason.contains("precharge"))
        );
    }
}

#[test]
fn slot_cursor_needs_a_paid_bound_and_positive_nonwrapping_progress() {
    let source = r#"core v1
sig g0 = (u64) -> ()
sig g1 = (u64, u64, u64) -> ()
inst n0 = sample@1/count/cursor [] []
fn f0 = n0 g0 {
  slot s0 = u64
  addr a0 = s0 : u64
b0(v0: u64):
  live s0
  address a0
  v1: u64 = const 0
  v2: u64 = const 6
  v3: u64 = const 1
  store a0, v1
  call rt.work_steps g1 (v0, v2, v1)
  jump b1
b1:
  v4: u64 = read a0
  v5: bool = lt v4, v0
  branch v5, b2, b3
b2:
  v6: u64 = wrapping_add v4, v3
  store a0, v6
  jump b1
b3:
  return
}
"#;
    let verify = |s: &str| {
        let m = load("slot cursor", s);
        wf::specialized_module(&m).unwrap();
        wf::metering_strict_with_budget(&m, &decode::Debit::unlimited())
    };
    verify(source).unwrap();
    verify(
        &source
            .replace("const 1\n", "const 3\n")
            .replace("wrapping_add", "add"),
    )
    .unwrap();
    let offset = source
        .replace(
            "store a0, v1",
            "store a0, v2\n  v7: u64 = wrapping_add v2, v0",
        )
        .replace("lt v4, v0", "lt v4, v7");
    verify(&offset).unwrap();
    let product = source.replace(
        "  call rt.work_steps g1 (v0,",
        "  v7: u64 = saturating_mul v0, v2\n  call rt.work_steps g1 (v7,",
    );
    verify(&product).unwrap();
    let split = source
        .replace(
            "b2:\n  v6",
            "b2:\n  v7: bool = eq v4, v2\n  branch v7, b4, b5\nb3:\n  return\nb4:\n  v6",
        )
        .replace(
            "store a0, v6\n  jump b1\nb3:\n  return",
            "store a0, v6\n  jump b1\nb5:\n  v8: u64 = add v4, v2\n  store a0, v8\n  jump b1",
        );
    verify(&split).unwrap();
    for bad in [
        offset.replace("store a0, v2", "store a0, v1"),
        product.replace("saturating_mul", "wrapping_mul"),
        split.replace("add v4, v2", "add v4, v1"),
        source
            .replace("  jump b1\nb1:", "  call f1 g2 ()\n  jump b1\nb1:")
            .replace("inst n0", "sig g2 = () -> ()\ninst n0")
            .replace("fn f0", "inst n1 = sample@1/count/other [] []\nfn f0")
            + "fn f1 = n1 g2 extern\n",
        source.replace(
            "  v6: u64 = wrapping_add v4, v3",
            "  store a0, v1\n  v7: u64 = read a0\n  v6: u64 = wrapping_add v7, v3",
        ),
        source.replace("  call rt.work_steps g1 (v0, v2, v1)\n", ""),
        source.replace("const 1\n", "const 0\n"),
        source.replace("wrapping_add", "wrapping_sub"),
        source.replace("wrapping_add v4, v3", "wrapping_add v4, v0"),
        source.replace("const 1\n", "const 2\n"),
        source.replace("lt v4, v0", "lt v4, v2"),
        source.replace("  store a0, v6", "  store a0, v6\n  store a0, v1"),
        source
            .replace("b0(v0:", "  addr a1 = s0 : u64\nb0(v0:")
            .replace(
                "  store a0, v6",
                "  store a0, v6\n  address a1\n  store a1, v1",
            ),
    ] {
        assert!(
            matches!(verify(&bad), Err(wf::Error::Invalid(ref e)) if e.contains("cycle")),
            "{bad}"
        );
    }
}

#[test]
fn failure_drain_uses_the_initialized_allocation_and_its_paid_count() {
    let source = r#"core v1
type t0 = own "sample@1/count/Element" { x: u64 }
sig g0 = (u64, ptr) -> ()
sig g1 = (u64, u64, u64) -> ()
inst n0 = sample@1/count/drain [] []
fn f0 = n0 g0 {
  slot s0 = u64
  addr a0 = s0 : u64
  addr a1 = v7 : t0
  addr a2 = v1 : t0
  addr a3 = v7 index v10 : t0
b0(v0: u64, v1: ptr):
  live s0
  address a0
  v2: u64 = const 0
  store a0, v2
  v3: u64 = const 12
  v4: ptr = const 0x0
  v5: u64 = held_size t0
  v6: u64 = mul v0, v5
  call rt.work_steps g1 (v6, v3, v2)
  v7: ptr, v8: u64 = alloc for seq t0 v6 align 8 charge v6
  v9: bool = ne v7, v4
  branch v9, b1, b7
b1:
  address a1
  address a2
  call rt.work_steps g1 (v6, v3, v2) fail b6
  move a1, a2, v6
  raise bounds cleanup b2
b2:
  jump b3
b3:
  v10: u64 = read a0
  v11: bool = lt v10, v0
  branch v11, b4, b5
b4:
  address a3
  v12: u64 = const 1
  v13: u64 = wrapping_add v10, v12
  store a0, v13
  drop a3 t0
  jump b3
b5:
  free v7 charge v6
  propagate
b6:
  free v7 charge v6
  propagate
b7:
  return
}
"#;
    let verify = |s: &str| {
        let m = load("allocation-paid drain", s);
        wf::specialized_module(&m).unwrap();
        wf::metering::metering_checks_with_budget(
            &m,
            &decode::Debit::unlimited(),
            wf::metering::Checks {
                cycles: wf::metering::Cycles::Local,
                ..wf::metering::Checks::NONE
            },
        )
    };
    verify(source).unwrap();
    for refused in [
        source.replacen("  call rt.work_steps g1 (v6, v3, v2)\n", "", 1),
        source.replace("lt v10, v0", "lt v10, v6"),
        source.replace("addr a3 = v7 index", "addr a3 = v1 index"),
        source.replace(" = mul v0, v5", " = wrapping_mul v0, v5"),
        source.replace("move a1, a2, v6", "move a1, a2, v2"),
        source.replace("branch v9, b1, b7", "branch v9, b7, b1"),
    ] {
        assert!(
            matches!(verify(&refused), Err(wf::Error::Invalid(ref e)) if e.contains("cycle")),
            "{refused}"
        );
    }
}
