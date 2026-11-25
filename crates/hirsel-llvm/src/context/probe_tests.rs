//! A fiber's guard is the only thing between a deep recursion and the memory below its stack,
//! and no program can observe whether a frame wider than the guard was probed: an unprobed
//! frame lands wherever the kernel mapped something, which may fault in the guard anyway or
//! may be writable. So the emitter's setting is the evidence: every function `emit_object`
//! emits, on every target, carries the inline stack probe at an interval within the guard.
//! (Hand-checked once against disassembly: a 72,000-byte frame gets a page-stride probe loop
//! with the attribute and none without it.)
use super::*;

// The smallest page an x86-64 target maps sets the minimum fiber guard size there.
const MIN_GUARD_BYTES: usize = 4096;

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

/// The machine code `object` defines for `name`, each relocated field zeroed.
fn code(object: &[u8], name: &str) -> Option<Vec<u8>> {
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
    let file = object::File::parse(object).ok()?;
    let symbol = file
        .symbols()
        .find(|s| s.is_definition() && s.name() == Ok(name))?;
    let section = file.section_by_index(symbol.section_index()?).ok()?;
    let start = symbol.address() - section.address();
    let end = start + symbol.size();
    let mut bytes = section.data().ok()?[start as usize..end as usize].to_vec();
    for (offset, relocation) in section.relocations() {
        if (start..end).contains(&offset) {
            let at = (offset - start) as usize;
            let stop = (at + usize::from(relocation.size() / 8)).min(bytes.len());
            bytes[at..stop].fill(0);
        }
    }
    Some(bytes)
}

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
        hirsel_core_layout::Target::X86_64Linux,
        hirsel_core_layout::Target::Aarch64Linux,
        hirsel_core_layout::Target::Aarch64Darwin,
    ] {
        let machine = TargetMachine::core(target, true).unwrap();
        let context = Context::new();
        let module = Module::parse(&context, "probes", IR).unwrap();
        module.set_target(&machine);
        module.emit_object(&machine).unwrap();
        // The guard is one page of the target: probing at a smaller stride
        // only costs, a larger one can step over the guard.
        let page = 1usize << target.page_log2();
        assert_probes_every(&module.text(), target.triple(), &["wide", "narrow"], page);
    }
    // The lazy C-ABI phase must probe just as the direct Rust phase does.
    // This live 256 KiB frame makes a missing or different probe interval
    // observable in the object even when a program would fault by accident.
    for speed in [false, true] {
        let machine = TargetMachine::core(hirsel_core_layout::Target::X86_64Linux, speed).unwrap();
        let context = Context::new();
        let module = Module::parse(&context, "hirsel-core", IR).unwrap();
        module.set_target(&machine);
        if speed {
            module.run_core_passes(&machine).unwrap();
        }
        let direct = module.emit_object(&machine).unwrap();
        assert_probes(&module.text(), "lazy CoreIR reference", &["wide", "narrow"]);
        let lazy = hirsel_llvm_lazy::compile_ir(IR, speed).unwrap();
        assert_eq!(lazy, direct, "lazy probe interval, speed={speed}");
        if speed {
            // Release composition gives each function a section of its own:
            // the same code, probes included, up to relocated fields.
            let object = hirsel_llvm_lazy::emit_program(
                &[hirsel_llvm_lazy::Unit::Text(IR)],
                &["wide".into(), "narrow".into()],
            )
            .unwrap();
            for name in ["wide", "narrow"] {
                let composed = code(&object, name);
                assert_eq!(
                    composed,
                    code(&direct, name),
                    "release composition probe interval: {name}"
                );
            }
        }
    }
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
    let entries = (0..core_ir.functions.len())
        .map(|i| crate::core::symbol(&core_ir, hirsel_core_ir::FuncId(i as u32)))
        .collect::<Vec<_>>();
    let required = entries.iter().map(String::as_str).collect::<Vec<_>>();
    // Public instance entries stay available to other units. Private bodies,
    // including infallible adapters, may disappear after inlining. Probe every
    // definition that survives: emit_object attaches probes after optimisation,
    // when an inlined callee's frame belongs to its caller.
    for opt in [
        crate::core::Optimization::None,
        crate::core::Optimization::Speed,
    ] {
        let text = crate::core::emitted_ir(&core_ir, opt).unwrap();
        assert_probes(&text, "CoreIR x86_64", &required);
    }
}

fn assert_probes(text: &str, target: &str, required: &[&str]) {
    assert_probes_every(text, target, required, MIN_GUARD_BYTES);
}

fn assert_probes_every(text: &str, target: &str, required: &[&str], stride: usize) {
    let kinds = defined_attribute(text, "probe-stack");
    let sizes = defined_attribute(text, "stack-probe-size");
    for required in required {
        assert!(
            kinds.iter().any(|(function, _)| function == required),
            "{target}: missing entry {required}\n{text}"
        );
    }
    for ((function, kind), (_, size)) in kinds.iter().zip(&sizes) {
        assert_eq!(kind.as_deref(), Some("inline-asm"), "{target} {function}");
        let size: usize = size.as_deref().unwrap().parse().unwrap();
        assert_eq!(
            size, stride,
            "{target} {function}: probes every {size} bytes"
        );
    }
}
