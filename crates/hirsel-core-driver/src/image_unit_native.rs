// The image unit. Law: a link reads the image objects it reaches as one unit,
// `ld -r` of them in link order, so a primed store and a fresh one link the
// same executable through the same unit (the reach laws, whose cores are
// images). The unit is not the separate objects' link: `ld -r` renumbers local
// and section symbols. What a program does and reports is the same. Oracle:
// the same build with the image objects linked one by one.

/// Three programs over std, each failing in or through std code or printing
/// through it: (exit, stdout, stderr).
const UNIT_PROGRAMS: [(&str, &str); 3] = [
    ("bounds", "pub fn main() -> Result[(), Exit] {\n    let mut xs: List[u64] = list.new()\n    let _ = list.remove(mut xs, 3)\n    Ok(())\n}\n"),
    ("adapter", "fn apply(f: fn(take Option[u64]) -> u64, x: Option[u64]) -> u64 {\n    f(x)\n}\npub fn main() -> Result[(), Exit] {\n    let none: Option[u64] = None\n    if string.starts_with(\"ab\", \"a\") && apply(option.unwrap, none) == 0u64 { Ok(()) } else { Error(Exit(status: 7)) }\n}\n"),
    ("strings", "pub fn main(out: Stdout) -> Result[(), Exit] {\n    let s = \"left right\"\n    let Some((a, b)) = string.split_once(s, \" \") else {\n        return Error(Exit(status: 2u8))\n    }\n    if string.starts_with(s, a) && string.ends_with(s, b) && string.find(s, 0, \"right\") == Some(5) {\n        result.unwrap(stdout.write(out, b));\n        Ok(())\n    } else {\n        Error(Exit(status: 1u8))\n    }\n}\n"),
];

/// Build `source` as project `project` over std imaged into `cache`, with
/// debug lines; the executable, its run, and the unit the link read.
fn unit_build(
    dir: &Path,
    backend: Backend,
    project: &str,
    source: &str,
    cache: &str,
    separate: bool,
) -> (Vec<u8>, std::process::Output, Option<(ContentHash, PathBuf, bool)>) {
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let project = dir.join(project);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(dir.join(cache)).unwrap();
    std::fs::write(project.join("main.hirsel"), source).unwrap();
    let request = CheckRequest::program(&project, &libraries, &project.join("store")).unwrap();
    let product = Driver::default()
        .check_reachable_with_product(request, &dir.join(cache), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let mut options = native_options(backend, &project);
    options.debug_lines = true;
    options.grant_mask = 1 << 4;
    UNIT.set(None);
    SEPARATE_IMAGE.set(separate);
    let built = product.build(&entry, &options);
    SEPARATE_IMAGE.set(false);
    let built = built.unwrap();
    assert!(built.linked, "{project:?}: linked");
    let output = Command::new(&built.executable).output().unwrap();
    (std::fs::read(&built.executable).unwrap(), output, UNIT.take())
}

/// The defined functions of an executable, by name and size: the symbols a
/// failure report or a debugger reads.
fn unit_functions(bytes: &[u8]) -> BTreeSet<(String, u64)> {
    use object::{Object as _, ObjectSymbol as _};
    let file = object::File::parse(bytes).unwrap();
    file.symbols()
        .filter(|s| s.kind() == object::SymbolKind::Text && s.is_definition())
        .map(|s| (s.name().unwrap().to_owned(), s.size()))
        .collect()
}

/// Each program linked through the unit runs, prints and reports as linked
/// from its separate objects, with the same functions and debug line rows.
/// Mutant: a unit missing one of its objects, red (the link refuses it). A
/// unit prelinked with `--discard-locals` or `--strip-all` stays green: the
/// functions are global symbols, which a relocatable output keeps.
fn image_unit_links_what_its_objects_link(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    for (name, source) in UNIT_PROGRAMS {
        let (unit, ran, read) = unit_build(dir.path(), backend, &format!("{name}-unit"), source, "unit-cache", false);
        let (separate, expected, none) =
            unit_build(dir.path(), backend, &format!("{name}-separate"), source, "separate-cache", true);
        assert!(read.is_some_and(|(_, _, prelinked)| prelinked), "{name}: the link prelinked its unit");
        assert!(none.is_none(), "{name}: the separate link read no unit");
        assert_eq!(
            (ran.status.code(), &ran.stdout, &ran.stderr),
            (expected.status.code(), &expected.stdout, &expected.stderr),
            "{name}: what the program does and reports"
        );
        assert_eq!(unit_functions(&unit), unit_functions(&separate), "{name}: functions");
        assert_eq!(
            crate::native_debug::source_positions(&unit).unwrap(),
            crate::native_debug::source_positions(&separate).unwrap(),
            "{name}: debug line rows"
        );
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn image_unit_links_what_its_objects_link_cranelift() { image_unit_links_what_its_objects_link(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn image_unit_links_what_its_objects_link_llvm() { image_unit_links_what_its_objects_link(Backend::Llvm); }

/// A unit whose bytes fail their check, or a valid unit's frame under another
/// unit's key, is a miss: the link prelinks the unit again and links the
/// executable it linked first. Mutant: a unit read without its check, red on
/// the damaged unit.
#[test]
#[ignore = "explicit native integration target"]
fn a_damaged_or_misplaced_image_unit_is_prelinked_again() {
    let dir = tempfile::tempdir().unwrap();
    let build = |project: &str, source: &str| {
        unit_build(dir.path(), Backend::Cranelift, project, source, "cache", false)
    };
    let (bounds, strings) = (UNIT_PROGRAMS[0].1, UNIT_PROGRAMS[2].1);
    let (first, _, unit) = build("bounds", bounds);
    let (key, path, _) = unit.unwrap();
    let (other, _, other_unit) = build("strings", strings);
    let (other_key, _, _) = other_unit.unwrap();
    assert_ne!(key, other_key, "two image object sets, two units");

    // One flipped byte inside the unit's text.
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0x50] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();
    let (again, ran, unit) = build("bounds-damaged", bounds);
    assert!(unit.is_some_and(|(k, _, prelinked)| k == key && prelinked), "a damaged unit is prelinked again");
    assert!(again == first, "the damaged unit's program links its first executable");
    assert_eq!(ran.status.code(), Some(101), "{ran:?}");

    // The first unit's frame placed under the second's key.
    let store = Store::new(path.parent().unwrap()).unwrap();
    std::fs::write(store.path(other_key), std::fs::read(store.path(key)).unwrap()).unwrap();
    let (again, ran, unit) = build("strings-misplaced", strings);
    assert!(unit.is_some_and(|(k, _, prelinked)| k == other_key && prelinked), "a misplaced unit is a miss");
    assert!(again == other, "the misplaced unit's program links its first executable");
    assert_eq!((ran.status.code(), ran.stdout.as_slice()), (Some(0), &b"right"[..]), "{ran:?}");
}
