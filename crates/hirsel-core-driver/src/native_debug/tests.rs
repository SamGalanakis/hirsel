mod darwin;
mod macho_cut;

#[test]
fn link_rebinds_rows_and_inline_calls_without_changing_native_code() {
    super::side::tests::binding_case();
}

#[test]
fn llvm_split_preserves_distinct_cie_rules() {
    use gimli::{
        BaseAddresses, CfaRule, EhFrame, LittleEndian, Register, RegisterRule, UnwindContext,
        UnwindSection as _,
    };
    use hirsel_core_ids::ContentHash;
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
    use std::{path::PathBuf, process::Command};

    let dir = tempfile::tempdir().unwrap();
    let bin = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin");
    let assembly = dir.path().join("two-cies.s");
    std::fs::write(
        &assembly,
        r#"
.section .text.normal,"ax",@progbits
.globl normal
.type normal,@function
normal:
.cfi_startproc
ret
.cfi_endproc
.size normal, .-normal
.section .text.custom,"ax",@progbits
.globl custom
.type custom,@function
custom:
.cfi_startproc simple
.cfi_def_cfa %rbp, 16
ret
.cfi_endproc
.size custom, .-custom
"#,
    )
    .unwrap();
    let whole = dir.path().join("whole.o");
    assert!(
        Command::new(bin.join("clang"))
            .args(["--target=x86_64-unknown-linux-gnu", "-c"])
            .arg(&assembly)
            .arg("-o")
            .arg(&whole)
            .status()
            .unwrap()
            .success()
    );
    let names = ["normal", "custom"].map(|name| (name, ContentHash::of_bytes(name.as_bytes())));
    let parts = hirsel_llvm_lazy::core::split_native_object(
        &std::fs::read(whole).unwrap(),
        &names,
        false,
        None,
    )
    .unwrap();
    let paths = parts
        .into_iter()
        .zip(names)
        .map(|(part, (name, _))| {
            let path = dir.path().join(format!("{name}.o"));
            std::fs::write(&path, part.bytes).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let image = dir.path().join("image");
    assert!(
        Command::new(bin.join("ld.lld"))
            .args(["--gc-sections", "-e", "normal", "-u", "custom", "-o"])
            .arg(&image)
            .args(paths)
            .status()
            .unwrap()
            .success()
    );
    let bytes = std::fs::read(image).unwrap();
    let elf = object::File::parse(bytes.as_slice()).unwrap();
    let section = elf.section_by_name(".eh_frame").unwrap();
    let base = BaseAddresses::default().set_eh_frame(section.address());
    let frame = EhFrame::new(section.data().unwrap(), LittleEndian);
    for (name, sp, offset, ret) in [
        ("normal", 7, 8, Some(RegisterRule::Offset(-8))),
        ("custom", 6, 16, None),
    ] {
        let address = elf.symbol_by_name(name).unwrap().address();
        let mut ctx = UnwindContext::new();
        let row = frame
            .unwind_info_for_address(&base, &mut ctx, address, EhFrame::cie_from_offset)
            .unwrap();
        assert_eq!(
            *row.cfa(),
            CfaRule::RegisterAndOffset {
                register: Register(sp),
                offset
            }
        );
        assert_eq!(row.register(Register(16)), ret);
    }
}

#[test]
fn native_symbol_tiers_follow_the_elf_identity() {
    use super::{NativeImage, SymbolSource, bind_detached, resolve_symbols, source_positions};
    use hirsel_core_ir::source::*;
    use object::{Object as _, ObjectSection as _};
    use std::{fs, path::PathBuf, process::Command};

    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let bin = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin");
    let source = root.join("main.s");
    let object = root.join("main.o");
    let image = root.join("main");
    fs::write(
        &source,
        "\
        .text\n.globl start\n.type start, %function\n.globl __hirsel_debug_test\n\
        start:\n__hirsel_debug_test:\nret\n.size start, .-start\n",
    )
    .unwrap();
    assert!(
        Command::new(bin.join("clang"))
            .arg("-c")
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .status()
            .unwrap()
            .success()
    );
    let map = SourceMap {
        files: vec![SourceFile {
            path: "app@1/main.hirsel".into(),
            text: "\n x".into(),
        }],
        items: [(
            hirsel_core_ids::DefPath::from_full("app@1/main/main").unwrap(),
            ItemSource {
                file: 0,
                file_offset: 0,
            },
        )]
        .into(),
        ..Default::default()
    };
    let debug = NativeDebug {
        items: map.items.keys().cloned().collect(),
        sections: vec![CodeSection {
            symbol: "__hirsel_debug_test".into(),
            size: 1,
        }],
        sequences: vec![LineSequence {
            range: CodeRange {
                section: 0,
                begin: 0,
                end: 1,
            },
            rows: vec![LineRow {
                offset: 0,
                source: Some(NativeLocation { item: 0, start: 2 }),
            }],
        }],
        ..Default::default()
    };
    let mut old_debug = None;
    for style in ["--build-id=0x123456", "--build-id=none"] {
        assert!(
            Command::new(bin.join("ld.lld"))
                .arg(style)
                .args(["-e", "start", "-o"])
                .arg(&image)
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        let bytes = fs::read(&image).unwrap();
        let elf = object::File::parse(bytes.as_slice()).unwrap();
        assert!(
            elf.sections()
                .all(|s| !s.name().unwrap().starts_with(".debug"))
        );
        let explicit = root.join("saved.debug");
        if let Some(previous) = &old_debug {
            fs::write(&explicit, previous).unwrap();
            assert_eq!(
                resolve_symbols(&image, &bytes, Some(&explicit))
                    .unwrap()
                    .tier(),
                "symtab_only"
            );
        }
        let debug = bind_detached(
            [&std::sync::Arc::new(debug.clone())],
            &map,
            None,
            &[],
            &[],
            &bytes,
        )
        .unwrap();
        let binary = NativeImage::new(bytes.into(), debug);
        binary.deliver(&image).unwrap();
        let SymbolSource::DetachedDwarf(rows) =
            resolve_symbols(&image, &binary.bytes, None).unwrap()
        else {
            panic!("missing side product for {style}")
        };
        assert_eq!(
            source_positions(&rows).unwrap(),
            [("app@1/main.hirsel".into(), 2, 2)]
        );
        if style.ends_with("123456") {
            assert_eq!(
                fs::read(root.join(".build-id/12/3456.debug")).unwrap(),
                rows
            );
        }
        old_debug = Some(rows);
    }
    let stripped = root.join("stripped");
    assert!(
        Command::new(bin.join("llvm-objcopy"))
            .arg("--strip-all")
            .arg(&image)
            .arg(&stripped)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        resolve_symbols(&stripped, &fs::read(&stripped).unwrap(), None)
            .unwrap()
            .tier(),
        "offsets_only"
    );
}

#[test]
fn native_fiber_boundary_has_no_caller() {
    use crate::{CheckRequest, Driver, Meter, native::tests::native_options};
    use gimli::{
        BaseAddresses, CfaRule, EhFrame, LittleEndian, Register, RegisterRule, UnwindContext,
        UnwindSection as _,
    };
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
    use std::path::Path;

    let directory = tempfile::tempdir().unwrap();
    let app = directory.path();
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    std::fs::write(
        app.join("main.hirsel"),
        "pub fn main() -> Result[(), Exit] { Ok(()) }\n",
    )
    .unwrap();
    let request = CheckRequest::program(app, &core, &app.join("store")).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let entry = hirsel_core_ids::DefPath::from_full("app@1/main/main").unwrap();
    let options = native_options(crate::native::Backend::Llvm, app);
    let built = product.build(&entry, &options).unwrap();
    let bytes = std::fs::read(built.executable).unwrap();
    let elf = object::File::parse(bytes.as_slice()).unwrap();
    let section = elf.section_by_name(".eh_frame").unwrap();
    let base = BaseAddresses::default().set_eh_frame(section.address());
    let eh = EhFrame::new(section.data().unwrap(), LittleEndian);
    let launch = ["main", "__hirsel_entry"].map(|name| {
        elf.symbol_by_name(name)
            .unwrap_or_else(|| panic!("missing entry function {name}"))
    });
    let hirsel = elf.symbols().filter(|symbol| {
        symbol.kind() == object::SymbolKind::Text
            && symbol.is_definition()
            && symbol.name().is_ok_and(|name| {
                name.starts_with("hirsel.g.")
                    || name
                        .strip_prefix('h')
                        .and_then(|rest| rest.get(..64))
                        .is_some_and(|hash| hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            })
    });
    for function in launch.into_iter().chain(hirsel) {
        let name = function.name().unwrap();
        let fde = eh
            .fde_for_address(&base, function.address(), EhFrame::cie_from_offset)
            .unwrap_or_else(|error| panic!("{name} has no FDE: {error}"));
        assert_eq!(
            (fde.initial_address(), fde.len()),
            (function.address(), function.size()),
            "incomplete FDE for {name}"
        );
    }
    let (sp, ra) = match elf.architecture() {
        object::Architecture::X86_64 => (Register(7), Register(16)),
        object::Architecture::Aarch64 => (Register(31), Register(30)),
        arch => panic!("unsupported test host {arch:?}"),
    };
    for marker in ["fiber_start", "switch"] {
        let function = elf
            .symbols()
            .find(|s| {
                s.kind() == object::SymbolKind::Text
                    && s.name()
                        .is_ok_and(|name| name.contains("hirsel_stack") && name.contains(marker))
            })
            .unwrap_or_else(|| panic!("missing {marker}"));
        let fde = eh
            .fde_for_address(&base, function.address(), EhFrame::cie_from_offset)
            .unwrap();
        let mut context = UnwindContext::new();
        let row = fde
            .unwind_info_for_address(
                &eh,
                &base,
                &mut context,
                function.address() + function.size() - 1,
            )
            .unwrap();
        assert_eq!(
            *row.cfa(),
            CfaRule::RegisterAndOffset {
                register: sp,
                offset: 0
            }
        );
        assert_eq!(row.register(ra), Some(RegisterRule::Undefined));
    }
}

#[test]
fn llvm_unwind_covers_live_whole_split_and_reused_code() {
    use gimli::{BaseAddresses, CieOrFde, EhFrame, LittleEndian, UnwindSection as _};
    use hirsel_core_ids::ContentHash;
    use hirsel_core_layout::Target;
    use hirsel_llvm_lazy::{Held, Lend, Unit, core, emit_program_parts};
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[derive(Default)]
    struct Parts {
        keys: BTreeMap<String, (String, [u8; 32])>,
        reuse: bool,
    }
    impl Held for Parts {
        fn part(&mut self, name: &str, machine: &str, ir: &[u8; 32]) -> bool {
            if self.reuse {
                self.keys[name] == (machine.into(), *ir)
            } else {
                self.keys.insert(name.into(), (machine.into(), *ir));
                false
            }
        }

        fn tail(&mut self, _: &[u8], _: &[u8; 32]) -> Option<Vec<u8>> {
            None
        }

        fn keep_tail(&mut self, _: &[u8], _: &[u8; 32], _: &[u8], _: &[u8]) {}
    }

    let linker = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/ld.lld");
    let dir = tempfile::tempdir().unwrap();
    for (target, return_register) in [(Target::X86_64Linux, 16), (Target::Aarch64Linux, 30)] {
        for (section, jobs, expected_parts) in match target {
            Target::X86_64Linux => &[("", 0, 3), ("", 2, 3), ("section \".text.start\"", 0, 1)][..],
            Target::Aarch64Linux => &[("", 0, 1)][..],
            Target::Aarch64Darwin | Target::Aarch64Ios | Target::Aarch64IosSim => {
                panic!("no ELF link for {}", target.triple())
            }
        }
        .iter()
        .copied()
        {
            let ir = format!(
                "define i64 @hirsel_entry(ptr %p) {section} {{
                    %v = call i64 @hirsel_live(ptr %p)
                    ret i64 %v
                }}
                define i64 @hirsel_live(ptr %p) noinline {{
                    %v = load volatile i64, ptr %p
                    ret i64 %v
                }}
                define i64 @hirsel_dead() {{ ret i64 7 }}"
            );
            let exports = [
                "hirsel_entry".into(),
                "hirsel_live".into(),
                "hirsel_dead".into(),
            ];
            let mut parts = Parts::default();
            let emit = |held: &mut Parts| {
                emit_program_parts(&[Unit::Text(&ir)], &exports, target, jobs, held, Lend::None)
                    .unwrap()
            };
            let cold = emit(&mut parts);
            assert_eq!(cold.parts.len(), expected_parts);
            let names = cold
                .parts
                .iter()
                .map(|p| (p.name.as_str(), ContentHash::of_bytes(p.name.as_bytes())))
                .collect::<Vec<_>>();
            let objects = match cold.objects.as_slice() {
                [whole] => core::split_native_object(whole, &names, false, None).unwrap(),
                objects => objects
                    .iter()
                    .zip(&names)
                    .flat_map(|(bytes, name)| {
                        core::split_native_object(bytes, std::slice::from_ref(name), false, None)
                            .unwrap()
                    })
                    .collect(),
            };
            let paths = objects
                .iter()
                .enumerate()
                .map(|(i, object)| {
                    let path = dir.path().join(format!("part{i}.o"));
                    std::fs::write(&path, &object.bytes).unwrap();
                    path
                })
                .collect::<Vec<_>>();
            parts.reuse = true;
            let warm = emit(&mut parts);
            assert_eq!(warm.parts.len(), expected_parts);
            assert!(warm.parts.iter().all(|p| p.reused));
            assert!(warm.objects.is_empty());

            let image = dir.path().join("image");
            let link = std::process::Command::new(&linker)
                .args(["--gc-sections", "-e", "hirsel_entry", "-o"])
                .arg(&image)
                .args(&paths)
                .output()
                .unwrap();
            assert!(
                link.status.success(),
                "{}",
                String::from_utf8_lossy(&link.stderr)
            );
            let bytes = std::fs::read(image).unwrap();
            let elf = object::File::parse(bytes.as_slice()).unwrap();
            let frame = elf.section_by_name(".eh_frame").unwrap();
            let bases = BaseAddresses::default().set_eh_frame(frame.address());
            let eh = EhFrame::new(frame.data().unwrap(), LittleEndian);
            let mut records = eh.entries(&bases);
            let mut ranges = Vec::new();
            while let Some(record) = records.next().unwrap() {
                if let CieOrFde::Fde(fde) = record {
                    let fde = fde.parse(EhFrame::cie_from_offset).unwrap();
                    assert_eq!(fde.cie().return_address_register().0, return_register);
                    ranges.push((fde.initial_address(), fde.len()));
                }
            }
            assert!(elf.symbol_by_name("hirsel_dead").is_none());
            assert_eq!(ranges.len(), 2, "GC kept an FDE for dead code");
            for name in ["hirsel_entry", "hirsel_live"] {
                let symbol = elf.symbol_by_name(name).unwrap();
                assert!(
                    ranges.contains(&(symbol.address(), symbol.size())),
                    "missing FDE for {target:?} {name}"
                );
            }
        }
    }
    // Mach-O: LLVM gives each function a compact unwind entry of its own
    // (§5.4), never a DWARF-mode one, in an image the driver links.
    let ir = "define i64 @hirsel_entry(ptr %p) {
                %v = call i64 @hirsel_live(ptr %p)
                ret i64 %v
            }
            define i64 @hirsel_live(ptr %p) noinline {
                %v = load volatile i64, ptr %p
                ret i64 %v
            }
            define i64 @hirsel_dead() { ret i64 7 }";
    let exports = [
        "hirsel_entry".into(),
        "hirsel_live".into(),
        "hirsel_dead".into(),
    ];
    let emitted = emit_program_parts(
        &[Unit::Text(ir)],
        &exports,
        Target::Aarch64Darwin,
        0,
        &mut Parts::default(),
        Lend::None,
    )
    .unwrap();
    let objects = emitted
        .objects
        .iter()
        .flat_map(|whole| {
            core::split_native_object(whole, &[("", ContentHash::of_bytes(b"whole"))], false, None)
                .unwrap()
        })
        .map(|object| object.bytes.into())
        .collect();
    let image = dir.path().join("darwin");
    link_macho(&linker, objects, "_hirsel_entry", &image, true);
    let bytes = std::fs::read(&image).unwrap();
    macho_structure(&bytes);
    let macho = object::File::parse(bytes.as_slice()).unwrap();
    assert!(macho.symbol_by_name("_hirsel_dead").is_none());
    let (entries, dump) = compact_unwind(linker.parent().unwrap(), &image);
    let text = macho
        .segments()
        .find(|segment| object::ObjectSegment::name(segment) == Ok(Some("__TEXT")))
        .map(|segment| object::ObjectSegment::address(&segment))
        .unwrap();
    for name in ["_hirsel_entry", "_hirsel_live"] {
        let offset = macho.symbol_by_name(name).unwrap().address() - text;
        // The entry covering a function is the last one at or before it:
        // ld64 folds a run of functions with one encoding into its first.
        let (_, encoding) = entries.range(..=offset).next_back().unwrap();
        // UNWIND_ARM64_MODE_FRAMELESS or _FRAME (compact_unwind_encoding.h).
        let mode = encoding & 0x0f00_0000;
        assert!(
            [0x0200_0000, 0x0400_0000].contains(&mode),
            "{name}:\n{dump}"
        );
    }
}

#[test]
fn llvm_split_parts_share_one_weak_placeholder() {
    use hirsel_core_ids::ContentHash;
    use hirsel_core_layout::Target;
    use hirsel_llvm_lazy::{Lend, Unheld, Unit, core, emit_program_parts};
    use std::path::PathBuf;
    use std::process::Command;

    // A placeholder another object may define over (as a `$revision`
    // carrier is), read by one part and addressed by two others. Exit code:
    // the value read, plus 10 when both addresses are one address.
    let ir = r#"
        @hirsel_slot = weak constant i64 0, align 8
        define i64 @hirsel_read() noinline nounwind {
            %v = load i64, ptr @hirsel_slot
            ret i64 %v
        }
        define ptr @hirsel_where_a() noinline nounwind { ret ptr @hirsel_slot }
        define ptr @hirsel_where_b() noinline nounwind { ret ptr @hirsel_slot }
        define void @hirsel_entry() noreturn nounwind {
            %v = call i64 @hirsel_read()
            %a = call ptr @hirsel_where_a()
            %b = call ptr @hirsel_where_b()
            %same = icmp eq ptr %a, %b
            %ten = select i1 %same, i64 10, i64 0
            %code = add i64 %v, %ten
            call void asm sideeffect "syscall", "{rax},{rdi},~{rcx},~{r11},~{memory}"(i64 60, i64 %code)
            unreachable
        }"#;
    let exports = [
        "hirsel_entry",
        "hirsel_read",
        "hirsel_where_a",
        "hirsel_where_b",
    ]
    .map(String::from);
    let bin = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin");
    let dir = tempfile::tempdir().unwrap();
    let assembly = dir.path().join("override.s");
    std::fs::write(
        &assembly,
        ".data\n.globl hirsel_slot\n.type hirsel_slot,@object\n.p2align 3\nhirsel_slot:\n.quad 5\n.size hirsel_slot, 8\n",
    )
    .unwrap();
    let defined = dir.path().join("override.o");
    let assembled = Command::new(bin.join("clang"))
        .args(["--target=x86_64-unknown-linux-gnu", "-c"])
        .arg(&assembly)
        .arg("-o")
        .arg(&defined)
        .status()
        .unwrap();
    assert!(assembled.success());
    let mut generated = Vec::new();
    for jobs in [0, 1, 2] {
        let program = emit_program_parts(
            &[Unit::Text(ir)],
            &exports,
            Target::X86_64Linux,
            jobs,
            &mut Unheld,
            Lend::None,
        )
        .unwrap();
        assert_eq!(
            program.parts.len(),
            4,
            "the placeholder kept the module whole"
        );
        let names = program
            .parts
            .iter()
            .map(|p| (p.name.as_str(), ContentHash::of_bytes(p.name.as_bytes())))
            .collect::<Vec<_>>();
        let objects = match program.objects.as_slice() {
            [whole] => core::split_native_object(whole, &names, false, None).unwrap(),
            objects => objects
                .iter()
                .zip(&names)
                .flat_map(|(bytes, name)| {
                    core::split_native_object(bytes, std::slice::from_ref(name), false, None)
                        .unwrap()
                })
                .collect(),
        };
        let paths = objects
            .iter()
            .enumerate()
            .map(|(i, object)| {
                let path = dir.path().join(format!("j{jobs}-part{i}.o"));
                std::fs::write(&path, &object.bytes).unwrap();
                path
            })
            .collect::<Vec<_>>();
        for (overridden, expected) in [(false, 10), (true, 15)] {
            let image = dir.path().join(format!("j{jobs}-{overridden}"));
            let link = Command::new(bin.join("ld.lld"))
                .args(["-e", "hirsel_entry", "-o"])
                .arg(&image)
                .args(&paths)
                .args(overridden.then_some(&defined))
                .output()
                .unwrap();
            assert!(
                link.status.success(),
                "{}",
                String::from_utf8_lossy(&link.stderr)
            );
            let status = Command::new(&image).status().unwrap();
            assert_eq!(
                status.code(),
                Some(expected),
                "jobs {jobs}, override {overridden}"
            );
        }
        if jobs > 0 {
            generated.push(objects.into_iter().map(|o| o.bytes).collect::<Vec<_>>());
        }
    }
    assert_eq!(
        generated[0], generated[1],
        "one and two jobs generate different parts"
    );
}

#[test]
fn cranelift_unwind_links_into_pie_from_whole_and_cached_objects() {
    use gimli::{BaseAddresses, EhFrame, LittleEndian, UnwindSection as _};
    use hirsel_core_ids::ContentHash;
    use hirsel_core_ir::text;
    use hirsel_core_layout::{ObjectFormat, Target};
    use object::{Object as _, ObjectSection as _, ObjectSegment as _, ObjectSymbol as _};
    use std::{path::PathBuf, process::Command};

    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::new(&dir.path().join("objects")).unwrap();
    let bin = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin");
    let header = "core v1\nsig g0 = () -> (u64)\ninst n0 = app@1/main/live [] []\ninst n1 = app@1/main/dead [] []\n";
    let live = "fn f0 = n0 g0 {\nb0():\n  v0: u64 = const 11\n  return v0\n}\n";
    let dead = "fn f1 = n1 g0 {\nb0():\n  v0: u64 = const 29\n  return v0\n}\n";
    let whole = text::parse(&format!("{header}{live}{dead}")).unwrap();
    for target in [
        Target::X86_64Linux,
        Target::Aarch64Linux,
        Target::Aarch64Darwin,
    ] {
        // The linked image names a C symbol with the target's prefix.
        let linked_name = |instance| {
            format!(
                "{}{}",
                target.symbol_prefix(),
                whole.identity(instance).symbol()
            )
        };
        let live_name = linked_name(whole.functions[0].instance);
        let dead_name = linked_name(whole.functions[1].instance);
        let options = hirsel_cranelift::core::Options {
            target: target.triple(),
            optimize: false,
            debug_lines: false,
        };
        let modes: &[bool] = match target {
            Target::X86_64Linux => &[false, true],
            Target::Aarch64Linux
            | Target::Aarch64Darwin
            | Target::Aarch64Ios
            | Target::Aarch64IosSim => &[false],
        };
        for &split in modes {
            let input = if split {
                vec![
                    text::parse(&format!("{header}{live}")).unwrap(),
                    text::parse(&format!("{header}{}", dead.replace("fn f1", "fn f0"))).unwrap(),
                ]
            } else {
                vec![whole.clone()]
            };
            let mut keys = Vec::new();
            let mut cold = Vec::new();
            for (index, module) in input.iter().enumerate() {
                let key = ContentHash::of_bytes(format!("{target:?}/{split}/{index}").as_bytes());
                let object = hirsel_cranelift::core::emit_object(module, &options, key).unwrap();
                crate::native_object::publish(&store, key, &object).unwrap();
                let path = dir.path().join(format!("unit{index}.o"));
                std::fs::write(&path, object.bytes).unwrap();
                keys.push(key);
                cold.push(path);
            }
            let warm = keys
                .into_iter()
                .map(|key| {
                    let (bare, _) = crate::native_object::load(&store, key).unwrap().unwrap();
                    crate::native_object::path(&store, bare.address)
                })
                .collect::<Vec<_>>();
            for paths in [cold, warm] {
                let executable = dir.path().join("image");
                match target.object_format() {
                    ObjectFormat::Elf => {
                        let linked = Command::new(bin.join("ld.lld"))
                            .args(["-pie", "--gc-sections", "--eh-frame-hdr"])
                            .args(["-e", &live_name, "-o"])
                            .arg(&executable)
                            .args(paths)
                            .output()
                            .unwrap();
                        assert!(
                            linked.status.success(),
                            "{}",
                            String::from_utf8_lossy(&linked.stderr)
                        );
                    }
                    // The driver's own Mach-O link of its BSD bundle.
                    ObjectFormat::MachO => link_macho(
                        &bin.join("ld.lld"),
                        paths
                            .iter()
                            .map(|path| std::fs::read(path).unwrap().into())
                            .collect(),
                        &live_name,
                        &executable,
                        true,
                    ),
                }
                let bytes = std::fs::read(&executable).unwrap();
                let image = object::File::parse(bytes.as_slice()).unwrap();
                assert_eq!(
                    image.kind(),
                    match target.object_format() {
                        // A PIE is ET_DYN; a Mach-O executable is MH_EXECUTE.
                        ObjectFormat::Elf => object::ObjectKind::Dynamic,
                        ObjectFormat::MachO => object::ObjectKind::Executable,
                    }
                );
                let frame = image
                    .section_by_name(match target.object_format() {
                        ObjectFormat::Elf => ".eh_frame",
                        ObjectFormat::MachO => "__eh_frame",
                    })
                    .unwrap();
                let base = BaseAddresses::default().set_eh_frame(frame.address());
                let eh = EhFrame::new(frame.data().unwrap(), LittleEndian);
                // ELF collects whole sections; Mach-O objects are split into
                // atoms at every symbol, so the unreferenced one goes either way.
                let stripped = match target.object_format() {
                    ObjectFormat::Elf => split,
                    ObjectFormat::MachO => true,
                };
                assert_eq!(image.symbol_by_name(&dead_name).is_none(), stripped);
                let names: &[&str] = if stripped {
                    &[&live_name]
                } else {
                    &[&live_name, &dead_name]
                };
                let text = image.section_by_name(match target.object_format() {
                    ObjectFormat::Elf => ".text",
                    ObjectFormat::MachO => "__text",
                });
                for name in names {
                    let symbol = image.symbol_by_name(name).unwrap();
                    // Mach-O records no symbol sizes; a function runs to the
                    // next symbol or the end of its section.
                    let size = match target.object_format() {
                        ObjectFormat::Elf => symbol.size(),
                        ObjectFormat::MachO => {
                            let text = text.as_ref().unwrap();
                            image
                                .symbols()
                                .map(|other| other.address())
                                .filter(|&address| address > symbol.address())
                                .min()
                                .unwrap_or(text.address() + text.size())
                                .min(text.address() + text.size())
                                - symbol.address()
                        }
                    };
                    let fde = eh
                        .fde_for_address(&base, symbol.address(), EhFrame::cie_from_offset)
                        .unwrap();
                    assert_eq!(
                        (fde.initial_address(), fde.len()),
                        (symbol.address(), size),
                        "{target:?} {name}"
                    );
                    assert_eq!(fde.cie().fde_address_encoding().unwrap().0, 0x1b);
                }
                match target.object_format() {
                    ObjectFormat::Elf => {}
                    ObjectFormat::MachO => macho_structure(&bytes),
                }
                match target.object_format() {
                    ObjectFormat::Elf => {}
                    // The §5.4 oracle: lld gives each function a DWARF-mode
                    // compact unwind entry (mode 3) whose low 24 bits are the
                    // offset of that function's FDE in __eh_frame.
                    ObjectFormat::MachO => {
                        let (entries, dump) = compact_unwind(&bin, &executable);
                        let text_base = image
                            .segments()
                            .find(|segment| segment.name() == Ok(Some("__TEXT")))
                            .unwrap()
                            .address();
                        for name in names {
                            let address = image.symbol_by_name(name).unwrap().address();
                            let encoding = entries[&(address - text_base)];
                            assert_eq!(encoding & 0x0f00_0000, 0x0300_0000, "{name}:\n{dump}");
                            let fde = eh
                                .fde_from_offset(
                                    &base,
                                    gimli::EhFrameOffset((encoding & 0x00ff_ffff) as usize),
                                    EhFrame::cie_from_offset,
                                )
                                .unwrap();
                            assert_eq!(fde.initial_address(), address, "{name}");
                        }
                    }
                }
            }
        }
    }
}

/// Each function's compact unwind encoding in the Mach-O image at `image`,
/// by its offset from `__TEXT`, as `llvm-objdump --unwind-info` prints the
/// second-level rows ("[k]: function offset=F, encoding[i]=E" or
/// "encoding=E"); and the dump.
fn compact_unwind(
    bin: &std::path::Path,
    image: &std::path::Path,
) -> (std::collections::BTreeMap<u64, u64>, String) {
    let dump = std::process::Command::new(bin.join("llvm-objdump"))
        .args(["--macho", "--unwind-info"])
        .arg(image)
        .output()
        .unwrap();
    let dump = String::from_utf8(dump.stdout).unwrap();
    let hex = |text: &str| u64::from_str_radix(text.trim_start_matches("0x"), 16).unwrap();
    let common = dump
        .lines()
        .filter_map(|line| line.trim().strip_prefix("encoding["))
        .map(|rest| hex(rest.split_once("]: ").unwrap().1))
        .collect::<Vec<_>>();
    let entries = dump
        .lines()
        .filter_map(|line| {
            let (_, row) = line.split_once("]: function offset=")?;
            let (function, encoding) = row.split_once(", encoding")?;
            let encoding = match encoding.strip_prefix('=') {
                Some(direct) => hex(direct),
                None => {
                    common
                        [hex(encoding.trim_start_matches('[').split_once(']').unwrap().0) as usize]
                }
            };
            Some((hex(function), encoding))
        })
        .collect();
    (entries, dump)
}

/// Link `objects` into `executable` with the driver's Mach-O link of its
/// bundle, with no libSystem, entering at `entry`; a dev link unless `optimize`.
fn link_macho(
    linker: &std::path::Path,
    objects: Vec<std::sync::Arc<[u8]>>,
    entry: &str,
    executable: &std::path::Path,
    optimize: bool,
) {
    use hirsel_core_layout::{ObjectFormat, Target};
    let held = crate::native::link_input::Bundle::of(&objects, ObjectFormat::MachO).unwrap();
    let bundle = held.path().unwrap();
    let recipe = crate::tool_inputs::LinkRecipe {
        schema: 0,
        target: Target::Aarch64Darwin.triple().into(),
        linker: linker.to_owned(),
        args: ["-flavor", "darwin", "-arch", "arm64", "-platform_version"]
            .into_iter()
            .chain(["macos", "26.0", "26.0", "-adhoc_codesign", "-e"])
            .chain([entry, crate::tool_inputs::INPUT_MARKER])
            .map(str::to_owned)
            .collect(),
        files: Vec::new(),
        source_files: Vec::new(),
        absent: Vec::new(),
        identity: hirsel_core_ids::ContentHash::of_bytes(b"recipe"),
    };
    crate::native::direct_link(
        &recipe,
        std::slice::from_ref(&bundle),
        Some(&bundle),
        executable,
        optimize,
        false,
        false,
    )
    .unwrap();
}

/// The linked image and its dSYM retain the same arm64 CPU and UUID.
fn macho_structure(bytes: &[u8]) {
    use object::{Object as _, ObjectSection as _};
    let uuid = super::macho::executable_uuid(bytes).unwrap();
    let dsym = super::dsym::file(bytes, &elf_debug_object()).unwrap();
    let dword = |at: usize| u32::from_le_bytes(dsym[at..at + 4].try_into().unwrap());
    // MH_DSYM of the image's CPU.
    assert_eq!(
        [dword(0), dword(4), dword(12)],
        [0xfeed_facf, 0x0100_000c, 0xa]
    );
    let parsed = object::File::parse(dsym.as_slice()).unwrap();
    assert_eq!(parsed.mach_uuid().unwrap(), Some(uuid));
    let line = parsed.section_by_name("__debug_line").unwrap();
    assert_eq!(line.data().unwrap(), b"line rows");
}

/// A detached debug object with one `.debug_line` section.
fn elf_debug_object() -> Vec<u8> {
    let mut object = object::write::Object::new(
        object::BinaryFormat::Elf,
        object::Architecture::Aarch64,
        object::Endianness::Little,
    );
    let section = object.add_section(
        Vec::new(),
        b".debug_line".to_vec(),
        object::SectionKind::Debug,
    );
    object.append_section_data(section, b"line rows", 1);
    object.write().unwrap()
}
