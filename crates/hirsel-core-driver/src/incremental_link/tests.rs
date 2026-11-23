use super::{Incremental, Slot};
use hirsel_core_ids::ContentHash;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin")
}

fn assemble(dir: &Path, name: &str, source: &str) -> Vec<u8> {
    let (input, output) = (dir.join(format!("{name}.s")), dir.join(format!("{name}.o")));
    std::fs::write(&input, source).unwrap();
    let status = Command::new(bin().join("clang"))
        .args(["--target=x86_64-unknown-linux-gnu", "-c"])
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(output).unwrap()
}

/// The oracle: lld's own link of the objects.
fn lld(dir: &Path, objects: &[&[u8]]) -> Vec<u8> {
    let paths: Vec<_> = objects
        .iter()
        .enumerate()
        .map(|(n, bytes)| {
            let path = dir.join(format!("in{n}.o"));
            std::fs::write(&path, bytes).unwrap();
            path
        })
        .collect();
    let image = dir.join("image");
    let status = Command::new(bin().join("ld.lld"))
        .args(["-pie", "--no-dynamic-linker", "-e", "start", "-o"])
        .arg(&image)
        .args(paths)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(image).unwrap()
}

const MAIN: &str = r#"
.text
.globl start
start:
    movq shared@GOTPCREL(%rip), %rax
    call value
    movl $60, %edi
    syscall
.data
.globl shared
shared:
    .quad 7
"#;

/// A unit as the compiler emits one: code that references another object
/// through a relaxable GOT load, read-only data named by a local symbol, a
/// hidden debug symbol named by the unit's key, and a note outside memory.
fn unit(constant: &str, data: u8, debug: char, note: u8) -> String {
    let key = debug.to_string().repeat(64);
    format!(
        r#"
.text
.globl value
.globl __hirsel_debug_{key}_0
.hidden __hirsel_debug_{key}_0
value:
__hirsel_debug_{key}_0:
    movq shared@GOTPCREL(%rip), %rax
    {constant}
    leaq __hirsel_data_7(%rip), %rcx
    ret
.section .rodata.subsection,"a",@progbits
__hirsel_data_7:
    .byte {data}, 1, 2, 3
.section .note.hirsel.source,"",@note
    .long 4, 4, 2
    .asciz "HSL"
    .long {note}
"#
    )
}

/// Law: a resident compiler's patched executable is lld's link of the same
/// objects byte for byte, and an edit the patch cannot place is linked by
/// lld again, after which edits patch over that link.
#[test]
fn a_patched_link_is_the_lld_link_of_the_edited_objects() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    let frame = ContentHash::of_bytes(b"frame");
    let main = assemble(dir, "main", MAIN);
    let mut linker = Incremental::default();
    let mut step = |name: &str, unit_source: String, patches: bool| {
        let unit = assemble(dir, name, &unit_source);
        let file = std::fs::File::open(dir.join(format!("{name}.o"))).unwrap();
        let objects = [
            Slot::Bytes(&main),
            Slot::Keyed((ContentHash::of_bytes(&unit), 0), &file),
        ];
        let want = lld(dir, &[&main, &unit]);
        match linker.patch(frame, &objects).unwrap() {
            Some(image) => {
                assert!(patches, "{name}: patched an edit lld must link");
                assert!(image == want, "{name}: the patched image is not lld's");
            }
            None => {
                assert!(
                    !patches,
                    "{name}: an edit the patch places fell back to lld"
                );
                linker.remember(frame, &objects, &want).unwrap();
            }
        }
    };
    step("cold", unit("movl $1, %eax", 5, 'a', 9), false);
    // The constant, the data byte, the debug symbol's name and the note.
    step("constant", unit("movl $2, %eax", 6, 'b', 10), true);
    step("constant-again", unit("movl $3, %eax", 6, 'c', 10), true);
    // A longer instruction moves everything after it.
    step("longer", unit("movabsq $3, %rax", 6, 'c', 10), false);
    step("after-fallback", unit("movabsq $4, %rax", 7, 'd', 11), true);
}
