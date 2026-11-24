//! Release code generation can regress while returning the same values.
use super::{Case, Optimization, execute_object, output, symbol};
use hirsel_core_ir::{FuncId, eval};
use hirsel_core_layout::Target;
use hirsel_llvm_lazy::Unit;
use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
use std::{ops::Range, process::Command};

// Isolate the machine-code kernels from source-level Step accounting. The
// simd source agreement test in llvm_source.rs covers the metered forms.
const KERNELS: &str = r#"
core v1
static d0 = align 1 hex 1334557697b8d9fa0c2d4e6f90b1d2f3
static d1 = align 1 hex 000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
sig g0 = (u8, u8) -> (u64)
sig g1 = (i64) -> (i64)
inst n0 = app@1/pins/lookup [] []
inst n1 = app@1/pins/by_seven [] []
fn f0 = n0 g0 versioned {
b0(v0: u8, v1: u8):
    v2: ptr = static d0
    v3: v16xu8 = simd from v2
    v4: ptr = static d1
    v5: v32xu8 = simd from v4
    v6: v32xu8 = simd splat v0
    v7: v32xu8 = simd wrapping_add v5, v6
    v8: v32xu8 = simd swizzle v3, v7
    v9: v32xu8 = simd splat v1
    v10: v32xu8 = simd eq v8, v9
    v11: u64 = simd bitmask v10
    return v11
}
fn f1 = n1 g1 {
b0(v0: i64):
    v1: i64 = const 7
    v2: i64 = div v0, v1
    v3: i64 = rem v0, v1
    v4: i64 = add v2, v3
    return v4
}
"#;

#[cfg(target_os = "linux")]
#[test]
fn masked_cell_loops_match_the_evaluator_in_release() {
    // CoreIR isolates the release loop from source-level step charging. The trip
    // count stays a parameter so the unroller needs a runtime remainder.
    let shapes: [(&str, u64, u64, &str, &[u64]); 6] = [
        ("eight", 8, 0, "const 0", &[0, 1, 7, 8, 9, 19]),
        ("four", 4, 0, "const 0", &[5, 11]),
        ("aligned_start", 8, 8, "const 0", &[9, 19]),
        ("unaligned_start", 8, 3, "const 0", &[9, 19]),
        ("six", 6, 0, "const 0", &[7, 15]),
        ("aliased", 8, 0, "read a2", &[9, 19]),
    ];
    let mut source = String::from("core v1\nsig g0 = (u64) -> (u64)\n");
    let mut cases = Vec::new();
    for (index, &(name, width, start, other_cell, counts)) in shapes.iter().enumerate() {
        use std::fmt::Write as _;
        write!(
            source,
            r#"
type t{index} = own "{name}" {{ cells: array {width} u64 }}
inst n{index} = app@1/masked/{name} [] []
fn f{index} = n{index} g0 {{
  slot s0 = t{index}
  addr a0 = s0 field 0 element v5 : u64
  addr a1 = s0 field 0 element v10 : u64
  addr a2 = s0 field 0 element v1 : u64
  addr a3 = s0 field 0 element v18 : u64
b0(v0: u64):
  live s0
  v1: u64 = const 0
  v2: u64 = const 1
  v3: u64 = const 8
  v4: u64 = const {width}
  address a2
  jump b1(v1)
b1(v5: u64):
  address a0 fail b5
  v6: u64 = add v5, v2
  store a0, v6
  v7: bool = lt v6, v4
  branch v7, b1(v6), b2
b2:
  v8: u64 = const {start}
  v9: bool = eq v0, v1
  v17: u64 = wrapping_add v0, v8
  branch v9, b4(v1, v1), b3(v8)
b3(v16: u64):
  v10: u64 = rem v16, v4
  address a1 fail b5
  v11: u64 = read a1
  v12: u64 = {other_cell}
  v13: u64 = wrapping_add v11, v12
  v14: u64 = wrapping_add v16, v2
  v15: u64 = wrapping_add v13, v14
  store a1, v15
  v23: bool = ne v14, v17
  branch v23, b3(v14), b4(v1, v1)
b4(v18: u64, v19: u64):
  address a3 fail b5
  v20: u64 = read a3
  v21: u64 = const 255
  v22: u64 = bitand v20, v21
  v24: u64 = mul v18, v3
  v25: u64 = shl v22, v24
  v26: u64 = bitor v19, v25
  v27: u64 = add v18, v2
  v28: bool = lt v27, v4
  branch v28, b4(v27, v26), b6
b5:
  propagate
b6:
  return v26
}}
"#
        )
        .unwrap();
        cases.extend(counts.iter().map(|&count| Case {
            function: FuncId(index as u32),
            args: vec![count],
            limit: u64::MAX,
            injection: None,
        }));
    }
    let program = hirsel_core_ir::text::parse(&source).unwrap();
    hirsel_core_ir::wf::module(&program).unwrap();
    let facts = hirsel_core_ir::roles::Facts::new(&program);
    let units = [Unit::Core {
        module: &program,
        facts: &facts,
        observe_charge: false,
        debug_lines: false,
    }];
    let exports = (0..shapes.len())
        .map(|i| symbol(&program, FuncId(i as u32)))
        .collect::<Vec<_>>();
    let object =
        hirsel_llvm_lazy::emit_program_for_target(&units, &exports, Target::X86_64Linux).unwrap();
    let reports = execute_object(&program, &cases, "", "", Optimization::Speed, Some(object));
    for (case, native) in cases.iter().zip(reports) {
        let oracle = eval::run(&program, case.function, &case.args, case.limit);
        let name = shapes[case.function.0 as usize].0;
        assert!(
            matches!(oracle.outcome, eval::Outcome::Returned(_)),
            "{name}({:?}): {:?}",
            case.args,
            oracle.outcome
        );
        assert_eq!(native.outcome, oracle.outcome, "{name}({:?})", case.args);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kernel {
    Lookup,
    BySeven,
}

impl Kernel {
    fn function(self) -> FuncId {
        match self {
            Self::Lookup => FuncId(0),
            Self::BySeven => FuncId(1),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Lookup => "lookup",
            Self::BySeven => "by_seven",
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Body {
    kernel: Kernel,
    triple: &'static str,
    isa: Option<u8>,
}

impl Body {
    fn symbol(self, base: &str) -> String {
        match self.isa {
            Some(level) => format!("{base}.isa{level}"),
            None => base.to_owned(),
        }
    }

    fn label(self) -> String {
        match self.isa {
            Some(level) => format!("{} {} v{level}", self.kernel.name(), self.triple),
            None => format!("{} {}", self.kernel.name(), self.triple),
        }
    }
}

const X86: &str = "x86_64-unknown-linux-gnu";
const ARM: &str = "aarch64-unknown-linux-gnu";
const LOOKUP_V2: Body = Body {
    kernel: Kernel::Lookup,
    triple: X86,
    isa: Some(2),
};
const LOOKUP_V3: Body = Body {
    isa: Some(3),
    ..LOOKUP_V2
};
const LOOKUP_ARM: Body = Body {
    triple: ARM,
    isa: None,
    ..LOOKUP_V2
};
const DIVIDE_X86: Body = Body {
    kernel: Kernel::BySeven,
    triple: X86,
    isa: None,
};
const DIVIDE_ARM: Body = Body {
    triple: ARM,
    ..DIVIDE_X86
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Width {
    Xmm,
    Ymm,
    Neon16Bytes,
}

#[derive(Clone, Copy, Debug)]
enum Metric {
    ByteShuffle(Width),
    Shuffle(Width),
    IntegerDivide,
    DivideOrRemainderCall,
    Stack,
}

#[derive(Clone, Copy)]
enum Bound {
    AtLeast(usize),
    AtMost(usize),
}

use Bound::{AtLeast, AtMost};
use Metric::*;
use Width::*;

const PINS: &[(Body, Metric, Bound)] = &[
    (LOOKUP_V2, ByteShuffle(Xmm), AtLeast(1)),
    (LOOKUP_V2, Shuffle(Ymm), AtMost(0)),
    (LOOKUP_V2, Stack, AtMost(0)),
    (LOOKUP_V3, ByteShuffle(Ymm), AtLeast(1)),
    (LOOKUP_V3, Stack, AtMost(0)),
    (DIVIDE_X86, IntegerDivide, AtMost(0)),
    (DIVIDE_X86, DivideOrRemainderCall, AtMost(0)),
    (DIVIDE_X86, Stack, AtMost(0)),
    (LOOKUP_ARM, ByteShuffle(Neon16Bytes), AtLeast(1)),
    (LOOKUP_ARM, Stack, AtMost(0)),
    (DIVIDE_ARM, IntegerDivide, AtMost(0)),
    (DIVIDE_ARM, DivideOrRemainderCall, AtMost(0)),
    (DIVIDE_ARM, Stack, AtMost(0)),
];

#[cfg(target_os = "linux")]
#[test]
fn release_machine_code_pins() {
    let program = hirsel_core_ir::text::parse(KERNELS).unwrap();
    hirsel_core_ir::wf::module(&program).unwrap();
    let facts = hirsel_core_ir::roles::Facts::program(&[&program], &[]);
    let units = [Unit::Core {
        module: &program,
        facts: &facts[0],
        observe_charge: false,
        debug_lines: false,
    }];
    let scratch = super::host_memory::NativeArtifacts::new();
    let objdump = std::path::PathBuf::from(
        std::env::var_os("LLVM_SYS_231_PREFIX").expect("pinned LLVM toolchain"),
    )
    .join("bin/llvm-objdump");
    let mut failures = Vec::new();
    for target_pins in PINS.chunk_by(|a, b| a.0.triple == b.0.triple) {
        let triple = target_pins[0].0.triple;
        let target = Target::from_triple(triple)
            .unwrap_or_else(|| panic!("unsupported codegen pin target {triple}"));
        let bodies: Vec<_> = target_pins
            .chunk_by(|a, b| a.0 == b.0)
            .map(|pins| {
                let body = pins[0].0;
                let name = body.symbol(&symbol(&program, body.kernel.function()));
                (body, name, pins)
            })
            .collect();
        let mut exports: Vec<_> = bodies.iter().map(|(_, name, _)| name.clone()).collect();
        if target == Target::X86_64Linux {
            // The native semantic check calls through the public dispatcher.
            exports.push(symbol(&program, Kernel::Lookup.function()));
        }
        let bytes = hirsel_llvm_lazy::emit_program_for_target(&units, &exports, target).unwrap();
        if target == Target::X86_64Linux {
            // At key 248 the additions wrap at lane 8. Table indices 0 and 15
            // then land at lanes 8 and 23, and lanes 0..8 and 24..32 yield zero.
            let cases = [
                (Kernel::Lookup, vec![248, 19], 1 << 8),
                (Kernel::Lookup, vec![248, 243], 1 << 23),
                (Kernel::Lookup, vec![248, 0], 0xff00_00ff),
                (Kernel::BySeven, vec![15], 3),
                (Kernel::BySeven, vec![-15i64 as u64], -3i64 as u64),
            ];
            let calls = cases.each_ref().map(|(kernel, args, _)| Case {
                function: kernel.function(),
                args: args.clone(),
                limit: 1_000,
                injection: None,
            });
            let native = execute_object(
                &program,
                &calls,
                "",
                "",
                Optimization::Speed,
                Some(bytes.clone()),
            );
            for ((case, (_, _, expected)), native) in calls.iter().zip(cases).zip(native) {
                let run = eval::run(&program, case.function, &case.args, case.limit);
                assert_eq!(run.outcome, eval::Outcome::Returned(vec![expected]));
                assert_eq!(native.outcome, run.outcome, "{:?}", case.args);
            }
        }
        let path = scratch.path().join(format!("{triple}.o"));
        std::fs::write(&path, &bytes).unwrap();
        let file = object::File::parse(bytes.as_slice()).unwrap();
        assert_eq!(file.format(), object::BinaryFormat::Elf);
        assert_eq!(
            file.architecture(),
            match target {
                Target::X86_64Linux => object::Architecture::X86_64,
                Target::Aarch64Linux | Target::Aarch64Darwin => object::Architecture::Aarch64,
            }
        );
        for (body, name, pins) in bodies {
            let label = body.label();
            let (section, extent) = extent(&file, &name, &label);
            let dump = output(
                Command::new(&objdump)
                    .args(["-drz", &format!("--triple={triple}"), "--section", section])
                    .arg(format!("--start-address={:#x}", extent.start))
                    .arg(format!("--stop-address={:#x}", extent.end))
                    .arg(&path),
            );
            let insns = instructions(&dump, &extent, target, &label);
            let calls: Vec<_> = insns
                .iter()
                .filter_map(|insn| insn.call_target(target, &extent, &name, &label))
                .collect();
            for &(_, metric, bound) in pins {
                let count = match metric {
                    DivideOrRemainderCall => calls
                        .iter()
                        .filter(|callee| divide_or_remainder(callee))
                        .count(),
                    _ => insns
                        .iter()
                        .filter(|insn| insn.matches(metric, target))
                        .count(),
                };
                let (passes, comparison, limit) = match bound {
                    AtLeast(limit) => (count >= limit, ">=", limit),
                    AtMost(limit) => (count <= limit, "<=", limit),
                };
                if !passes {
                    failures.push(format!(
                        "{label}: {metric:?} expected {comparison} {limit}, found {count}"
                    ));
                }
            }
            if !calls.is_empty() {
                failures.push(format!(
                    "{label}: expected a leaf, found transfers to {calls:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn extent<'a>(file: &'a object::File<'_>, name: &str, label: &str) -> (&'a str, Range<u64>) {
    let mut definitions = file
        .symbols()
        .filter(|symbol| symbol.is_definition() && symbol.name() == Ok(name));
    let symbol = definitions
        .next()
        .unwrap_or_else(|| panic!("{label}: missing ELF definition {name}"));
    assert!(
        definitions.next().is_none(),
        "{label}: duplicate ELF definition {name}"
    );
    assert_eq!(symbol.kind(), object::SymbolKind::Text, "{label}: {name}");
    assert_ne!(symbol.size(), 0, "{label}: empty ELF definition {name}");
    let section = file
        .section_by_index(symbol.section_index().unwrap())
        .unwrap();
    let start = symbol.address();
    let end = start.checked_add(symbol.size()).unwrap();
    assert!(
        section.data_range(start, symbol.size()).unwrap().is_some(),
        "{label}: {name} outside ELF section"
    );
    (section.name().unwrap(), start..end)
}

struct Instruction<'a> {
    opcode: &'a str,
    operands: &'a str,
    extent: Range<u64>,
    relocation: Option<&'a str>,
}

fn instructions<'a>(
    dump: &'a str,
    extent: &Range<u64>,
    target: Target,
    label: &str,
) -> Vec<Instruction<'a>> {
    let hex_digits_per_word = match target {
        Target::X86_64Linux => 2,
        Target::Aarch64Linux | Target::Aarch64Darwin => 8,
    };
    let mut insns: Vec<Instruction<'_>> = Vec::new();
    let mut next = extent.start;
    for line in dump.lines() {
        let Some((address, text)) = line.trim_start().split_once(':') else {
            continue;
        };
        let Ok(address) = u64::from_str_radix(address, 16) else {
            continue;
        };
        if !extent.contains(&address) {
            continue;
        }
        let text = text.trim_start();
        if text.starts_with("R_") {
            let previous = insns
                .last_mut()
                .expect("relocation follows its instruction");
            assert!(
                previous.extent.contains(&address),
                "{label}: misplaced relocation {line}"
            );
            let callee = text.split_whitespace().nth(1).expect("relocation target");
            assert!(
                previous.relocation.replace(callee).is_none(),
                "{label}: multiple relocations in {line}"
            );
            continue;
        }
        let (encoding, assembly) = text
            .split_once('\t')
            .expect("objdump separates bytes from assembly");
        let mut size = 0;
        for word in encoding.split_whitespace() {
            assert!(
                word.len() == hex_digits_per_word && word.bytes().all(|b| b.is_ascii_hexdigit()),
                "{label}: invalid encoding in {line}"
            );
            size += (hex_digits_per_word / 2) as u64;
        }
        let assembly = assembly.trim_start();
        let (opcode, operands) = assembly
            .split_once(char::is_whitespace)
            .unwrap_or((assembly, ""));
        assert!(
            opcode.starts_with(|c: char| c.is_ascii_alphabetic()),
            "{label}: undecoded instruction at {address:#x}: {line}"
        );
        assert!(
            size > 0 && address == next && address + size <= extent.end,
            "{label}: invalid instruction extent at {address:#x}: {line}"
        );
        next = address + size;
        insns.push(Instruction {
            opcode,
            operands: operands.trim(),
            extent: address..next,
            relocation: None,
        });
    }
    assert_eq!(
        next, extent.end,
        "{label}: objdump must decode the entire ELF extent"
    );
    insns
}

impl<'a> Instruction<'a> {
    fn matches(&self, metric: Metric, target: Target) -> bool {
        match metric {
            ByteShuffle(width) => {
                matches!(self.opcode, "pshufb" | "vpshufb" | "tbl") && self.has_width(width)
            }
            Shuffle(width) => {
                (self.opcode.contains("shuf") || self.opcode.starts_with("vperm"))
                    && self.has_width(width)
            }
            IntegerDivide => {
                matches!(self.opcode, "udiv" | "sdiv")
                    || matches!(
                        self.opcode.trim_end_matches(['b', 'w', 'l', 'q']),
                        "div" | "idiv"
                    )
            }
            Stack => {
                self.registers()
                    .any(|r| matches!(r, "sp" | "wsp" | "rsp" | "esp" | "spl"))
                    || target == Target::X86_64Linux
                        && (self.opcode.starts_with("push")
                            || self.opcode.starts_with("pop") && !self.opcode.starts_with("popcnt")
                            || matches!(
                                self.opcode.trim_end_matches(['w', 'l', 'q']),
                                "enter" | "leave"
                            )
                            || self.opcode.starts_with("ret") && !self.operands.is_empty())
            }
            DivideOrRemainderCall => unreachable!("requires resolved call targets"),
        }
    }

    fn registers(&self) -> impl Iterator<Item = &str> {
        self.operands
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
            .filter(|token| !token.is_empty())
    }

    fn has_width(&self, width: Width) -> bool {
        let lane_register = match width {
            Xmm | Ymm => {
                // AT&T syntax puts the vector destination last.
                let operands = self.operands.split('#').next().unwrap();
                let destination = operands.rsplit(',').next().unwrap().trim();
                destination.strip_prefix(match width {
                    Xmm => "%xmm",
                    Ymm => "%ymm",
                    Neon16Bytes => unreachable!(),
                })
            }
            Neon16Bytes => {
                // Even an 8-byte tbl names a 16-byte table operand.
                let destination = self.operands.split(',').next().unwrap();
                destination
                    .trim()
                    .strip_prefix('v')
                    .and_then(|lanes| lanes.strip_suffix(".16b"))
            }
        };
        lane_register.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }

    fn call_target(
        &self,
        target: Target,
        extent: &Range<u64>,
        name: &str,
        label: &str,
    ) -> Option<&'a str> {
        let (call, branch) = match target {
            Target::X86_64Linux => (
                self.opcode.starts_with("call"),
                self.opcode.starts_with('j') || self.opcode.starts_with("loop"),
            ),
            Target::Aarch64Linux | Target::Aarch64Darwin => (
                matches!(self.opcode, "bl" | "blr"),
                matches!(self.opcode, "b" | "br" | "cbz" | "cbnz" | "tbz" | "tbnz")
                    || self.opcode.starts_with("b."),
            ),
        };
        if !call && !branch {
            return None;
        }
        if let Some(relocation) = self.relocation {
            let callee = relocation.split(['+', '-']).next().unwrap();
            return (call || callee != name).then_some(callee);
        }
        let (operand, destination) = self.operands.rsplit_once(" <").unwrap_or_else(|| {
            panic!(
                "{label}: unresolved control transfer {} {}",
                self.opcode, self.operands
            )
        });
        let address = operand
            .split_whitespace()
            .last()
            .unwrap()
            .trim_end_matches(',');
        let address =
            u64::from_str_radix(address.trim_start_matches("0x"), 16).expect("branch address");
        if branch && extent.contains(&address) {
            return None;
        }
        let callee = destination
            .strip_suffix('>')
            .unwrap()
            .split('+')
            .next()
            .unwrap();
        Some(callee)
    }
}

fn divide_or_remainder(callee: &str) -> bool {
    // compiler-rt/libgcc integer helpers and the C library quotient/remainder
    // functions. Relocation decorations have already been removed.
    let name = callee.trim_start_matches('_');
    [
        "div",
        "udiv",
        "mod",
        "umod",
        "rem",
        "urem",
        "aeabi_idiv",
        "aeabi_uidiv",
        "aeabi_ldiv",
        "aeabi_uldiv",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
        || matches!(name, "ldiv" | "lldiv" | "imaxdiv")
}
