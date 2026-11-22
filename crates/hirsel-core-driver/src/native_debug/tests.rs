#[test]
fn link_rebinds_rows_and_inline_calls_without_changing_native_code() {
    super::side::tests::binding_case();
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
        let debug = bind_detached([&debug], &map, &[], &bytes).unwrap();
        let binary = NativeImage { bytes, debug };
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
                [whole] => core::split_native_object(whole, &names, false).unwrap(),
                objects => objects
                    .iter()
                    .zip(&names)
                    .flat_map(|(bytes, name)| {
                        core::split_native_object(bytes, std::slice::from_ref(name), false).unwrap()
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
}

#[test]
fn cranelift_unwind_links_into_pie_from_whole_and_cached_objects() {
    use gimli::{BaseAddresses, EhFrame, LittleEndian, UnwindSection as _};
    use hirsel_core_ids::ContentHash;
    use hirsel_core_ir::text;
    use hirsel_core_layout::Target;
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
    use std::{path::PathBuf, process::Command};

    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::new(&dir.path().join("objects")).unwrap();
    let bin = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin");
    let header = "core v1\nsig g0 = () -> (u64)\ninst n0 = app@1/main/live [] []\ninst n1 = app@1/main/dead [] []\n";
    let live = "fn f0 = n0 g0 {\nb0():\n  v0: u64 = const 11\n  return v0\n}\n";
    let dead = "fn f1 = n1 g0 {\nb0():\n  v0: u64 = const 29\n  return v0\n}\n";
    let whole = text::parse(&format!("{header}{live}{dead}")).unwrap();
    let live_name = whole.identity(whole.functions[0].instance).symbol();
    let dead_name = whole.identity(whole.functions[1].instance).symbol();
    for target in [Target::X86_64Linux, Target::Aarch64Linux] {
        let options = hirsel_cranelift::core::Options {
            target: target.triple(),
            optimize: false,
            debug_lines: false,
        };
        let modes: &[bool] = if target == Target::X86_64Linux {
            &[false, true]
        } else {
            &[false]
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
                    let (bare, _, _) = crate::native_object::load(&store, key).unwrap().unwrap();
                    crate::native_object::path(&store, bare.address)
                })
                .collect::<Vec<_>>();
            for paths in [cold, warm] {
                let executable = dir.path().join("image");
                let linked = Command::new(bin.join("ld.lld"))
                    .args([
                        "-pie",
                        "--gc-sections",
                        "--eh-frame-hdr",
                        "-e",
                        &live_name,
                        "-o",
                    ])
                    .arg(&executable)
                    .args(paths)
                    .output()
                    .unwrap();
                assert!(
                    linked.status.success(),
                    "{}",
                    String::from_utf8_lossy(&linked.stderr)
                );
                let bytes = std::fs::read(executable).unwrap();
                let elf = object::File::parse(bytes.as_slice()).unwrap();
                assert_eq!(elf.kind(), object::ObjectKind::Dynamic);
                let frame = elf.section_by_name(".eh_frame").unwrap();
                let base = BaseAddresses::default().set_eh_frame(frame.address());
                let eh = EhFrame::new(frame.data().unwrap(), LittleEndian);
                assert_eq!(elf.symbol_by_name(&dead_name).is_none(), split);
                let names: &[&str] = if split {
                    &[&live_name]
                } else {
                    &[&live_name, &dead_name]
                };
                for name in names {
                    let symbol = elf.symbol_by_name(name).unwrap();
                    let fde = eh
                        .fde_for_address(&base, symbol.address(), EhFrame::cie_from_offset)
                        .unwrap();
                    assert_eq!(
                        (fde.initial_address(), fde.len()),
                        (symbol.address(), symbol.size())
                    );
                    assert_eq!(fde.cie().fde_address_encoding().unwrap().0, 0x1b);
                }
            }
        }
    }
}
