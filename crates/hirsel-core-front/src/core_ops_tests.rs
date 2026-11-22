//! Source-to-runtime tests. Values and charges are hand-derived from CONC-4/5,
//! MEM-24 and the declaration order of Result, Closed and SendError.
use super::*;

fn canonical(source: &str) -> (NameIndex, Vec<ModuleInput>) {
    inputs_with_core(
        source,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/core"),
    )
}

#[test]
fn core_operation_declarations_do_not_drift() {
    let (index, modules) = canonical("");
    let p = Program::declare(&index, &modules);
    crate::body::core_ops::assert_declarations(&p);
}

#[test]
fn cap6_ambient_kernels_take_authority() {
    let (index, modules) = canonical("");
    let p = Program::declare(&index, &modules);
    crate::body::core_ops::assert_no_ambient_input(&p);
}

#[test]
fn wire_net_operations_lower() {
    let (index, modules) = canonical(
        r#"
fn denied(n: Net) -> u64 waits {
    match host.connect(n, Addr(ip: V4(134744072u32), port: 80), Never) { Error(Denied) => 1, _ => 0 }
}
fn admitted(n: Net) -> u64 waits {
    match host.connect(n, Addr(ip: V4(2130706433u32), port: 80), Never) { Error(Refused) => 1, _ => 0 }
}
fn listening(n: Net) -> u64 waits {
    match host.listen(n, Addr(ip: V4(2130706433u32), port: 80), 16) { Ok((l, addr)) => if addr.port == 80 { 1 } else { 0 }, _ => 0 }
}
fn narrow(n: Net) -> u64 waits {
    match net.narrow(n, Rule(dir: Connect, prefix: V4(2130706433u32), bits: 32, ports: (80u16, 80u16))) {
        Ok(child) => match host.connect(child, Addr(ip: V4(2130706433u32), port: 81), Never) { Error(Denied) => 1, _ => 0 },
        _ => 0
    }
}
fn halves(take s: Socket) { let (r, w) = net.split(take s) }
fn read(mut r: ReadHalf, mut b: Bytes, c: Clock) waits { let _ = host.recv_bounded(mut r, mut b, 1, At(c, 1)) }
fn write(mut w: WriteHalf, b: Bytes) waits { let _ = host.send_pair(mut w, b, string.as_bytes(""), Never) }
fn accepting(mut l: Listener) waits { let _ = host.accept(mut l, Never) }
"#,
    );
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    for (f, function) in p
        .fns
        .iter()
        .enumerate()
        .filter(|(_, f)| f.path.package_is("app", "1"))
    {
        let mut emitter = Emitter::new();
        let checked = p.check_lower(f, &mut emitter);
        assert!(
            checked.refusals.is_empty() && checked.gaps.is_empty(),
            "{}: {checked:?}",
            function.path
        );
        wf::module(&emitter.module).unwrap_or_else(|e| {
            panic!(
                "{}: {e}\n{}",
                function.path,
                hirsel_core_ir::text::print(&emitter.module)
            )
        });
    }
}

#[test]
fn wire_socket_resources_are_move_only_send_and_size_eight() {
    for name in ["Socket", "ReadHalf", "WriteHalf", "Listener"] {
        let source = format!("fn f(take x: {name}) {{ let y = x }}");
        let (index, modules) = canonical(&source);
        let p = Program::declare(&index, &modules);
        let f = p
            .fns
            .iter()
            .position(|f| f.path.package_is("app", "1"))
            .unwrap();
        let mut emitter = Emitter::new();
        let c = p.check_lower(f, &mut emitter);
        assert!(c.refusals.is_empty() && c.gaps.is_empty(), "{c:?}");
        let recipes = hirsel_core_layout::compute(&emitter.module.types).unwrap();
        let shape = emitter
            .module
            .types
            .iter()
            .position(|t| t.name.ends_with(&format!("/{name}")))
            .unwrap();
        assert_eq!(recipes[shape].logical_size, Some(8));
        assert!(!recipes[shape].plain);
        let source = format!("fn f(x: Rc[{name}]) {{}}");
        let (index, modules) = canonical(&source);
        let p = Program::declare(&index, &modules);
        assert!(!p.refusals.is_empty(), "{name} must not be shareable");
    }
}
