use super::{bin, link_macho, macho_structure};
use hirsel_core_ids::ContentHash;
use hirsel_core_layout::Target;
use hirsel_llvm_lazy::{Lend, Unheld, Unit, core::split_native_object, emit_program_parts};

#[test]
fn macho_release_links_a_whole_image_then_three_split_images() {
    // Writable data keeps the first image whole. In LLVM 23 its Apple tuning
    // used to enable compare/select alignment for every subsequent image in
    // this process, including parts whose per-function features disable it.
    let whole = r"
        @counter = global i32 7
        define i32 @hirsel_entry() {
            %n = load volatile i32, ptr @counter
            ret i32 %n
        }
    ";
    let dir = tempfile::tempdir().unwrap();
    let exports = ["hirsel_entry".to_owned()];
    for image in 0..4 {
        let split = format!(
            r"
            define i32 @hirsel_entry(i32 %a, i32 %b) noinline {{
                %cmp = icmp slt i32 %a, {image}
                %value = select i1 %cmp, i32 %a, i32 %b
                ret i32 %value
            }}
            "
        );
        let ir = if image == 0 { whole } else { &split };
        let program = emit_program_parts(
            &[Unit::Text(ir)],
            &exports,
            Target::Aarch64Darwin,
            1,
            &mut Unheld,
            Lend::None,
        )
        .unwrap();
        let [part] = program.parts.as_slice() else {
            panic!("image {image} must have one part");
        };
        let [object] = program.objects.as_slice() else {
            panic!("image {image} must have one object");
        };
        assert_eq!(part.name, if image == 0 { "" } else { "hirsel_entry" });
        let parts = [(part.name.as_str(), ContentHash::of_bytes(ir.as_bytes()))];
        let objects = split_native_object(object, &parts, false, None).unwrap();
        let output = dir.path().join(image.to_string());
        link_macho(
            &bin().join("ld.lld"),
            objects.into_iter().map(|o| o.bytes.into()).collect(),
            "_hirsel_entry",
            &output,
            true,
        );
        macho_structure(&std::fs::read(output).unwrap());
    }
}
