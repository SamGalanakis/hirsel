//! Poison-bit law for the C boundary, shared by the Cranelift and LLVM native
//! harnesses.
//!
//! The C ABI leaves some register and stack-slot bits unspecified: under the
//! System V x86-64 convention clang follows, a narrow integer is extended to
//! 32 bits and bits 32..63 are garbage; under AAPCS64 every bit above the type
//! width is garbage. A float in a vector register leaves the lane above it
//! unspecified. The law fills exactly those bits with seeded garbage, in both
//! directions, and checks every value against a plain C oracle compiled by the
//! harness's C compiler:
//!
//! - Hirsel to C: a CoreIR function narrows poisoned words and passes them to a
//!   runtime adapter whose `--wrap` stand-in reads each argument as a full
//!   `uint64_t`, in registers and on the stack, and checks the defined bits.
//! - C to Hirsel: seeded signatures over bool, i8..u64, f32, f64 and ptr, up to
//!   fourteen parameters so some spill to the stack, return a seeded subset of
//!   the parameters and their widenings; results range from a lone status byte
//!   through register pairs, mixed int/float records and memory (sret) returns.
//! - C to Hirsel returns: a wrapped runtime call returns a narrow zero with the
//!   undefined bits poisoned.
//!
//! The C source names a CoreIR function `n` as `@Fn@`; the harness replaces it
//! with the function's symbol, binds sites and adds `-Wl,--wrap=` for every
//! `__wrap_` symbol. The program prints `FAIL ...` lines and then `ok <cells>`.
use std::fmt::Write as _;

pub struct Law {
    pub core: String,
    pub c: String,
    pub cells: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum T {
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    Ptr,
}
const TYPES: [T; 12] = [
    T::Bool,
    T::I8,
    T::U8,
    T::I16,
    T::U16,
    T::I32,
    T::U32,
    T::I64,
    T::U64,
    T::F32,
    T::F64,
    T::Ptr,
];
impl T {
    fn core(self) -> &'static str {
        match self {
            T::Bool => "bool",
            T::I8 => "i8",
            T::U8 => "u8",
            T::I16 => "i16",
            T::U16 => "u16",
            T::I32 => "i32",
            T::U32 => "u32",
            T::I64 => "i64",
            T::U64 => "u64",
            T::F32 => "f32",
            T::F64 => "f64",
            T::Ptr => "ptr",
        }
    }
    fn c(self) -> &'static str {
        match self {
            T::Bool | T::U8 => "uint8_t",
            T::I8 => "int8_t",
            T::I16 => "int16_t",
            T::U16 => "uint16_t",
            T::I32 => "int32_t",
            T::U32 => "uint32_t",
            T::I64 => "int64_t",
            T::U64 => "uint64_t",
            T::F32 => "float",
            T::F64 => "double",
            T::Ptr => "uint64_t",
        }
    }
    /// The C type a poisoned argument travels as: a full register or slot.
    fn wide(self) -> &'static str {
        match self {
            T::F32 | T::F64 => "double",
            _ => "uint64_t",
        }
    }
    /// The C expression for the true value of word `w`.
    fn value(self, w: &str) -> String {
        match self {
            T::Bool => format!("(uint8_t)({w} & 1)"),
            T::F32 => format!("((float)((int64_t)({w} % 2001) - 1000) / 8.0f)"),
            T::F64 => format!("((double)((int64_t)({w} % 200001) - 100000) / 64.0)"),
            T::Ptr => w.to_string(),
            _ => format!("({}){w}", self.c()),
        }
    }
    /// The C expression that poisons value `x` under garbage word `g`.
    fn poison(self, x: &str, g: &str) -> String {
        match self {
            T::Bool | T::U8 | T::U16 | T::U32 => format!("poison_u({x}, {}, {g})", self.bits()),
            T::I8 | T::I16 | T::I32 => format!("poison_s({x}, {}, {g})", self.bits()),
            T::F32 => format!("poison_f32({x}, {g})"),
            T::I64 | T::U64 | T::Ptr => format!("(uint64_t){x}"),
            T::F64 => x.to_string(),
        }
    }
    fn bits(self) -> u32 {
        match self {
            T::Bool | T::I8 | T::U8 => 8,
            T::I16 | T::U16 => 16,
            T::I32 | T::U32 | T::F32 => 32,
            _ => 64,
        }
    }
    fn signed(self) -> bool {
        matches!(self, T::I8 | T::I16 | T::I32 | T::I64)
    }
    fn int(self) -> bool {
        !matches!(self, T::F32 | T::F64 | T::Ptr)
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const PRELUDE: &str = r#"#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "hirsel_core_rt.h"
extern HirselCoreContext *hirsel_core_test_context_new(uint64_t);
static unsigned long fails, cells;
#define FAIL(...) do { fails++; printf("FAIL " __VA_ARGS__); printf("\n"); } while (0)
/* The bits a C callee may rely on: x86-64 extends to 32 bits, AAPCS64 keeps
   the type width. Everything above them is the poison. */
#if defined(__x86_64__)
static const unsigned defined_bits_narrow = 32;
#elif defined(__aarch64__)
static const unsigned defined_bits_narrow = 0;
#else
#error "poison law target"
#endif
static unsigned defined_bits(unsigned bits) {
    return defined_bits_narrow > bits ? defined_bits_narrow : bits;
}
static uint64_t low(uint64_t v, unsigned bits) {
    return bits >= 64 ? v : v & ((UINT64_C(1) << bits) - 1);
}
static uint64_t extend_u(uint64_t v, unsigned bits) { return low(v, bits); }
static uint64_t extend_s(uint64_t v, unsigned bits) {
    unsigned shift = 64 - bits;
    return (uint64_t)((int64_t)(v << shift) >> shift);
}
static uint64_t poison_with(uint64_t extended, unsigned bits, uint64_t g) {
    unsigned keep = defined_bits(bits);
    return keep >= 64 ? extended : low(extended, keep) | (g & ~((UINT64_C(1) << keep) - 1));
}
static uint64_t poison_u(uint64_t v, unsigned bits, uint64_t g) { return poison_with(extend_u(v, bits), bits, g); }
static uint64_t poison_s(uint64_t v, unsigned bits, uint64_t g) { return poison_with(extend_s(v, bits), bits, g); }
static double poison_f32(float x, uint64_t g) {
    uint32_t b; memcpy(&b, &x, 4);
    uint64_t w = (g << 32) | b; double d; memcpy(&d, &w, 8); return d;
}
/* Checks a narrow integer argument read as a full register or slot. */
static void check_arg(const char *cell, int index, uint64_t raw, uint64_t want, unsigned bits, int sign) {
    unsigned keep = defined_bits(bits);
    uint64_t extended = sign ? extend_s(want, bits) : extend_u(want, bits);
    if (low(raw, keep) != low(extended, keep))
        FAIL("%s arg%d u%u: got 0x%016llx, defined bits 0..%u must be 0x%llx", cell, index, bits,
             (unsigned long long)raw, keep - 1, (unsigned long long)low(extended, keep));
}
"#;

pub fn law(seed: u64) -> Law {
    let mut rng = Rng(seed | 1);
    let mut core = String::from("core v1\n");
    let mut c = String::from(PRELUDE);
    let mut main = String::from(
        "int main(void) {\nHirselCoreContext *ctx = hirsel_core_test_context_new(UINT64_C(1000000));\n",
    );
    let mut cells = 0;
    let mut sigs = Vec::<String>::new();
    let sig = |sigs: &mut Vec<String>, text: String| -> usize {
        sigs.iter().position(|s| *s == text).unwrap_or_else(|| {
            sigs.push(text);
            sigs.len() - 1
        })
    };
    let mut functions = String::new();
    let mut insts = String::new();
    let mut function = 0usize;

    // Hirsel to C: narrowed poison words reach the runtime's narrow
    // parameters in registers and, past the sixth (x86-64) word, on the stack.
    let adapters: [(&str, &str, &str, &[T], &[T]); 2] = [
        (
            "host_connect",
            "HirselCoreHostOutcome",
            "hirsel_core_host_connect",
            &[T::U64, T::U8, T::U64, T::U64, T::U16, T::U8, T::U64, T::U64],
            &[T::U64, T::U8, T::U32, T::U64],
        ),
        (
            "host_listen",
            "HirselCoreAddressOutcome",
            "hirsel_core_host_listen",
            &[T::U64, T::U8, T::U64, T::U64, T::U16, T::U16],
            &[T::U64, T::U8, T::U32, T::U64, T::U8, T::U64, T::U64, T::U16],
        ),
    ];
    for (op, outcome, symbol, params, results) in adapters {
        let wide = sig(
            &mut sigs,
            format!(
                "({}) -> ({})",
                vec!["u64"; params.len()].join(", "),
                results.iter().map(|t| t.core()).collect::<Vec<_>>().join(", ")
            ),
        );
        let narrow = sig(
            &mut sigs,
            format!(
                "({}) -> ({})",
                params.iter().map(|t| t.core()).collect::<Vec<_>>().join(", "),
                results.iter().map(|t| t.core()).collect::<Vec<_>>().join(", ")
            ),
        );
        writeln!(insts, "inst n{function} = test@0/poison/{op} [] []").unwrap();
        writeln!(functions, "fn f{function} = n{function} g{wide} {{").unwrap();
        let inputs: Vec<_> = (0..params.len()).map(|i| format!("v{i}: u64")).collect();
        writeln!(functions, "b0({}):", inputs.join(", ")).unwrap();
        let mut next = params.len();
        let mut operands = Vec::new();
        for (i, t) in params.iter().enumerate() {
            if *t == T::U64 {
                operands.push(format!("v{i}"));
            } else {
                writeln!(functions, "  v{next}: {} = truncate v{i}", t.core()).unwrap();
                operands.push(format!("v{next}"));
                next += 1;
            }
        }
        let outs: Vec<_> = results
            .iter()
            .enumerate()
            .map(|(j, t)| format!("v{}: {}", next + j, t.core()))
            .collect();
        writeln!(
            functions,
            "  {} = call rt.{op} g{narrow} ({}) fail b1",
            outs.join(", "),
            operands.join(", ")
        )
        .unwrap();
        let names: Vec<_> = (0..results.len()).map(|j| format!("v{}", next + j)).collect();
        writeln!(functions, "  return {}\nb1:\n  propagate\n}}", names.join(", ")).unwrap();

        // The stand-in reads every argument as a full word.
        writeln!(c, "static uint64_t want_{op}[{}];", params.len()).unwrap();
        writeln!(c, "static const char *cell_{op};").unwrap();
        let wide_params: Vec<_> = (0..params.len()).map(|i| format!("uint64_t a{i}")).collect();
        writeln!(
            c,
            "{outcome} __wrap_{symbol}(HirselCoreContext *ctx, {}, uint64_t site) {{ (void)ctx; (void)site;",
            wide_params.join(", ")
        )
        .unwrap();
        for (i, t) in params.iter().enumerate() {
            if *t != T::U64 {
                writeln!(c, "  check_arg(cell_{op}, {i}, a{i}, want_{op}[{i}], {}, 0);", t.bits()).unwrap();
            } else {
                writeln!(c, "  if (a{i} != want_{op}[{i}]) FAIL(\"%s arg{i} u64 changed\", cell_{op});").unwrap();
            }
        }
        writeln!(c, "  {outcome} out; memset(&out, 0, sizeof out); return out; }}").unwrap();
        // A packed result record for the Hirsel entry.
        writeln!(c, "struct r{function} {{").unwrap();
        for (j, t) in results.iter().enumerate() {
            writeln!(c, "  {} v{j};", t.c()).unwrap();
        }
        writeln!(c, "  uint8_t status; }};").unwrap();
        writeln!(
            c,
            "extern struct r{function} @F{function}@(HirselCoreContext *, {}, uint64_t);",
            vec!["uint64_t"; params.len()].join(", ")
        )
        .unwrap();
        for k in 0..8 {
            let words: Vec<u64> = (0..params.len()).map(|_| rng.next()).collect();
            let cell = format!("{op}#{k}");
            main.push_str("{ ");
            for (i, w) in words.iter().enumerate() {
                write!(main, "want_{op}[{i}] = UINT64_C({w:#x}); ").unwrap();
            }
            writeln!(
                main,
                "cell_{op} = \"{cell}\"; struct r{function} r = @F{function}@(ctx, {}, 0); if (r.status) FAIL(\"{cell} status %u\", r.status); cells++; }}",
                words.iter().map(|w| format!("UINT64_C({w:#x})")).collect::<Vec<_>>().join(", ")
            )
            .unwrap();
            cells += 1;
        }
        function += 1;
    }

    // C to Hirsel returns: the runtime hands back a narrow zero whose
    // undefined bits are poisoned; Hirsel must not read it as cancelled.
    {
        let unit = sig(&mut sigs, "() -> ()".into());
        let word = sig(&mut sigs, "(u64) -> (u64)".into());
        writeln!(insts, "inst n{function} = test@0/poison/check_cancel [] []").unwrap();
        writeln!(
            functions,
            "fn f{function} = n{function} g{word} {{\nb0(v0: u64):\n  call rt.check_cancel g{unit} () fail b1\n  return v0\nb1:\n  propagate\n}}"
        )
        .unwrap();
        writeln!(c, "static uint64_t cancel_poison;").unwrap();
        c.push_str("uint64_t __wrap_hirsel_core_check_cancel(HirselCoreContext *ctx, uint64_t site) { (void)ctx; (void)site; return poison_u(0, 8, cancel_poison); }\n");
        writeln!(c, "struct r{function} {{ uint64_t v0; uint8_t status; }};").unwrap();
        writeln!(c, "extern struct r{function} @F{function}@(HirselCoreContext *, uint64_t, uint64_t);").unwrap();
        for k in 0..4 {
            let g = rng.next() | 0x8000_0000_8000_0100;
            writeln!(
                main,
                "{{ cancel_poison = UINT64_C({g:#x}); struct r{function} r = @F{function}@(ctx, {k}, 0); if (r.status || r.v0 != {k}) FAIL(\"check_cancel#{k} poisoned zero read as cancelled: status %u\", r.status); cells++; }}"
            )
            .unwrap();
            cells += 1;
        }
        function += 1;
    }

    // C to Hirsel: seeded signatures.
    for k in 0..48 {
        let count = 1 + rng.below(14) as usize;
        let params: Vec<T> = (0..count)
            .map(|_| TYPES[rng.below(TYPES.len() as u64) as usize])
            .collect();
        // Candidates: each parameter, its 64-bit widening, f32 resized.
        let mut candidates: Vec<(T, String, String)> = Vec::new(); // (type, core def, C oracle)
        for (i, t) in params.iter().enumerate() {
            candidates.push((*t, String::new(), format!("x{i}")));
            if t.int() && t.bits() < 64 && *t != T::Bool {
                let to = if t.signed() { T::I64 } else { T::U64 };
                candidates.push((to, format!("widen v{i}"), format!("({})x{i}", to.c())));
            }
            if *t == T::F32 {
                candidates.push((T::F64, format!("float_resize v{i}"), format!("(double)x{i}")));
            }
        }
        let chosen = rng.below(9) as usize;
        let picks: Vec<usize> = (0..chosen.min(candidates.len()))
            .map(|_| rng.below(candidates.len() as u64) as usize)
            .collect();
        let results: Vec<T> = picks.iter().map(|&p| candidates[p].0).collect();
        let g = sig(
            &mut sigs,
            format!(
                "({}) -> ({})",
                params.iter().map(|t| t.core()).collect::<Vec<_>>().join(", "),
                results.iter().map(|t| t.core()).collect::<Vec<_>>().join(", ")
            ),
        );
        writeln!(insts, "inst n{function} = test@0/poison/matrix{k} [] []").unwrap();
        writeln!(functions, "fn f{function} = n{function} g{g} {{").unwrap();
        let inputs: Vec<_> = params.iter().enumerate().map(|(i, t)| format!("v{i}: {}", t.core())).collect();
        writeln!(functions, "b0({}):", inputs.join(", ")).unwrap();
        let mut next = params.len();
        let mut names = Vec::new();
        for &p in &picks {
            let (t, def, _) = &candidates[p];
            if def.is_empty() {
                // A bare parameter: its index is in the oracle name.
                let i: usize = candidates[p].2[1..].parse().unwrap();
                names.push(format!("v{i}"));
            } else {
                writeln!(functions, "  v{next}: {} = {def}", t.core()).unwrap();
                names.push(format!("v{next}"));
                next += 1;
            }
        }
        if names.is_empty() {
            functions.push_str("  return\n}\n");
        } else {
            writeln!(functions, "  return {}\n}}", names.join(", ")).unwrap();
        }

        let shape: Vec<_> = params.iter().map(|t| t.core()).collect();
        let result_shape: Vec<_> = results.iter().map(|t| t.core()).collect();
        let cell = format!("matrix{k}({}) -> ({})", shape.join(","), result_shape.join(","));
        let wide: Vec<_> = params.iter().map(|t| t.wide()).collect();
        if results.is_empty() {
            writeln!(c, "extern uint64_t @F{function}@(HirselCoreContext *, {}, uint64_t);", wide.join(", ")).unwrap();
        } else {
            writeln!(c, "struct r{function} {{").unwrap();
            for (j, t) in results.iter().enumerate() {
                writeln!(c, "  {} v{j};", t.c()).unwrap();
            }
            writeln!(c, "  uint8_t status; }};").unwrap();
            writeln!(c, "extern struct r{function} @F{function}@(HirselCoreContext *, {}, uint64_t);", wide.join(", ")).unwrap();
        }
        for round in 0..2 {
            main.push_str("{ ");
            for (i, t) in params.iter().enumerate() {
                let w = format!("UINT64_C({:#x})", rng.next());
                let g = format!("UINT64_C({:#x})", rng.next());
                write!(main, "{} x{i} = {}; ", t.c(), t.value(&w)).unwrap();
                write!(main, "{} a{i} = {}; ", t.wide(), t.poison(&format!("x{i}"), &g)).unwrap();
            }
            let args: Vec<_> = (0..params.len()).map(|i| format!("a{i}")).collect();
            let label = format!("{cell} round {round}");
            if results.is_empty() {
                // A lone status byte: the caller may rely on its defined bits.
                writeln!(
                    main,
                    "uint64_t s = @F{function}@(ctx, {}, 0); if (low(s, defined_bits(8)) != 0) FAIL(\"{label}: status register 0x%016llx\", (unsigned long long)s); cells++; }}",
                    args.join(", ")
                )
                .unwrap();
            } else {
                write!(main, "struct r{function} r = @F{function}@(ctx, {}, 0); if (r.status) FAIL(\"{label}: status %u\", r.status); ", args.join(", ")).unwrap();
                for (j, &p) in picks.iter().enumerate() {
                    let (t, _, oracle) = &candidates[p];
                    writeln!(
                        main,
                        "{{ {} want = {oracle}; if (memcmp(&want, &r.v{j}, sizeof want)) FAIL(\"{label}: result {j} {} wrong\"); }}",
                        t.c(),
                        t.core()
                    )
                    .unwrap();
                }
                main.push_str("cells++; }\n");
            }
            cells += 1;
        }
        function += 1;
    }
    main.push_str("printf(\"ok %lu\\n\", cells); return fails != 0; }\n");
    for (i, s) in sigs.iter().enumerate() {
        writeln!(core, "sig g{i} = {s}").unwrap();
    }
    core.push_str(&insts);
    core.push_str(&functions);
    c.push_str(&main);
    Law { core, c, cells }
}

/// The failing cells of a run's output, or why the output is malformed.
pub fn failures(law: &Law, stdout: &str) -> Vec<String> {
    let mut failures: Vec<String> = stdout
        .lines()
        .filter(|l| l.starts_with("FAIL "))
        .map(str::to_owned)
        .collect();
    if !stdout.lines().any(|l| l == format!("ok {}", law.cells)) {
        failures.push(format!("the run did not finish all {} cells:\n{stdout}", law.cells));
    }
    failures
}

/// `--wrap` symbols a law's C defines.
pub fn wraps(c: &str) -> Vec<String> {
    let mut out: Vec<String> = c
        .match_indices("__wrap_")
        .map(|(i, _)| {
            c[i + 7..]
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect()
        })
        .collect();
    out.sort();
    out.dedup();
    out
}
