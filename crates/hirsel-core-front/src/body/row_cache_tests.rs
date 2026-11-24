use super::*;

#[test]
fn only_mentioned_function_rows_occupy_the_cache() {
    let mut source = (0..64)
        .map(|i| format!("fn unused{i}() -> u64 {{ 0 }}\n"))
        .collect::<String>();
    source.push_str("fn only() -> u64 { 42 }\n");
    let (index, modules) = crate::tests::program_inputs(&source);
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let f = p.fns.iter().position(|f| f.path.0.name == "only").unwrap();
    assert!(f >= 64);
    let mut e = Emitter::new();
    let mut t = Local::with_program(&p);
    let row = e.row(&p, &mut t, f).unwrap();
    assert_eq!(e.rows.len(), 1, "unmentioned definitions took cache slots");
    assert_eq!(e.module.functions.len(), 1);
    assert_eq!(e.row(&p, &mut t, f).unwrap(), row);
    assert_eq!(e.rows.len(), 1);
}
