// The C-boundary poison law on the including suite's NATIVE_TARGET.
/// The C-boundary poison law (`c_boundary_poison.rs`) under one seed.
fn check_c_boundary_poison(seed: u64) {
    let law = c_boundary_poison::law(seed);
    let m = text::parse(&law.core).unwrap_or_else(|e| panic!("{e:?}\n{}", law.core));
    wf::module(&m).unwrap_or_else(|e| panic!("{e:?}\n{}", law.core));
    let mut source = law.c.clone();
    for (i, f) in m.functions.iter().enumerate() {
        source = source.replace(&format!("@F{i}@"), &m.identity(f.instance).symbol().to_string());
    }
    bind_sites(&mut source, &m, &m.sites());
    for optimize in [false, true] {
        let scratch = NativeArtifacts::new();
        fs::write(scratch.path().join("input.o"), m.object(optimize, false, false)).unwrap();
        let run = native_command(&link_program(&scratch, &source), false).output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let failures = c_boundary_poison::failures(&law, &stdout);
        assert!(
            failures.is_empty(),
            "{NATIVE_TARGET} seed {seed:#x} optimize={optimize}: {} of {} cells failed ({:?})\n{}",
            failures.len(),
            law.cells,
            run.status,
            failures.join("\n")
        );
    }
}

/// Poisoned upper register and stack-slot bits must not change a value
/// crossing the C boundary in either direction (`c_boundary_poison.rs`).
#[test]
fn c_boundary_poison_bits() {
    check_c_boundary_poison(0x2496_c0de_5eed);
}
