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
    // Even a locally initialized length word at offset eight cannot attest to
    // the live capacity of a collection. Keep both the charge and test tied to
    // that very same read to exercise the provenance requirement.
    let loaded = source
        .replace("sig g0", "type t0 = own \"sample@1/count/Header\" { word: u64, capacity: u64 }\nsig g0")
        .replace("  addr a0", "  slot s0 = t0\n  addr a0")
        .replace("b0(v0:", "  addr a1 = s0 byte v10 : u64\nb0(v0:")
        .replace("  v3: u64 = const 0", "  live s0\n  v10: u64 = const 8\n  address a1\n  store a1, v1\n  v11: u64 = read a1\n  v3: u64 = const 0")
        .replace("(v1, v4, v3)", "(v11, v4, v3)")
        .replace("lt v6, v1", "lt v6, v11");
    let fixture = load("locally read capacity", &loaded);
    wf::specialized_module(&fixture).unwrap();
    assert!(
        matches!(wf::metering_strict_with_budget(&fixture, &decode::Debit::unlimited()), Err(wf::Error::Invalid(ref why)) if why.contains("cycle"))
    );
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
