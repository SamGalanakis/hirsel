use super::*;

fn signature<'a>(
    ir: &'a str,
    kind: &str,
    symbol: &str,
) -> Option<(&'a str, Vec<&'a str>, &'a str)> {
    let symbol = format!(" @{symbol}(");
    let (prefix, tail) = ir
        .lines()
        .find_map(|line| line.strip_prefix(kind)?.split_once(&symbol))?;
    let (params, suffix) = tail.rsplit_once(')')?;
    let params = params
        .split(", ")
        .map(|param| param.rsplit_once(" %").map_or(param, |(attrs, _)| attrs))
        .collect();
    let group = suffix
        .split_whitespace()
        .find(|word| word.starts_with('#'))?;
    let attrs = ir
        .lines()
        .find_map(|line| line.strip_prefix(&format!("attributes {group} = ")))
        .unwrap();
    Some((
        prefix.strip_prefix("hidden ").unwrap_or(prefix),
        params,
        attrs,
    ))
}

#[test]
fn release_partition_foreign_declarations_copy_definition_attributes() {
    let module = text::parse(
        "core v1
type t0 = own \"Bytes\" { data: seq u8 }
sig g0 = (&view t0) -> ()
inst n0 = tests@0/partition_attrs/stop0 [] []
inst n1 = tests@0/partition_attrs/caller0 [] []
fn f0 = n0 g0 cold {
b0(v0: ptr):
  unreachable
}
fn f1 = n1 g0 {
  addr a0 = v0 : t0
b0(v0: ptr):
  v1: u64 = const 0
  address a0
  call f0 g0 (&a0)
  return
}
",
    )
    .unwrap();
    wf::module(&module).unwrap();
    let facts = hirsel_core_ir::roles::Facts::new(&module);
    let symbols = module.link_symbols();
    let callee = format!("{}.infallible", symbols[0]);
    let caller = format!("{}.infallible", symbols[1]);
    let mut partitions = Vec::new();
    hirsel_llvm_lazy::lower_release(
        module,
        &facts,
        Target::X86_64Linux,
        false,
        true,
        1,
        &mut |partition| {
            partitions.push(partition.print()?);
            Ok(())
        },
    )
    .unwrap();

    let definition = partitions
        .iter()
        .find_map(|ir| signature(ir, "define ", &callee))
        .expect("the callee's partition defines it");
    let caller_ir = partitions
        .iter()
        .find(|ir| signature(ir, "define ", &caller).is_some())
        .expect("the caller's partition defines it");
    let declaration = signature(caller_ir, "declare ", &callee)
        .expect("the caller's partition declares the callee");
    assert!(definition.2.contains("noreturn") && definition.2.contains("nounwind"));
    assert!(definition.1.iter().any(|param| param.contains("readonly")));
    assert_eq!(declaration, definition);
}
