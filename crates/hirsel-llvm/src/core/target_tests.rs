//! Executed C ABI oracle, including padding, mixed classes and ARM HFAs.
use super::*;
use hirsel_core_layout::Target;
use std::path::PathBuf;
use std::process::Command;

fn native_input(name: &str) -> std::result::Result<PathBuf, String> {
    let purpose = match name {
        "HIRSEL_AARCH64_CC" => "the AArch64 C compiler executable",
        "HIRSEL_QEMU_AARCH64" => "the QEMU AArch64 user executable",
        "HIRSEL_AARCH64_SYSROOT" => "the AArch64 runtime sysroot directory",
        "LLVM_SYS_231_PREFIX" => "the pinned LLVM installation directory",
        _ => unreachable!("native test input name"),
    };
    let path = std::env::var_os(name).map(PathBuf::from).ok_or_else(|| {
        format!("native test input unavailable: {name} is unset; set {name} to {purpose}. Buck native aliases declare these through native_tool_inputs().")
    })?;
    if !path
        .try_exists()
        .map_err(|e| format!("native test input {name}: {e}"))?
    {
        return Err(format!(
            "native test input unavailable: {name}={} does not exist; set {name} to {purpose}",
            path.display()
        ));
    }
    Ok(path)
}

fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{command:?}: {output:?}");
}

#[test]
fn linux_c_results_execute_with_mixed_fields_padding_and_hfas() -> std::result::Result<(), String> {
    let arm_cc = native_input("HIRSEL_AARCH64_CC")?;
    let qemu = native_input("HIRSEL_QEMU_AARCH64")?;
    let sysroot = native_input("HIRSEL_AARCH64_SYSROOT")?;
    let llvm_prefix = native_input("LLVM_SYS_231_PREFIX")?;
    use Scalar::*;
    let shapes: &[&[Scalar]] = &[
        &[Bool],
        &[Char],
        &[I8],
        &[U8],
        &[I16],
        &[U16],
        &[U32],
        &[U64],
        &[F32],
        &[F64],
        &[F32, U8],
        &[U8, F32],
        &[F64, U8],
        &[U8, F64],
        &[F64, U64],
        &[U64, F64],
        &[U8, U64, U8],
        &[U64, U64, U64],
        &[F32, F32],
        &[F32, F32, F32],
        &[F32, F32, F32, F32],
        &[F64, F64],
        &[F64, F64, F64],
        &[F64, F64, F64, F64],
    ];
    let module = Module::default();
    let body = Body::default();
    let failures = failure::Failures::new(&module);
    let dir = std::env::temp_dir().join(format!("hirsel-core-targets-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for target in [Target::X86_64Linux, Target::Aarch64Linux] {
        let mut ir = String::new();
        let mut declarations = BTreeSet::new();
        let mut c =
            String::from("#include <stdint.h>\n#include <stdbool.h>\n#include <assert.h>\n");
        let mut checks = String::new();
        for (i, shape) in shapes.iter().enumerate() {
            let fields = shape.iter().copied().map(Ty::Scalar).collect::<Vec<_>>();
            let mut e = FunctionEmitter {
                module: &module,
                body: &body,
                recipes: &[],
                abi: abi::Return::on_target(target, &fields),
                has_status: false,
                failures: &failures,
                constants: &[],
                empty_bounds: empty::EmptyBounds::default(),
                observe_charge: false,
                facts: false,
                aliases: None,
                caller_site: "0",
                debug_location: None,
                anchor: None,
                relocatable_sites: false,
                private_float: vec![],
                value_slots: vec![],
                code: String::new(),
                allocations: String::new(),
                next: 0,
                intrinsics: &mut declarations,
            };
            let incoming = e.native_call(
                &format!("produce_{i}"),
                &fields,
                &[("ptr".into(), "%context".into())],
            );
            let values = incoming
                .iter()
                .zip(&fields)
                .map(|(x, &t)| {
                    e.calc(if t == Ty::Scalar(Bool) {
                        format!("xor {} {x}, 1", ty(t))
                    } else if t.is_float() {
                        format!("fadd {} {x}, 1.500000e+00", ty(t))
                    } else {
                        format!("add {} {x}, 3", ty(t))
                    })
                })
                .collect::<Vec<_>>();
            e.return_values(&values);
            writeln!(
                ir,
                "define {} @probe_{i}({}) {{\nentry:\n{}{} }}",
                e.abi.return_type(),
                if e.abi.indirect {
                    format!("ptr sret({}) %sret, ptr %context", e.abi.storage)
                } else {
                    "ptr %context".into()
                },
                e.allocations,
                e.code
            )
            .unwrap();
            let c_ty = |s: Scalar| match s {
                Bool => "bool",
                I8 => "int8_t",
                U8 => "uint8_t",
                I16 => "int16_t",
                U16 => "uint16_t",
                I32 => "int32_t",
                U32 | Char => "uint32_t",
                I64 => "int64_t",
                U64 => "uint64_t",
                F32 => "float",
                F64 => "double",
            };
            if shape.len() == 1 {
                writeln!(c, "typedef {} R{i};", c_ty(shape[0])).unwrap();
            } else {
                writeln!(
                    c,
                    "typedef struct {{ {} }} R{i};",
                    shape
                        .iter()
                        .enumerate()
                        .map(|(j, s)| format!("{} f{j};", c_ty(*s)))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
                .unwrap();
            }
            let initials = shape
                .iter()
                .enumerate()
                .map(|(j, s)| match s {
                    Bool => "true".into(),
                    F32 | F64 => format!("{}.25", j + 2),
                    I8 | I16 | I32 | I64 => format!("-{}", j + 17),
                    U64 => format!("UINT64_C(0x80000000000000{:02x})", j + 17),
                    _ => format!("{}", j + 129),
                })
                .collect::<Vec<_>>();
            writeln!(c, "R{i} produce_{i}(void *context) {{ assert((uintptr_t)context == UINT64_C(0x12345678)); return (R{i}){{ {} }}; }}\nextern R{i} probe_{i}(void *);", initials.join(", ")).unwrap();
            writeln!(
                checks,
                "R{i} r{i}=probe_{i}((void *)(uintptr_t)0x12345678);"
            )
            .unwrap();
            for (j, s) in shape.iter().enumerate() {
                let field = if shape.len() == 1 {
                    format!("r{i}")
                } else {
                    format!("r{i}.f{j}")
                };
                let expected = match s {
                    Bool => "false".into(),
                    F32 | F64 => format!("{}.75", j + 3),
                    _ => format!("({})(({}) + 3)", c_ty(*s), initials[j]),
                };
                writeln!(checks, "assert({field} == {expected});").unwrap();
            }
        }
        for declaration in declarations {
            writeln!(ir, "{declaration}").unwrap();
        }
        writeln!(c, "int main(void) {{ {checks} return 0; }}").unwrap();
        std::fs::write(dir.join("probe.c"), c).unwrap();
        for speed in [false, true] {
            let machine = TargetMachine::core(target, speed).unwrap();
            let context = Context::new();
            let llvm = LlvmModule::parse(&context, "hirsel-core", &ir).unwrap();
            llvm.set_target(&machine);
            llvm.verify().unwrap();
            if speed {
                llvm.run_core_passes(&machine).unwrap();
            }
            let direct = llvm.emit_object(&machine).unwrap();
            let lazy = hirsel_llvm_lazy::compile_ir_for_target(&ir, speed, target).unwrap();
            assert_eq!(direct, lazy, "eager/lazy bytes: {target:?}, speed={speed}");
            let exports = (0..shapes.len())
                .map(|i| format!("probe_{i}"))
                .collect::<Vec<_>>();
            let mut objects = vec![lazy];
            if speed {
                objects.push(
                    hirsel_llvm_lazy::emit_program_for_target(
                        &[hirsel_llvm_lazy::Unit {
                            ir: &ir,
                            imported: false,
                        }],
                        &exports,
                        target,
                    )
                    .unwrap(),
                );
            }
            for object in objects {
                std::fs::write(dir.join("probe.o"), object).unwrap();
                let compiler = match target {
                    Target::X86_64Linux => llvm_prefix.join("bin/clang"),
                    Target::Aarch64Linux => arm_cc.clone(),
                };
                run(Command::new(compiler)
                    .arg(dir.join("probe.c"))
                    .arg(dir.join("probe.o"))
                    .arg("-o")
                    .arg(dir.join("probe")));
                match target {
                    Target::X86_64Linux => run(&mut Command::new(dir.join("probe"))),
                    Target::Aarch64Linux => run(Command::new(&qemu)
                        .arg("-L")
                        .arg(&sysroot)
                        .arg(dir.join("probe"))),
                }
            }
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
    Ok(())
}

#[test]
fn checked_arithmetic_preserves_status_context_and_cause_copies() -> std::result::Result<(), String>
{
    let arm_cc = native_input("HIRSEL_AARCH64_CC")?;
    let qemu = native_input("HIRSEL_QEMU_AARCH64")?;
    let sysroot = native_input("HIRSEL_AARCH64_SYSROOT")?;
    let llvm_prefix = native_input("LLVM_SYS_231_PREFIX")?;
    let module =
        hirsel_core_ir::text::parse(include_str!("../../../hirsel-core-ir/samples/arith.core"))
            .unwrap();
    let add = symbol(&module, FuncId(1));
    let divide = symbol(&module, FuncId(7));
    let c = format!(
        r#"
#include <stdint.h>
#include <assert.h>
#include "hirsel_core_rt.h"
static unsigned raised, kind;
void hirsel_core_raise(HirselCoreContext *context, HirselCoreCause cause) {{
    assert((uintptr_t)context == UINT64_C(0x12345678));
    ++raised; kind=hirsel_core_cause_kind(&cause);
    /* Force writes to the C callee's private copy, even at O0. */
    volatile HirselCoreCause *copy=&cause;
    copy->site=123; copy->message.capacity=0;
}}
typedef struct {{ int32_t value; uint8_t status; }} Add;
typedef struct {{ int64_t quotient, remainder; uint8_t status; }} Divide;
extern Add {add}(HirselCoreContext *, int32_t, int32_t, uint64_t);
extern Divide {divide}(HirselCoreContext *, int64_t, int64_t, uint64_t);
int main(void) {{
    HirselCoreContext *context=(void *)(uintptr_t)0x12345678;
    Add a={add}(context,2,3,0); assert(a.value==5 && a.status==0 && raised==0);
    a={add}(context,INT32_MAX,1,0); assert(a.status==1 && raised==1 && kind==0);
    a={add}(context,INT32_MIN,-1,0); assert(a.status==1 && raised==2 && kind==0);
    Divide d={divide}(context,-7,2,0); assert(d.quotient==-3 && d.remainder==-1 && d.status==0 && raised==2);
    d={divide}(context,1,0,0); assert(d.status==1 && raised==3 && kind==2);
    d={divide}(context,INT64_MIN,-1,0); assert(d.status==1 && raised==4 && kind==0);
    return 0;
}}
"#
    );
    let dir =
        std::env::temp_dir().join(format!("hirsel-core-target-failure-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("probe.c"), c).unwrap();
    let prefix = llvm_prefix;
    let include = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hirsel-core-rt/include");
    for target in [Target::X86_64Linux, Target::Aarch64Linux] {
        for speed in [false, true] {
            for facts in [false, true] {
                let ir = emit_ir_target_with_facts(
                    &module,
                    target,
                    false,
                    facts,
                    SourceEmission {
                        relocatable_sites: false,
                        debug_lines: true,
                    },
                )
                .unwrap();
                let object = hirsel_llvm_lazy::compile_ir_for_target(&ir, speed, target).unwrap();
                std::fs::write(dir.join("probe.o"), object).unwrap();
                let compiler = match target {
                    Target::X86_64Linux => prefix.join("bin/clang"),
                    Target::Aarch64Linux => arm_cc.clone(),
                };
                run(Command::new(compiler)
                    .arg("-O0")
                    .arg("-I")
                    .arg(&include)
                    .arg(dir.join("probe.c"))
                    .arg(dir.join("probe.o"))
                    .arg("-lm")
                    .arg("-o")
                    .arg(dir.join("probe")));
                match target {
                    Target::X86_64Linux => run(&mut Command::new(dir.join("probe"))),
                    Target::Aarch64Linux => run(Command::new(&qemu)
                        .arg("-L")
                        .arg(&sysroot)
                        .arg(dir.join("probe"))),
                }
            }
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
    Ok(())
}
