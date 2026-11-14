//! A fiber's guard is the only thing between a deep recursion and the memory below its stack,
//! and no program can observe whether a frame wider than the guard was probed: an unprobed
//! frame lands wherever the kernel mapped something, which may fault in the guard anyway or
//! may be writable. So the emitter's setting is the evidence: every function `emit_object`
//! emits, on every target, carries the inline stack probe at an interval within the guard.
//! (Hand-checked once against disassembly: a 72,000-byte frame gets a page-stride probe loop
//! with the attribute and none without it.)
use super::*;

/// A frame several guards wide, a small frame and a declaration, whose attributes the IR
/// sets none of.
const IR: &str = r#"
declare void @external(ptr)

define void @wide() {
  %frame = alloca [262144 x i8], align 16
  call void @external(ptr %frame)
  ret void
}

define void @narrow() {
  %frame = alloca [16 x i8], align 16
  call void @external(ptr %frame)
  ret void
}
"#;

/// The value of `name` in each defined function's attribute group, by function name.
fn defined_attribute(text: &str, name: &str) -> Vec<(String, Option<String>)> {
    let key = format!("\"{name}\"=\"");
    let group = |id: &str| {
        let prefix = format!("attributes {id} = {{");
        let line = text.lines().find(|line| line.starts_with(&prefix))?;
        let value = &line[line.find(&key)? + key.len()..];
        Some(value[..value.find('"')?].to_owned())
    };
    text.lines()
        .filter(|line| line.starts_with("define "))
        .map(|line| {
            let function = line[line.find('@').unwrap() + 1..line.find('(').unwrap()].to_owned();
            let id = line
                .split_whitespace()
                .find(|word| word.starts_with('#'))
                .map(str::to_owned);
            (function, id.and_then(|id| group(&id)))
        })
        .collect()
}

#[test]
fn every_emitted_function_probes_its_frame_within_the_guard() {
    for target in [
        TargetSpec::x86_64_unknown_linux_gnu(),
        TargetSpec::aarch64_unknown_linux_gnu(),
    ] {
        let machine = TargetMachine::new(&target, true).unwrap();
        let context = Context::new();
        let module = Module::parse(&context, "probes", IR).unwrap();
        module.set_target(&machine);
        module.emit_object(&machine).unwrap();
        assert_probes(&module.text(), &target.triple, 2);
    }
    // The lazy C-ABI phase must probe just as the direct Rust phase does.
    // This live 256 KiB frame makes a missing or different probe interval
    // observable in the object even when a program would fault by accident.
    for speed in [false, true] {
        let machine = TargetMachine::new(&TargetSpec::x86_64_unknown_linux_gnu(), speed).unwrap();
        let context = Context::new();
        let module = Module::parse(&context, "hirsel-core", IR).unwrap();
        module.set_target(&machine);
        if speed {
            module.run_core_passes(&machine).unwrap();
        }
        let direct = module.emit_object(&machine).unwrap();
        assert_probes(&module.text(), "lazy CoreIR reference", 2);
        let lazy = hirsel_llvm_lazy::compile_ir(IR, speed).unwrap();
        assert_eq!(lazy, direct, "lazy probe interval, speed={speed}");
        if speed {
            let composed = hirsel_llvm_lazy::emit_program(
                &[hirsel_llvm_lazy::Unit::Text {
                    ir: IR,
                    imported: false,
                }],
                &["wide".into(), "narrow".into()],
            )
            .unwrap();
            assert_eq!(composed, direct, "release composition probe interval");
        }
    }
    // Published RAW bitcode starts without probe attributes. Exercise a live
    // wide frame from the import itself, and compare with the direct emitter
    // whose probe settings are inspected above, before and after composition.
    let wide = hirsel_core_ir::text::parse(
        r#"core v1
type t0 = own "Wide" { bytes: array 262144 u8 }
sig g0 = () -> ()
sig g1 = (ptr) -> ()
inst n0 = tests@0/probes/wide [] []
inst n1 = tests@0/probes/external [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 : t0
b0:
  live s0
  address a0
  v0: ptr = pointer a0
  call f1 g1 (v0)
  dead s0
  return
}
fn f1 = n1 g1 extern
"#,
    )
    .unwrap();
    let facts = hirsel_core_ir::roles::Facts::new(&wide);
    let target = hirsel_core_layout::Target::X86_64Linux;
    let raw = crate::core::emit_ir_relocatable_with_role_facts_for_target(
        &wide, target, false, false, &facts,
    )
    .unwrap();
    let machine = TargetMachine::core(target, true).unwrap();
    let context = Context::new();
    let direct_module = Module::parse(&context, "hirsel-core", &raw).unwrap();
    direct_module.set_target(&machine);
    direct_module.run_core_passes(&machine).unwrap();
    let direct = direct_module.emit_object(&machine).unwrap();
    assert_probes(&direct_module.text(), "published CoreIR", 1);
    let library = hirsel_llvm_lazy::compile_library(&wide, &facts, target, false, false).unwrap();
    assert_eq!(library.object, direct, "publication probe interval");
    let exported = crate::core::symbol(&wide, hirsel_core_ir::FuncId(0));
    // Keep the imported definition in the owned wrapper even if the inliner
    // chooses to retain its large frame as an out-of-line call.
    let wrapper = format!(
        "declare i8 @{exported}(ptr, i64)\ndefine i8 @entry(ptr %ctx, i64 %site) {{\n%status = call i8 @{exported}(ptr %ctx, i64 %site) alwaysinline\nret i8 %status\n}}\n"
    );
    let exports = ["entry".to_owned()];
    let reference = hirsel_llvm_lazy::emit_program(
        &[
            hirsel_llvm_lazy::Unit::Text {
                ir: &raw,
                imported: true,
            },
            hirsel_llvm_lazy::Unit::Text {
                ir: &wrapper,
                imported: false,
            },
        ],
        &exports,
    )
    .unwrap();
    let decoded = hirsel_llvm_lazy::emit_program(
        &[
            hirsel_llvm_lazy::Unit::Import {
                bitcode: &library.bitcode,
                summary: &library.summary,
            },
            hirsel_llvm_lazy::Unit::Text {
                ir: &wrapper,
                imported: false,
            },
        ],
        &exports,
    )
    .unwrap();
    assert_eq!(decoded, reference, "bitcode import probe interval");
    let core_ir = hirsel_core_ir::text::parse(
        r#"core v1
type t0 = own "Probe" { value: u64 }
type t1 = own "Node" { children: seq rc t1 }
sig g0 = (u64) -> (u64)
sig g1 = (ptr) -> ()
inst n0 = tests@0/probes/core [] []
inst n1 = tests@0/probes/infallible [] []
inst n2 = tests@0/probes/collections [] []
fn f0 = n0 g0 {
  slot s0 = t0
  addr a0 = s0 field 0 : u64
  addr a1 = s0 : t0
b0(v0: u64):
  live s0
  address a0
  address a1
  store a0, v0
  drop a1 t0
  dead s0
  return v0
}
fn f1 = n1 g0 {
b0(v0: u64):
  return v0
}
fn f2 = n2 g1 {
b0(v0: ptr):
  release v0 rc t1
  return
}
"#,
    )
    .unwrap();
    for (opt, definitions) in [
        (crate::core::Optimization::None, 8),
        (crate::core::Optimization::Speed, 5),
    ] {
        let text = crate::core::emitted_ir(&core_ir, opt).unwrap();
        assert_probes(&text, "CoreIR x86_64", definitions);
    }
}

fn assert_probes(text: &str, target: &str, count: usize) {
    let kinds = defined_attribute(text, "probe-stack");
    let sizes = defined_attribute(text, "stack-probe-size");
    assert_eq!(kinds.len(), count, "{text}");
    for ((function, kind), (_, size)) in kinds.iter().zip(&sizes) {
        assert_eq!(kind.as_deref(), Some("inline-asm"), "{target} {function}");
        let size: usize = size.as_deref().unwrap().parse().unwrap();
        assert!(
            size <= hirsel_lir::runtime_abi::STACK_GUARD_BYTES,
            "{target} {function}: probes every {size} bytes"
        );
    }
}
