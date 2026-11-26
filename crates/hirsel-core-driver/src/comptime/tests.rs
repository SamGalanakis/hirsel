//! LANG-20 and PROG-10 laws the corpus cannot state: a library image carries its
//! computed values, and assertions are numbered across a module's files.
use super::*;
use crate::tests::{library_request, request};
use crate::{CoreImage, Driver, LibraryInput, Meter, PackageManifest, package};

fn path(full: &str) -> DefPath {
    DefPath::from_full(full).unwrap()
}

/// A program reading a library's computed constant through its image takes the
/// image's value and runs no initialiser; the library's passing assertion does
/// not refuse the import. 12 * 12 = 144, by hand.
#[test]
fn image_values_answer_library_constants_without_a_run() {
    let lib = tempfile::tempdir().unwrap();
    let source = "fn square(n: u64) -> u64 { n * n }\npub const AREA: u64 = square(12)\nfn holds() {}\nconst _: () = holds()\n";
    let (_, image) = Driver::default()
        .produce_image(library_request(source, lib.path()), &mut Meter::new(false))
        .unwrap();
    assert!(image.is_some());
    let image = CoreImage::open(&lib.path().join("store")).unwrap().unwrap();
    std::fs::remove_file(lib.path().join("main.hirsel")).unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request("fn area() -> u64 { lib.AREA }\n", app.path());
    req.library = LibraryInput::Image(image);
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    for target in Target::ALL {
        let value = product
            .computed(&path("core@1/lib/AREA"), target)
            .value
            .unwrap();
        assert_eq!(value.value, StaticValue::Bits(144));
    }
    assert!(
        product.constant_values.published().is_empty(),
        "the app ran an initialiser"
    );
}

/// The k-th assertion of a module, its files in index order and each file's items
/// in order, is `_$k` (PROG-10): the failing one here is the third, `_$2`.
#[test]
fn assertions_are_numbered_across_a_module_files_in_index_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = request("fn holds() {}\nconst _: () = holds()\n", dir.path());
    let second = dir.path().join("second.hirsel");
    std::fs::write(
        &second,
        "fn zero() -> u8 { 0 }\nfn bad() { let _ = 1u8 / zero() }\nconst _: () = holds()\nconst _: () = bad()\n",
    )
    .unwrap();
    let mut source = req.inputs.packages[0].sources[0].clone();
    source.file = second;
    req.inputs.packages[0].sources.push(source);
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    let messages: Vec<&str> = product
        .diagnostics
        .iter()
        .filter(|d| d.rule == "LANG-20")
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "constant app@1/main/_$2 is refused: it fails with Division, in app@1/main/bad from app@1/main/_$2"
        ]
    );
}

/// A closed-list function without a body, called through a function value, still
/// stops the run that reaches it (LANG-20): its value adapter carries the mark.
#[test]
fn closed_kernel_called_through_a_value_refuses_the_constant() {
    let dir = tempfile::tempdir().unwrap();
    let source =
        "fn via() -> u64 { let f = unique.new\n let _ = f()\n 1 }\nconst VIA: u64 = via()\n";
    let mut req = request(source, dir.path());
    let core = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/core");
    req.inputs.packages[0].dependencies.push(package("core"));
    req.inputs
        .packages
        .push(PackageManifest::directory(&core, package("core"), true).unwrap());
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    let messages: Vec<&str> = product
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert!(
        messages.len() == 1
            && messages[0].starts_with(
                "constant app@1/main/VIA is refused: it reaches unique.new, on the closed list"
            ),
        "{messages:?}"
    );
}

/// LANG-20: a computed String or FrozenBytes reads as its literal on both native engines.
#[test]
#[ignore = "explicit native integration target"]
fn computed_static_bytes_on_both_native_engines() {
    let case = include_str!("../../../../core/front/cases/body/constant-computed-static.hirsel");
    for engine in [crate::Backend::Cranelift, crate::Backend::Llvm] {
        crate::native::tests::run_lines_native(engine, case);
    }
}
