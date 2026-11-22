#[test]
fn link_rebinds_rows_and_inline_calls_without_changing_native_code() {
    super::side::tests::binding_case();
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
