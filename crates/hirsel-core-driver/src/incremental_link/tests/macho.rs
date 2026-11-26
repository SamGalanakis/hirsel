//! The arm64 Mach-O resident link against lld's own link of the same
//! objects. On macOS each patched image is also checked with `codesign -v`
//! and run.
use super::super::{Incremental, Slot};
use super::bin;
use hirsel_core_ids::ContentHash;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

const TARGET: &str = "aarch64-apple-darwin";

fn assemble(dir: &Path, name: &str, source: &str) -> Vec<u8> {
    let (input, output) = (dir.join(format!("{name}.s")), dir.join(format!("{name}.o")));
    std::fs::write(&input, source).unwrap();
    let status = Command::new(bin().join("clang"))
        .args(["--target=arm64-apple-macos26.0", "-c"])
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(output).unwrap()
}

/// The oracle: lld's own link of the objects, through the driver's Mach-O
/// link (its flags and its output name, which enters the UUID).
fn lld(dir: &Path, objects: &[&[u8]]) -> Vec<u8> {
    let mut inputs: Vec<PathBuf> = objects
        .iter()
        .enumerate()
        .map(|(n, bytes)| {
            let path = dir.join(format!("in{n}.o"));
            std::fs::write(&path, bytes).unwrap();
            path
        })
        .collect();
    inputs.push(
        PathBuf::from(std::env::var_os("HIRSEL_LINK_KIT").unwrap())
            .join(TARGET)
            .join("libSystem.tbd"),
    );
    let recipe = crate::tool_inputs::LinkRecipe {
        schema: 0,
        target: TARGET.into(),
        linker: bin().join("ld.lld"),
        args: ["-flavor", "darwin", "-arch", "arm64", "-platform_version"]
            .into_iter()
            .chain(["macos", "26.0", "26.0", "-adhoc_codesign", "-e", "_main"])
            .chain([crate::tool_inputs::INPUT_MARKER])
            .map(str::to_owned)
            .collect(),
        files: Vec::new(),
        source_files: Vec::new(),
        absent: Vec::new(),
        identity: ContentHash::of_bytes(b"recipe"),
    };
    let image = dir.join("image");
    crate::native::direct_link(&recipe, &inputs, None, &image, false, false, false).unwrap();
    std::fs::read(image).unwrap()
}

/// On macOS: the image's signature verifies and it exits with `status`.
fn runs(dir: &Path, name: &str, image: &[u8], status: i32) {
    if !crate::host::RUNS_DARWIN_IMAGES {
        return;
    }
    let path = dir.join(format!("{name}.run"));
    crate::executable::deliver(&path, image, None).unwrap();
    let verify = Command::new("codesign")
        .arg("-v")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "{name}: codesign -v: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
    let ran = Command::new(&path).status().unwrap();
    assert_eq!(ran.code(), Some(status), "{name}: exit status");
}

const MAIN: &str = r#"
.section __TEXT,__text,regular,pure_instructions
.build_version macos, 26, 0
.globl _main
.p2align 2
_main:
    stp x29, x30, [sp, #-16]!
    bl _value
    adrp x8, _shared@GOTPAGE
    ldr x8, [x8, _shared@GOTPAGEOFF]
    ldp x29, x30, [sp], #16
    ret
.section __DATA,__data
.globl _shared
.p2align 3
_shared:
    .quad 7
    // Past lld's 1 MiB UUID chunk, so the UUID and the page hashes are
    // derived again across chunks the edit never touches.
    .space 2621440
.subsections_via_symbols
"#;

/// A unit as the compiler emits one: code that loads another object's
/// symbol through the GOT, read-only data named by a local symbol that
/// carries the unit's key, and a private-extern debug symbol of that key.
fn unit(constant: &str, data: u8, key: char) -> String {
    let key = key.to_string().repeat(64);
    format!(
        r#"
.section __TEXT,__text,regular,pure_instructions
.build_version macos, 26, 0
.globl _value
.globl ___hirsel_debug_{key}_0
.private_extern ___hirsel_debug_{key}_0
.p2align 2
_value:
___hirsel_debug_{key}_0:
    adrp x9, _shared@GOTPAGE
    ldr x9, [x9, _shared@GOTPAGEOFF]
    {constant}
    adrp x10, _hirsel_data_{key}@PAGE
    add x10, x10, _hirsel_data_{key}@PAGEOFF
    ret
.section __TEXT,__const
_hirsel_data_{key}:
    .byte {data}, 1, 2, 3
.subsections_via_symbols
"#
    )
}

/// Law: a resident compiler's patched arm64 executable is lld's link of the
/// same objects byte for byte, its UUID and its ad-hoc signature included;
/// an edit the patch cannot place is linked by lld again, after which edits
/// patch over that link.
#[test]
fn a_patched_macho_link_is_the_lld_link_of_the_edited_objects() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    let frame = ContentHash::of_bytes(b"frame");
    let main = assemble(dir, "main", MAIN);
    let mut linker = Incremental::default();
    let mut patched = 0;
    let mut step = |name: &str, unit_source: String, status: i32, patches: bool| {
        let unit = assemble(dir, name, &unit_source);
        let member: Arc<[u8]> = unit.as_slice().into();
        let objects = [
            Slot::Bytes(&main),
            Slot::Keyed((ContentHash::of_bytes(&unit), 0), &member),
        ];
        let want = lld(dir, &[&main, &unit]);
        let image = match linker.patch(frame, &objects).unwrap() {
            Some(image) => {
                assert!(patches, "{name}: patched an edit lld must link");
                assert!(*image == want, "{name}: the patched image is not lld's");
                patched += 1;
                image
            }
            None => {
                assert!(
                    !patches,
                    "{name}: an edit the patch places fell back to lld"
                );
                let image = Arc::new(want);
                linker.remember(frame, &objects, &image).unwrap();
                image
            }
        };
        runs(dir, name, &image, status);
    };
    step("cold", unit("mov w0, #1", 5, 'a'), 1, false);
    // The constant, the data byte and both keyed names.
    step("constant", unit("mov w0, #2", 6, 'b'), 2, true);
    step("constant-again", unit("mov w0, #3", 6, 'c'), 3, true);
    // A longer body moves everything after it.
    step(
        "longer",
        unit("mov w0, #3\n    add w0, w0, #1", 6, 'c'),
        4,
        false,
    );
    step(
        "after-fallback",
        unit("mov w0, #4\n    add w0, w0, #1", 7, 'd'),
        5,
        true,
    );
    assert_eq!(patched, 3, "the patch path was not taken");
}

/// A member defining `_shared_fn` weak, returning `n`.
fn weak(n: u8) -> String {
    format!(
        r#"
.section __TEXT,__text,regular,pure_instructions
.build_version macos, 26, 0
.globl _shared_fn
.weak_definition _shared_fn
.p2align 2
_shared_fn:
    mov w0, #{n}
    ret
.subsections_via_symbols
"#
    )
}

/// Law: lld keeps the first of two weak copies, so an edit to the second
/// copy leaves the image as it was; the patch never writes the edit over
/// the copy lld kept.
#[test]
fn an_edited_weak_copy_is_not_patched_over_the_copy_lld_kept() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    let frame = ContentHash::of_bytes(b"frame");
    let main = assemble(
        dir,
        "main",
        ".section __TEXT,__text,regular,pure_instructions\n.build_version macos, 26, 0\n.globl _main\n.p2align 2\n_main:\n    b _shared_fn\n.subsections_via_symbols\n",
    );
    let first = assemble(dir, "first", &weak(1));
    let mut linker = Incremental::default();
    let mut link = |n: u8| {
        let second = assemble(dir, &format!("second{n}"), &weak(n));
        let members: [Arc<[u8]>; 2] = [first.as_slice().into(), second.as_slice().into()];
        let objects = [
            Slot::Bytes(&main),
            Slot::Keyed((ContentHash::of_bytes(&first), 0), &members[0]),
            Slot::Keyed((ContentHash::of_bytes(&second), 0), &members[1]),
        ];
        let want = lld(dir, &[&main, &first, &second]);
        let patched = linker.patch(frame, &objects).unwrap();
        if patched.is_none() {
            linker
                .remember(frame, &objects, &Arc::new(want.clone()))
                .unwrap();
        }
        (patched, want)
    };
    assert!(link(1).0.is_none());
    let (patched, want) = link(2);
    if let Some(patched) = patched {
        assert!(*patched == want, "the patch wrote over the kept weak copy");
    }
    runs(dir, "weak", &want, 1);
}

/// A Cranelift arm64 Darwin unit with one static, `d0`, holding `byte`,
/// which its function names.
fn cranelift_unit(name: &str, byte: u8) -> Vec<u8> {
    let text = format!(
        "core v1\nstatic d0 = align 1 hex {byte:02x}\nsig g0 = () -> (u64)\ninst n0 = app@1/main/{name} [] []\nfn f0 = n0 g0 {{\nb0():\n  v0: ptr = static d0\n  v1: u64 = const 0\n  return v1\n}}\n"
    );
    let module = hirsel_core_ir::text::parse(&text).unwrap();
    let options = hirsel_cranelift::core::Options {
        target: TARGET,
        optimize: false,
        debug_lines: false,
    };
    hirsel_cranelift::core::emit_object(&module, &options, ContentHash::of_bytes(text.as_bytes()))
        .unwrap()
        .bytes
}

/// Law: as for ELF, two Cranelift members whose statics share an index
/// each place their own, so an edit to one member's static patches the
/// arm64 image to lld's link of the edited objects.
#[test]
fn an_edited_static_patches_a_macho_image() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    let frame = ContentHash::of_bytes(b"frame");
    let start = assemble(
        dir,
        "start",
        ".section __TEXT,__text,regular,pure_instructions\n.build_version macos, 26, 0\n.globl _main\n.p2align 2\n_main:\n    mov w0, #0\n    ret\n.subsections_via_symbols\n",
    );
    let left = cranelift_unit("left", 5);
    let mut linker = Incremental::default();
    let mut link = |right: &[u8]| {
        let members: [Arc<[u8]>; 2] = [left.as_slice().into(), right.into()];
        let objects = [
            Slot::Bytes(&start),
            Slot::Keyed((ContentHash::of_bytes(&left), 0), &members[0]),
            Slot::Keyed((ContentHash::of_bytes(right), 0), &members[1]),
        ];
        let want = Arc::new(lld(dir, &[&start, &left, right]));
        let patched = linker.patch(frame, &objects).unwrap();
        if patched.is_none() {
            linker.remember(frame, &objects, &want).unwrap();
        }
        (patched, want)
    };
    assert!(link(&cranelift_unit("right", 5)).0.is_none());
    let (patched, want) = link(&cranelift_unit("right", 6));
    let patched = patched.expect("the edited static fell back to lld");
    assert!(patched == want, "the patched image is not lld's");
    runs(dir, "static", &patched, 0);
}
