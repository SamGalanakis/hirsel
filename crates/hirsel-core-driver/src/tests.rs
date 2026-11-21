use super::*;
use hirsel_core_ir::{FuncId, eval};

#[path = "diagnostic_hints.rs"]
mod diagnostic_hints;

pub(crate) fn request(source: &str, dir: &Path) -> CheckRequest {
    let source_file = dir.join("main.hirsel");
    std::fs::write(&source_file, source).unwrap();
    CheckRequest {
        manifest: None,
        inputs: CorpusManifest {
            core: package("core"),
            packages: vec![PackageManifest {
                package: package("app"),
                sources: vec![hirsel_core_resolve::ModuleSource {
                    file: source_file,
                    modules: vec!["main".into()],
                    signatures: false,
                }],
                dependencies: vec![],
            }],
        },
        store: dir.join("store"),
        library: LibraryInput::Sources,
        entry: None,
    }
}

#[test]
fn private_runner_launch_hook_is_lowered_and_evaluated() {
    let directory = tempfile::tempdir().unwrap();
    let runner = include_str!("../../../core/programs/store/runner.hirsel");
    let source = format!(
        "{runner}\npub fn probe(a: Args) -> u64 {{\n let d = result.unwrap(launch_digest())\n let t = result.unwrap(launch_target(a))\n launch_word(0u64, 0u64) + list.len(d) + string.byte_len(t)\n}}\n"
    );
    let path = directory.path().join("runner.hirsel");
    std::fs::write(&path, source).unwrap();
    let product = runner_launch_fixture(directory.path(), Some(&path));
    let identity =
        hirsel_core_ir::Identity::plain(DefPath::from_full("store@1/runner/probe").unwrap());
    let units = product.units([identity.clone()], 65_536).unwrap();
    let modules = units.modules().unwrap();
    let (module, entry) = hirsel_core_materialize::join(&modules, &identity).unwrap();
    let launch = hirsel_core_ids::launch::Metadata::embedded(Some([7; 32]), "test", 419, false);
    assert_eq!(
        eval::run_launch(&module, entry, &[0], 1_000_000, &[], &launch).outcome,
        eval::Outcome::Returned(vec![419 + 32 + 4])
    );
}

fn runner_launch_fixture(directory: &Path, runner_override: Option<&Path>) -> Arc<CheckProduct> {
    #[derive(serde::Deserialize)]
    struct StoreManifest {
        packages: Vec<PackageManifest>,
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut request =
        CheckRequest::libraries(&root.join("core"), &directory.join("store")).unwrap();
    let StoreManifest { mut packages } = serde_json::from_slice(
        &std::fs::read(root.join("core/programs/store/manifest.json")).unwrap(),
    )
    .unwrap();
    for source in packages.iter_mut().flat_map(|package| &mut package.sources) {
        source.file = root.join("core/programs/store").join(&source.file);
    }
    let store = packages
        .iter_mut()
        .find(|p| p.package == package("store"))
        .unwrap();
    if let Some(path) = runner_override {
        store
            .sources
            .iter_mut()
            .find(|source| source.modules == ["runner"])
            .unwrap()
            .file = path.to_owned();
    }
    store.sources.push(hirsel_core_resolve::ModuleSource {
        file: root.join("crates/hirsel-core-driver/src/testdata/runner_launch.hirsel"),
        modules: vec!["runner_launch".into()],
        signatures: false,
    });
    request.inputs.packages.extend(packages);
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    product
}

fn run_runner_launch(
    product: &CheckProduct,
    name: &str,
    n_place: u64,
    describe: bool,
) -> eval::Report {
    let identity = hirsel_core_ir::Identity::plain(
        DefPath::from_full(&format!("store@1/runner_launch/{name}")).unwrap(),
    );
    let units = product.units([identity.clone()], 65_536).unwrap();
    let modules = units.modules().unwrap();
    let (module, entry) = hirsel_core_materialize::join(&modules, &identity).unwrap();
    let launch = hirsel_core_ids::launch::Metadata::embedded(
        Some([7; 32]),
        "test-target",
        n_place,
        describe,
    );
    eval::run_launch(&module, entry, &[0, 1], 64 * 1024 * 1024, &[], &launch)
}

#[test]
fn runner_describe_launch_is_bounded_and_precedes_serve() {
    let directory = tempfile::tempdir().unwrap();
    let product = runner_launch_fixture(directory.path(), None);
    let report = run_runner_launch(&product, "law_main", 1024 * 1024, true);
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![0]));
    assert!(report.stderr.is_empty());
    assert!(!report.stdout.is_empty());
    for allowance in [0, 1] {
        let report = run_runner_launch(&product, "law_main", allowance, true);
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![2]));
        assert_eq!(
            report.stdout.len(),
            crate::describe::PLACE_CAPACITY_ERR_LEN as usize
        );
        assert_eq!(&report.stdout[..4], &0u32.to_le_bytes());
        let requested = u64::from_le_bytes(report.stdout[4..12].try_into().unwrap());
        let available = u64::from_le_bytes(report.stdout[12..20].try_into().unwrap());
        assert!(requested > available && available <= allowance);
    }
    let duplicate = run_runner_launch(&product, "law_duplicate", 1024 * 1024, true);
    assert_eq!(duplicate.outcome, eval::Outcome::Returned(vec![2]));
    assert_eq!(&duplicate.stdout[..4], &1u32.to_le_bytes());
    assert!(duplicate.stdout[8..].starts_with(b"\x01store@1/runner_launch/echo("));
    let denied = run_runner_launch(&product, "law_capability", 1024 * 1024, true);
    assert_eq!(denied.outcome, eval::Outcome::Stopped("DescribeCapability"));
    assert!(denied.stdout.is_empty());
    let ordinary = run_runner_launch(&product, "law_main", 1024 * 1024, false);
    assert_eq!(ordinary.outcome, eval::Outcome::Returned(vec![71]));
    assert!(ordinary.stdout.is_empty());
}
#[test]
#[should_panic(expected = "describe must finish before serve")]
fn store_opened_in_describe_mutant_is_caught_by_the_launch_law() {
    let directory = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = std::fs::read_to_string(root.join("core/programs/store/runner.hirsel")).unwrap();
    let marker = "    if launch_word(5u64, 0u64) == 0u64 {";
    assert!(source.contains(marker));
    let mutant = source.replacen(marker, "    if launch_word(5u64, 0u64) != 0u64 { return start_server() }\n    if launch_word(5u64, 0u64) == 0u64 {", 1);
    let path = directory.path().join("runner-mutant.hirsel");
    std::fs::write(&path, mutant).unwrap();
    let product = runner_launch_fixture(directory.path(), Some(&path));
    let report = run_runner_launch(&product, "law_main", 1024 * 1024, true);
    assert_eq!(
        report.outcome,
        eval::Outcome::Returned(vec![0]),
        "describe must finish before serve"
    );
}

#[test]
fn runner_describe_capture_matches_reply_schema() {
    let directory = tempfile::tempdir().unwrap();
    let product = runner_launch_fixture(directory.path(), None);
    let report = run_runner_launch(&product, "law_main", 1024 * 1024, true);
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![0]));
    let mut wire = report.stdout.as_slice();
    fn count(wire: &mut &[u8]) -> usize {
        let value = u32::from_le_bytes(wire[..4].try_into().unwrap()) as usize;
        *wire = &wire[4..];
        value
    }
    fn bytes<'a>(wire: &mut &'a [u8]) -> &'a [u8] {
        let len = count(wire);
        let value = &wire[..len];
        *wire = &wire[len..];
        value
    }
    assert_eq!(bytes(&mut wire), &[7; 32]);
    assert_eq!(bytes(&mut wire), b"test-target");
    assert_eq!(
        u64::from_le_bytes(wire[..8].try_into().unwrap()),
        1024 * 1024
    );
    wire = &wire[8..];
    assert_eq!(count(&mut wire), 2);
    assert_eq!(count(&mut wire), 0); // Described.kind = Compute.
    assert!(bytes(&mut wire).starts_with(b"\x01store@1/runner_launch/echo("));
    let args = bytes(&mut wire);
    let result = bytes(&mut wire);
    assert_eq!(args, result);
    assert_eq!(args, &[3, 0, 0, 0, 0, 6]);
    const U64_FP: u64 = 0x8dbfb7fd6918702c;
    assert_eq!(&wire[..8], &U64_FP.to_le_bytes());
    assert_eq!(&wire[8..16], &U64_FP.to_le_bytes());
    wire = &wire[16..];
    assert!(bytes(&mut wire).is_empty()); // The evaluator fixture has no code revision.
    assert_eq!(count(&mut wire), 1); // Transact.
    assert!(bytes(&mut wire).starts_with(b"\x01store@1/runner_launch/transaction("));
    assert_eq!(bytes(&mut wire), &[3, 0, 0, 0, 0, 6]);
    // Result[u64,bool], including its Result opcode 17 and error arm bool=1.
    assert_eq!(bytes(&mut wire), &[3, 0, 0, 0, 0, 17, 6, 1]);
    assert_eq!(&wire[..8], &U64_FP.to_le_bytes());
    assert_eq!(&wire[8..16], &0xebc2239a2dc21ea8u64.to_le_bytes());
    wire = &wire[16..];
    assert!(bytes(&mut wire).is_empty());
    assert!(wire.is_empty());
}

/// Separate definition units must preserve generic calls and function values.
/// The oracle is 20 + 21 + 1. Two definitions claiming the same code variant
/// while returning different values are a link conflict.
#[test]
fn joined_units_preserve_calls_and_refuse_conflicting_variants() {
    use hirsel_core_ir::{Identity, Inst};
    use hirsel_core_materialize::{Unit, join};
    let dir = tempfile::tempdir().unwrap();
    let source = "type Pair { x: u64, y: u64 }\nfn id[T](take x: T) -> T { x }\nfn later() -> u64 { let p = id(Pair(x: 20, y: 21)); p.x + p.y }\nfn answer() -> u64 { let f = fn() -> u64 { later() }; id(f()) + 1 }\n";
    let product = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let identity = Identity::plain(DefPath::from_full("app@1/main/answer").unwrap());
    let units = product.units([identity.clone()], 65_536).unwrap();
    let modules = units.modules().unwrap();
    let (joined, entry) = join(&modules, &identity).unwrap();
    hirsel_core_ir::wf::module(&joined).unwrap();
    assert_eq!(
        eval::run(&joined, entry, &[], 1_000_000).outcome,
        eval::Outcome::Returned(vec![42])
    );
    let root = modules.iter().find(|u| *u.identity == identity).unwrap();
    let mut a = root.module.clone();
    a.code_variants.insert(root.function, identity.digest());
    let mut b = a.clone();
    let body = b.functions[root.function.index()].body.as_mut().unwrap();
    let constant = body
        .insts
        .iter_mut()
        .find_map(|i| match i {
            Inst::Const { bits, .. } if *bits == 1 => Some(bits),
            _ => None,
        })
        .unwrap();
    *constant = 2;
    let conflict = [
        Unit {
            module: &a,
            ..*root
        },
        Unit {
            module: &b,
            ..*root
        },
    ];
    assert!(
        join(&conflict, &identity)
            .unwrap_err()
            .contains("conflicting code identity")
    );
}
/// A body record runs like the checked body, and every product's module hash
/// is the `hirsel.core-ir.v1` hash of its module's whole serialization: fresh, reopened from
/// the store, demanded from an image, and the helper for modules no record
/// carries, with a scratch buffer left holding another module's bytes.
/// Oracle: an independent encode and digest.
#[test]
fn publishes_a_checked_body_record_that_runs_like_the_checked_body() {
    let full = |ir: &Module| {
        ContentHash::of_parts("hirsel.core-ir.v1", [&bincode::serialize(ir).unwrap()[..]])
    };
    let dir = tempfile::tempdir().unwrap();
    let source = "fn answer() -> u64 { 6 * 7 }\nfn wider(a: u64, b: u64) -> u64 { if a > b { a * b } else { a + b + 1 } }\n";
    let check = || {
        let product = Driver::default()
            .check(request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(product.accepted(), "{:?}", product.diagnostics);
        product
    };
    let product = check();
    let answer = DefPath::from_full("app@1/main/answer").unwrap();
    let d = &product.definitions[&answer];
    let value = eval::run(d.ir().unwrap(), d.check.function.unwrap(), &[], 1_000_000).outcome;
    assert_eq!(value, eval::Outcome::Returned(vec![42]));
    let store = Store::new(&dir.path().join("store")).unwrap();
    let index = check_index::ProgramIndex::open(&store).unwrap();
    assert_eq!(index.root, Some(product.root));
    let (_, record) = index.live(&d.definition).unwrap();
    let body = store.get_body(record).unwrap().unwrap();
    let disk: DiskCheck = decode(store.get_body_head(record).unwrap().unwrap().head()).unwrap();
    let ir: Module = decode(body.ir()).unwrap();
    assert_eq!(
        eval::run(&ir, FuncId(disk.function.unwrap()), &[], 1_000_000).outcome,
        value
    );
    let reopened = check();
    assert!(reopened.rechecked.is_empty());
    let mut scratch = Vec::new();
    for d in product.definitions.values() {
        let expected = full(d.ir().unwrap());
        assert_eq!(d.ir_hash(), expected, "fresh {}", d.definition);
        assert_eq!(
            reopened.definitions[&d.definition].ir_hash(),
            expected,
            "reopened"
        );
        assert_eq!(
            full(reopened.definitions[&d.definition].ir().unwrap()),
            expected
        );
        // The scratch holds the previous, unequal module's bytes.
        assert_eq!(
            crate::body::ir_hash(d.ir().unwrap(), &mut scratch).unwrap(),
            expected
        );
    }
    let image_dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request("pub fn answer() -> u64 { 7u64 }", image_dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    let image = CoreImage::open(&image.unwrap().store_root())
        .unwrap()
        .unwrap();
    let demanded = image
        .demand(&DefPath::from_full("core@1/lib/answer").unwrap())
        .unwrap();
    assert_eq!(demanded.ir_hash(), full(demanded.ir().unwrap()), "image");
}
/// The body frame, against a hand-built envelope: `HBODY VERSION head_len
/// ir_len head ir_hash ir prefix_check ir_check address`, the address the
/// SHA-256 of everything before the IR and the checks the xxh3-64 of that
/// prefix and of the IR. Any changed byte, a truncation, a trailing byte or
/// an impossible length reads as nothing; a changed IR byte keeps the address,
/// so only the IR check refuses it. The head read (`get_body_head`) refuses the
/// same frames except a changed IR or IR-check byte, which it never reads: a
/// same-length IR damage is found by the IR read. An ordinary read refuses a
/// body frame and a body read an ordinary one, and neither accepts VERSION 31.
#[test]
fn a_body_record_reads_back_only_whole() {
    use sha2::Digest;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path()).unwrap();
    let (head, ir) = (b"a head".as_slice(), b"the module bytes".as_slice());
    let ir_hash = ContentHash::of_parts("hirsel.core-ir.v1", [ir]);
    let mut expected = b"HBODY\0\0\0".to_vec();
    expected.extend(37u32.to_le_bytes());
    expected.extend((head.len() as u64).to_le_bytes());
    expected.extend((ir.len() as u64).to_le_bytes());
    expected.extend(head);
    expected.extend(ir_hash.0);
    let address = ContentHash(sha2::Sha256::digest(&expected).into());
    let prefix_check = twox_hash::XxHash3_64::oneshot(&expected);
    expected.extend(ir);
    expected.extend(prefix_check.to_le_bytes());
    expected.extend(twox_hash::XxHash3_64::oneshot(ir).to_le_bytes());
    expected.extend(address.0);
    let framed = Store::frame_body(head, |b| {
        b.extend_from_slice(ir);
        Ok(())
    })
    .unwrap();
    assert_eq!(framed, (address, ir_hash, expected.clone()));
    let read = |bytes: &[u8]| {
        std::fs::write(store.path(address), bytes).unwrap();
        store.get_body(address).unwrap()
    };
    let read_head = |bytes: &[u8]| {
        std::fs::write(store.path(address), bytes).unwrap();
        store.get_body_head(address).unwrap()
    };
    let ir_at = expected.len() - 48 - ir.len();
    let ir_check = expected.len() - 40..expected.len() - 32;
    let whole = read(&expected).unwrap();
    assert_eq!(
        (
            store.get_body_head(address).unwrap().unwrap().head(),
            whole.ir(),
            whole.ir_hash
        ),
        (head, ir, ir_hash)
    );
    for at in 0..expected.len() {
        let mut flipped = expected.clone();
        flipped[at] ^= 1;
        assert!(read(&flipped).is_none(), "byte {at}");
        let in_ir = (ir_at..ir_at + ir.len()).contains(&at) || ir_check.contains(&at);
        assert_eq!(read_head(&flipped).is_some(), in_ir, "head read, byte {at}");
    }
    for len in 0..expected.len() {
        assert!(read(&expected[..len]).is_none(), "{len} bytes");
        assert!(
            read_head(&expected[..len]).is_none(),
            "head read, {len} bytes"
        );
    }
    let trailing = [&expected[..], &[0]].concat();
    assert!(read(&trailing).is_none(), "trailing byte");
    assert!(read_head(&trailing).is_none(), "head read, trailing byte");
    for at in [12, 20] {
        let mut long = expected.clone();
        long[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(read(&long).is_none(), "length at {at}");
        assert!(read_head(&long).is_none(), "head read, length at {at}");
    }
    let mut old = expected.clone();
    old[8..12].copy_from_slice(&31u32.to_le_bytes());
    assert!(read(&old).is_none(), "VERSION 31 body");
    assert!(read_head(&old).is_none(), "head read, VERSION 31 body");
    std::fs::write(store.path(address), &expected).unwrap();
    assert_eq!(
        store.get(address).unwrap(),
        None,
        "an ordinary read of a body"
    );
    let ordinary = store.put(b"an ordinary payload").unwrap();
    assert!(
        store.get_body(ordinary).unwrap().is_none(),
        "a body read of an ordinary record"
    );
    let mut bytes = std::fs::read(store.path(ordinary)).unwrap();
    bytes[8..12].copy_from_slice(&31u32.to_le_bytes());
    std::fs::write(store.path(ordinary), bytes).unwrap();
    assert_eq!(store.get(ordinary).unwrap(), None, "VERSION 31 record");
}
#[test]
fn refused_body_is_published_and_unreached_demand_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(
            request("fn bad() -> bool { let x = 7; true }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(!product.accepted());
    assert!(product.diagnostics.iter().any(|r| r.rule == "LANG-11"));
    assert!(
        product
            .definitions
            .values()
            .next()
            .unwrap()
            .check
            .function
            .is_none()
    );
    let absent = DefPath::from_full("app@1/absent/missing").unwrap();
    assert!(
        product
            .demand(&absent)
            .err()
            .unwrap()
            .to_string()
            .contains("outside reached")
    );
}
/// Each stored kind against its hand-built frame: a record is `HCORE VERSION
/// payload len check address`, a selection `address check`, and a bare object
/// its plain bytes under the checksum its referrer holds (each check the
/// xxh3-64 of what precedes it). Every flipped byte, the checksum field's
/// included, and every power-loss shape (short, zero-filled, missing) reads as
/// nothing; the whole frame reads back.
#[test]
fn damaged_disk_bytes_are_a_miss() {
    use sha2::Digest;
    use twox_hash::XxHash3_64;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path()).unwrap();
    let payload = b"published payload";
    let mut record = b"HCORE\0\0\0".to_vec();
    record.extend(37u32.to_le_bytes());
    record.extend(payload);
    record.extend((payload.len() as u64).to_le_bytes());
    let key = ContentHash(sha2::Sha256::digest(&record).into());
    record.extend(XxHash3_64::oneshot(&record).to_le_bytes());
    record.extend(key.0);
    assert_eq!(store.put(payload).unwrap(), key);
    assert_eq!(std::fs::read(store.path(key)).unwrap(), record);
    let mut selection = key.0.to_vec();
    selection.extend(b"live");
    selection.extend(XxHash3_64::oneshot(&selection).to_le_bytes());
    store.select("live", key).unwrap();
    assert_eq!(std::fs::read(dir.path().join("live")).unwrap(), selection);
    let object = b"\x7fELF object bytes".as_slice();
    let (bare, _) = store
        .put_bare(ContentHash::of_bytes(b"object"), object)
        .unwrap();
    assert_eq!(bare.check, XxHash3_64::oneshot(object));
    let kinds: [(&str, PathBuf, &[u8], &dyn Fn() -> bool); 3] = [
        ("record", store.path(key), &record, &|| {
            store.get(key).unwrap().is_some()
        }),
        ("selection", dir.path().join("live"), &selection, &|| {
            store.selection("live").unwrap() == Some(key)
        }),
        ("bare object", store.path(bare.address), object, &|| {
            store.get_bare(bare).unwrap().is_some()
        }),
    ];
    for (kind, path, good, reads) in kinds {
        let write = |bytes: &[u8]| std::fs::write(&path, bytes).unwrap();
        for at in 0..good.len() {
            let mut damaged = good.to_vec();
            damaged[at] ^= 1;
            write(&damaged);
            assert!(!reads(), "{kind}: byte {at}");
        }
        for len in 0..good.len() {
            write(&good[..len]);
            assert!(!reads(), "{kind}: {len} bytes");
        }
        write(&vec![0; good.len()]);
        assert!(!reads(), "{kind}: zero-filled");
        std::fs::remove_file(&path).unwrap();
        assert!(!reads(), "{kind}: missing");
        write(good);
        assert!(reads(), "{kind}: whole");
    }
    assert_eq!(store.get(key).unwrap().unwrap(), payload);
    assert_eq!(store.get_bare(bare).unwrap().unwrap().0, object);
}

#[test]
fn prog8_unrelated_parse_failure_does_not_suppress_other_results() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn answer() -> u64 { 42 }\nfn independently_bad() -> u64 { true }\n";
    let mut req = request(source, dir.path());
    let bad = dir.path().join("broken.hirsel");
    std::fs::write(&bad, "fn broken(\n").unwrap();
    req.inputs.packages[0]
        .sources
        .push(hirsel_core_resolve::ModuleSource {
            file: bad,
            modules: vec!["broken".into()],
            signatures: false,
        });
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(!product.accepted());
    let diagnostics = product.machine_diagnostics().unwrap();
    assert!(
        diagnostics.len() >= 2,
        "{}",
        serde_json::to_string(&diagnostics).unwrap()
    );
    assert!(diagnostics.iter().any(|d| d.rule == "LANG-11"
        && d.primary.module == "app@1/main"
        && &source[d.primary.span.start as usize..d.primary.span.end as usize] == "true"));
    let answer = DefPath::from_full("app@1/main/answer").unwrap();
    let d = product.demand(&answer).unwrap();
    assert_eq!(
        eval::run(d.ir().unwrap(), d.check.function.unwrap(), &[], 1_000_000).outcome,
        eval::Outcome::Returned(vec![42])
    );
}

/// The root a store publishes.
fn published(store: &Path) -> ContentHash {
    Store::new(store)
        .unwrap()
        .selection(check_index::SELECTION)
        .unwrap()
        .unwrap()
}

fn names(product: &CheckProduct) -> Vec<&str> {
    let mut names: Vec<_> = product
        .rechecked
        .iter()
        .map(|p| p.0.name.as_str())
        .collect();
    names.sort_unstable();
    names
}

#[test]
fn warm_and_body_edits_reuse_signature_readers_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let a = "fn callee() -> u64 { 1 }\nfn caller() -> u64 { callee() }\nfn independent() -> u64 { 42 }\n";
    let first = driver
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&first), ["callee", "caller", "independent"]);
    let warm = driver
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(warm.rechecked.is_empty());
    let b = a.replace("{ 1 }", "{ 2 }");
    let edit = driver
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&edit), ["callee"]);
    let callee = edit
        .definitions
        .values()
        .find(|d| d.definition.0.name == "callee")
        .unwrap();
    assert_eq!(
        eval::run(
            callee.ir().unwrap(),
            callee.check.function.unwrap(),
            &[],
            1_000_000
        )
        .outcome,
        eval::Outcome::Returned(vec![2])
    );
    let disk_warm = Driver::default()
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(disk_warm.rechecked.is_empty());
    let disk_edit = Driver::default()
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&disk_edit), ["callee"]);
}
#[test]
fn signature_edits_recheck_readers_and_leave_independent_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    // `outer` reads only `caller`'s signature, which a body name of `caller`
    // does not enter, wherever `caller` sits in the file.
    let a = "fn callee() -> u64 { 1 }\nfn caller() -> u64 { callee() }\nfn independent() -> u64 { 42 }\nfn outer() -> u64 { caller() }\n";
    driver
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    let b = a
        .replace("callee() -> u64", "callee() -> bool")
        .replace("{ 1 }", "{ true }");
    let edited = driver
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&edited), ["callee", "caller"]);
    assert!(!edited.accepted());
    assert!(edited.diagnostics.iter().any(|r| r.primary.item == 1));
    let reused = Driver::default()
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(reused.rechecked.is_empty());
    assert_eq!(
        encode(&edited.diagnostics).unwrap(),
        encode(&reused.diagnostics).unwrap()
    );
}
/// A duplicate definition (LANG-7) checked against a store that holds its
/// copies reports what a fresh store reports. The copies share one name,
/// whose key hashes one copy's text, so none is a hit; the path's product is
/// the last copy's. Found as a panic framing a stored hit that replaced the
/// fresh copy before it (the second pair); the third pair's type refusal was
/// lost to a hit under the other copy's text.
#[test]
fn a_duplicate_definition_over_a_stored_copy_reports_as_fresh() {
    for (stored, duplicate) in [
        (
            "fn f() -> u64 { 1 }\nfn f() -> u64 { 2 }\nfn g() -> u64 { f() }\n",
            "fn f() -> u64 { 3 }\nfn f() -> u64 { 2 }\nfn g() -> u64 { f() }\n",
        ),
        (
            "fn f() -> u64 { 1 }\nfn f() -> u64 { 2 }\nfn g() -> u64 { f() }\n",
            "fn f() -> u64 { g() }\nfn f() -> u64 { 2 }\nfn g() -> u64 { f() }\n",
        ),
        (
            "fn f() -> u64 { 1 }\nfn f() -> u64 { 2 }\nfn g() -> u64 { f() }\n",
            "fn f() -> u64 { true }\nfn f() -> u64 { 2 }\nfn g() -> u64 { f() }\n",
        ),
        (
            "fn f() -> u64 { 1 }\nfn g() -> u64 { f() }\n",
            "fn f() -> u64 { 1 }\nfn f() -> u64 { 2 }\nfn g() -> u64 { f() }\n",
        ),
    ] {
        let primed = tempfile::tempdir().unwrap();
        Driver::default()
            .check(request(stored, primed.path()), &mut Meter::new(false))
            .unwrap();
        let over = Driver::default()
            .check(request(duplicate, primed.path()), &mut Meter::new(false))
            .unwrap();
        let empty = tempfile::tempdir().unwrap();
        let fresh = Driver::default()
            .check(request(duplicate, empty.path()), &mut Meter::new(false))
            .unwrap();
        assert!(
            fresh.diagnostics.iter().any(|r| r.rule == "LANG-7"),
            "{duplicate}"
        );
        assert_eq!(
            encode(&over.diagnostics).unwrap(),
            encode(&fresh.diagnostics).unwrap(),
            "{duplicate}"
        );
        let again = Driver::default()
            .check(request(duplicate, primed.path()), &mut Meter::new(false))
            .unwrap();
        assert_eq!(
            encode(&again.diagnostics).unwrap(),
            encode(&fresh.diagnostics).unwrap(),
            "{duplicate}: read back"
        );
    }
}
#[test]
fn negative_and_shadowed_name_reads_invalidate_on_new_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let a = "fn local() -> u64 { let invented = 42u64; invented }\nfn missing() -> u64 { future() }\nfn bad() -> u64 { let n = 7; 0 }\n";
    let old = driver
        .check(request(a, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&old), ["bad", "local", "missing"]);
    let b = format!("{a}fn invented() -> u64 {{ 3 }}\nfn future() -> u64 {{ 9 }}\n");
    let edited = driver
        .check(request(&b, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&edited), ["future", "invented", "local", "missing"]);
    assert!(
        edited
            .diagnostics
            .iter()
            .any(|r| r.rule == "LANG-7" && r.message.contains("invented"))
    );
    let local = edited
        .definitions
        .values()
        .find(|d| d.definition.0.name == "local")
        .unwrap();
    assert_ne!(
        old.definitions[&local.definition].key.lookups,
        local.key.lookups
    );
    let missing = edited
        .definitions
        .values()
        .find(|d| d.definition.0.name == "missing")
        .unwrap();
    assert!(missing.check.refusals.is_empty());
    assert!(missing.check.function.is_some());
}
#[test]
fn prog1_prog6_transitive_type_reads_and_discovery_order_have_stable_results() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let other = dir.path().join("other.hirsel");
    std::fs::write(&other, "fn extra() -> u64 { 3 }\n").unwrap();
    let configured = |text: &str| {
        let mut req = request(text, dir.path());
        req.inputs.packages[0]
            .sources
            .push(hirsel_core_resolve::ModuleSource {
                file: other.clone(),
                modules: vec!["other".into()],
                signatures: false,
            });
        req
    };
    let a = "type Number = u64\nfn callee() -> Number { 1 }\nfn caller() -> u64 { callee() }\nfn independent() -> u64 { 42 }\n";
    let old = driver.check(configured(a), &mut Meter::new(false)).unwrap();
    assert!(old.accepted(), "{:?}", old.diagnostics);
    let b = a.replace("Number = u64", "Number = u32");
    let edited = driver
        .check(configured(&b), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&edited), ["callee", "caller"]);
    let mut req = configured(&b);
    req.inputs.packages.reverse();
    req.inputs.packages[0].sources.reverse();
    req.store = dir.path().join("reverse-store");
    let reversed = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert_eq!(
        published(&dir.path().join("store")),
        published(&dir.path().join("reverse-store"))
    );
    assert_eq!(
        encode(&edited.diagnostics).unwrap(),
        encode(&reversed.diagnostics).unwrap()
    );
}

#[test]
fn damaged_disk_results_recompute() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn answer() -> u64 { 42 }\n";
    let first = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let store = Store::new(&dir.path().join("store")).unwrap();
    std::fs::write(store.path(first.root), b"damaged").unwrap();
    let disk = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(disk.accepted());
    assert_eq!(names(&disk), ["answer"]);
}

#[test]
fn adding_a_definition_rebinds_cached_refusal_sites_without_checking_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let old = "fn first() -> u64 { 1 }\nfn bad() -> u64 { let n = 7; 0 }\n";
    driver
        .check(request(old, dir.path()), &mut Meter::new(false))
        .unwrap();
    let new = old.replace("fn bad()", "fn inserted() -> u64 { 2 }\nfn bad()");
    let result = driver
        .check(request(&new, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert_eq!(names(&result), ["inserted"]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.rule == "LANG-11" && d.primary.item == 2)
    );
}

#[test]
fn failed_archive_write_keeps_previous_root_and_removes_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path()).unwrap();
    let old = store.put(b"previous complete result").unwrap();
    store.select(check_index::SELECTION, old).unwrap();
    let failure = store.archive(|writer| {
        use std::io::Write;
        writer.write_all(b"unfinished next result")?;
        Err(io::Error::other("injected publication failure"))
    });
    assert!(failure.is_err());
    assert_eq!(store.selection(check_index::SELECTION).unwrap(), Some(old));
    assert_eq!(
        store.get(old).unwrap().unwrap(),
        b"previous complete result"
    );
    // The record, the selection and `tmp`, which holds no temporary.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    assert_eq!(
        std::fs::read_dir(dir.path().join("tmp")).unwrap().count(),
        0
    );
}

#[test]
fn native_ir_tables_round_trip_without_text_reconstruction() {
    for source in [
        include_str!("../../hirsel-core-ir/samples/arith.core"),
        include_str!("../../hirsel-core-ir/samples/compare-convert.core"),
        include_str!("../../hirsel-core-ir/samples/control.core"),
        include_str!("../../hirsel-core-ir/samples/slots.core"),
        include_str!("../../hirsel-core-ir/samples/calls.core"),
        include_str!("../../hirsel-core-ir/samples/failure.core"),
        include_str!("../../hirsel-core-ir/samples/cleanup.core"),
        include_str!("../../hirsel-core-ir/samples/admission.core"),
        include_str!("../../hirsel-core-ir/samples/counted.core"),
        include_str!("../../hirsel-core-ir/samples/custody.core"),
        include_str!("../../hirsel-core-ir/samples/variants.core"),
    ] {
        let ir = hirsel_core_ir::text::parse(source).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path()).unwrap();
        let mut at = None;
        let hash = store
            .archive(|writer| {
                at = Some(writer.record(&ir)?);
                Ok(())
            })
            .unwrap();
        let bytes = store.get(hash).unwrap().unwrap();
        let decoded: Module = decode(at.unwrap().bytes(&bytes).unwrap()).unwrap();
        assert_eq!(ir, decoded);
    }
    let dir = tempfile::tempdir().unwrap();
    let source = "fn f(x: u64) -> u64 { let mut n: u64 = 0; while n < x { n += 1 }; if n == x { n } else { 0 } }\n";
    let first = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(first.accepted(), "{:?}", first.diagnostics);
    let second = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(second.rechecked.is_empty());
    let a = first.definitions.values().next().unwrap();
    let b = second.definitions.values().next().unwrap();
    assert_eq!(a.ir().unwrap(), b.ir().unwrap());
    assert_eq!(
        eval::run(b.ir().unwrap(), b.check.function.unwrap(), &[17], 1_000_000).outcome,
        eval::Outcome::Returned(vec![17])
    );
}

fn library_request(source: &str, dir: &Path) -> CheckRequest {
    let mut req = request(source, dir);
    req.inputs.packages[0].package = package("core");
    req.inputs.packages[0].sources[0].modules = vec!["lib".into()];
    req
}
#[test]
fn image_signature_use_skips_source_and_body_decoding() {
    let dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request(
                "pub type Number = u64\npub fn answer() -> Number { 42 }\n",
                dir.path(),
            ),
            &mut Meter::new(false),
        )
        .unwrap();
    let produced = image.unwrap();
    let opened = CoreImage::open(&dir.path().join("store")).unwrap().unwrap();
    assert_eq!(opened.identity, produced.identity);
    assert_eq!((opened.decoded_files(), opened.decoded_bodies()), (0, 0));
    std::fs::remove_file(dir.path().join("main.hirsel")).unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request(
        "fn caller(x: lib.Number) -> u64 { x + lib.answer() }\n",
        app.path(),
    );
    req.library = LibraryInput::Image(Arc::clone(&opened));
    let checked = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(
        checked.accepted(),
        "{:?} {:?}",
        checked.diagnostics,
        checked.incomplete
    );
    assert_eq!(names(&checked), ["caller"]);
    assert_eq!((opened.decoded_files(), opened.decoded_bodies()), (0, 0));
    // The caller's key reads the installed signatures it names: an image
    // header edit changes it and an image body edit does not.
    let caller_signatures = |library: &str| {
        let lib = tempfile::tempdir().unwrap();
        let (_, image) = Driver::default()
            .produce_image(library_request(library, lib.path()), &mut Meter::new(false))
            .unwrap();
        let app = tempfile::tempdir().unwrap();
        let mut req = request(
            "fn caller(x: lib.Number) -> u64 { x + lib.answer() }\n",
            app.path(),
        );
        req.library = LibraryInput::Image(image.unwrap());
        let checked = Driver::default()
            .check(req, &mut Meter::new(false))
            .unwrap();
        checked.definitions.values().next().unwrap().key.signatures
    };
    let caller = checked.definitions.values().next().unwrap();
    assert_eq!(
        caller.key.signatures,
        caller_signatures("pub type Number = u64\npub fn answer() -> Number { 43 }\n")
    );
    assert_ne!(
        caller.key.signatures,
        caller_signatures("pub type Number = u64\npub fn answer() -> u64 { 42 }\n")
    );
    let answer = DefPath::from_full("core@1/lib/answer").unwrap();
    let body = checked.demand_body(&answer).unwrap();
    assert_eq!(
        eval::run(
            body.ir().unwrap(),
            body.check.function.unwrap(),
            &[],
            1_000_000
        )
        .outcome,
        eval::Outcome::Returned(vec![42])
    );
    assert_eq!((opened.decoded_files(), opened.decoded_bodies()), (0, 1));
    let sources = checked
        .native_sources(std::slice::from_ref(&answer))
        .unwrap();
    let source = &sources.items[&answer];
    assert_eq!(source.file_offset, 22);
    assert_eq!(
        sources.files[source.file as usize].text,
        "pub type Number = u64\npub fn answer() -> Number { 42 }\n"
    );
    // The executable names the file by its module, not by where it was read.
    assert_eq!(
        sources.files[source.file as usize].path,
        "core@1/lib.hirsel"
    );
    assert_eq!(
        (opened.decoded_files(), opened.decoded_bodies()),
        (0, 1),
        "native source binding must not decode a published syntax tree"
    );
    assert!(
        opened
            .file(0)
            .unwrap()
            .syntax
            .text()
            .contains("pub type Number = u64")
    );
    assert_eq!(opened.decoded_files(), 1);
}
#[test]
fn corrupt_unused_image_body_is_a_miss_on_demand_only() {
    // The whole record, then only its module part under a valid address.
    for damage in ["record", "module part"] {
        let dir = tempfile::tempdir().unwrap();
        let (_, image) = Driver::default()
            .produce_image(
                library_request("pub fn answer() -> u64 { 42 }\n", dir.path()),
                &mut Meter::new(false),
            )
            .unwrap();
        let image = image.unwrap();
        let answer = DefPath::from_full("core@1/lib/answer").unwrap();
        let hash = image.directory().unwrap().definitions[&answer];
        let store = Store::new(&dir.path().join("store")).unwrap();
        if damage == "record" {
            std::fs::write(store.path(hash), b"damaged").unwrap();
        } else {
            let mut bytes = std::fs::read(store.path(hash)).unwrap();
            let last = bytes.len() - 33;
            bytes[last] ^= 1;
            std::fs::write(store.path(hash), bytes).unwrap();
        }
        let opened = CoreImage::open(&dir.path().join("store")).unwrap().unwrap();
        let app = tempfile::tempdir().unwrap();
        let mut req = request("fn caller() -> u64 { 42 }\n", app.path());
        req.library = LibraryInput::Image(Arc::clone(&opened));
        let checked = Driver::default()
            .check(req, &mut Meter::new(false))
            .unwrap();
        assert!(checked.accepted(), "{:?}", checked.diagnostics);
        // A demand reads the head alone: damage behind it refuses the first
        // read of the module, names the record and removes it.
        let error = match opened.demand(&answer) {
            Err(e) => e.to_string(),
            Ok(body) => {
                assert_eq!(damage, "module part");
                let error = body.ir().err().unwrap().to_string();
                assert!(!store.path(hash).exists(), "the damaged record is removed");
                error
            }
        };
        let expected = match damage {
            "record" => format!("{} is missing or damaged", store.path(hash).display()),
            _ => format!(
                "the body record {} of `{answer}` is damaged behind its head",
                store.path(hash).display()
            ),
        };
        assert!(error.contains(&expected), "{damage}: {error}");
        let missing = DefPath::from_full("core@1/absent/absent").unwrap();
        assert!(
            opened
                .demand(&missing)
                .err()
                .unwrap()
                .to_string()
                .contains("outside reached")
        );
    }
}
#[test]
fn refused_library_never_publishes_an_image() {
    let dir = tempfile::tempdir().unwrap();
    let (product, image) = Driver::default()
        .produce_image(
            library_request("pub fn bad() -> u64 { true }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(!product.diagnostics.is_empty());
    assert!(image.is_none());
    assert!(
        CoreImage::open(&dir.path().join("store"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn prog7_machine_refusal_spans_point_into_current_file_bytes() {
    let dir = tempfile::tempdir().unwrap();
    // The refused `n` is the found `u16` where the result's `u64` is expected (LANG-11):
    // the machine form carries both facts, found as declared and expected as required.
    let source =
        "// leading comment\nfn good() -> u64 { 42 }\nfn bad() -> u64 { let n: u16 = 7; n }\n";
    let result = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let diagnostics = result.machine_diagnostics().unwrap();
    let refusal = diagnostics.iter().find(|d| d.rule == "LANG-11").unwrap();
    assert_eq!(refusal.primary.module, "app@1/main");
    let span = refusal.primary.span;
    assert_eq!(span.start as usize, source.rfind('n').unwrap());
    assert_eq!(&source[span.start as usize..span.end as usize], "n");
    let json = serde_json::to_value(refusal).unwrap();
    assert_eq!(json["declared"], "u16");
    assert_eq!(json["required"], "u64");
    assert_eq!(json["causal"], serde_json::json!([]));
}
#[test]
fn prog7_terminal_sites_count_scalar_columns_and_crlf_lines() {
    // Bugs: a column that counts bytes, and a CR before LF counted as a column.
    // Oracle: positions counted by hand below (docs/spec/diagnostics.md,
    // "Terminal sites"). On line 3, 44 scalars (50 bytes) precede the refused
    // `n`, so it is column 45; a byte count would say 51.
    let dir = tempfile::tempdir().unwrap();
    let source = "// \u{fc}\r\nfn good() -> u64 { 42 }\r\nfn bad() -> u64 { /* \u{e9}\u{2192}\u{1f600} */ let n: u16 = 7; n }\r\n";
    let req = request(source, dir.path());
    let files = crate::SourceFiles::of(&req);
    let path = req.inputs.packages[0].sources[0].file.clone();
    let result = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    let diagnostics = result.machine_diagnostics().unwrap();
    let refusal = diagnostics.iter().find(|d| d.rule == "LANG-11").unwrap();
    assert_eq!(
        files.render(&refusal.primary),
        format!("{}:3:45-46", path.display())
    );
    // "// \u{fc}" is four scalars: its CR is the line end, so the LF is column 5.
    let lf = source.find('\n').unwrap();
    assert_eq!(crate::line_column(source.as_bytes(), lf), (1, 5));
    assert_eq!(crate::line_column(source.as_bytes(), lf + 1), (2, 1));
}
#[test]
fn instance_collection_is_finite_deduplicated_and_order_independent() {
    use hirsel_core_ids::ContentHash;
    use hirsel_core_ir::Identity;
    let definition = |name: &str| DefPath::from_full(&format!("app@1/main/{name}")).unwrap();
    let key = |name: &str| Identity::plain(definition(name));
    let a = key("a");
    let b = key("b");
    let leaf = key("leaf");
    let edges = |k: &Identity| {
        Ok(if *k == a || *k == b {
            vec![leaf.clone(), leaf.clone()]
        } else {
            vec![]
        })
    };
    assert_eq!(
        collect_instances([a.clone(), b.clone()], 3, edges).unwrap(),
        collect_instances([b.clone(), a.clone()], 3, edges).unwrap()
    );
    let root = Identity {
        definition: definition("growing"),
        dictionaries: vec![],
        closures: vec![ContentHash([0; 32])],
    };
    let stop = collect_instances([root], 8, |key| {
        let mut next = key.clone();
        next.closures[0] = ContentHash::of_parts("list", [&key.closures[0].0[..]]);
        Ok(vec![next])
    })
    .unwrap_err();
    assert!(matches!(stop, InstanceStop::Resource { limit: 8, .. }));
}
#[test]
fn checked_instance_collection_follows_calls_without_body_rechecks() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(
            request(
                "fn leaf() -> u64 { 42 }\nfn root() -> u64 { leaf() }\n",
                dir.path(),
            ),
            &mut Meter::new(false),
        )
        .unwrap();
    let root = product
        .definitions
        .keys()
        .find(|p| p.0.name == "root")
        .unwrap();
    let root = hirsel_core_ir::Identity::plain(root.clone());
    let instances = product.instances([root], 8).unwrap();
    assert_eq!(
        instances
            .iter()
            .map(|i| i.definition.0.name.as_str())
            .collect::<Vec<_>>(),
        ["leaf", "root"]
    );
    assert_eq!(names(&product), ["leaf", "root"]);
}

#[test]
fn entry_waits_policy_is_an_explicit_cached_request_input() {
    let dir = tempfile::tempdir().unwrap();
    let source = "pub fn main() waits {}\n";
    let mut driver = Driver::default();
    let mut req = request(source, dir.path());
    req.entry = Some(DefPath::from_full("app@1/main/main").unwrap());
    let entry = driver.check(req, &mut Meter::new(false)).unwrap();
    // PROG-3 now refuses unit main. Entry still suppresses the unused-waits
    // refusal; checking this same body as an ordinary function must restore it.
    assert!(entry.diagnostics.iter().any(|d| d.rule == "PROG-3"));
    assert!(!entry.diagnostics.iter().any(|d| d.rule == "LANG-8"));
    let library = driver
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(!library.accepted());
    assert_eq!(names(&library), ["main"]);
    assert!(library.diagnostics.iter().any(|d| d.rule == "LANG-8"));
}

/// A request given an image declares over the image's declarations whether
/// its program reads them or not (OPTION-L L2), so a damaged declaration
/// section refuses both, never answers over a silently missing library.
#[test]
fn a_damaged_image_declaration_section_refuses_every_request_given_the_image() {
    let dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request("pub fn answer() -> u64 { 42 }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    let image = image.unwrap();
    let store = Store::new(&dir.path().join("store")).unwrap();
    std::fs::write(
        store.path(image.directory().unwrap().declarations),
        b"damaged",
    )
    .unwrap();
    let opened = CoreImage::open(&dir.path().join("store")).unwrap().unwrap();
    for source in [
        "fn scalar() -> u64 { 42 }\n",
        "fn caller() -> u64 { lib.answer() }\n",
    ] {
        let app = tempfile::tempdir().unwrap();
        let mut req = request(source, app.path());
        req.library = LibraryInput::Image(Arc::clone(&opened));
        let refused = Driver::default().check(req, &mut Meter::new(false)).err();
        assert!(
            refused.is_some_and(|e| e.to_string().contains("cache miss")),
            "{source}"
        );
    }
}

#[test]
fn a_damaged_section_page_refuses_the_request_and_names_the_image() {
    let dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request("pub fn answer() -> u64 { 42 }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    let image = image.unwrap();
    let store = Store::new(&dir.path().join("store")).unwrap();
    let root: crate::image::ImageRoot =
        decode(&store.get(image.identity).unwrap().unwrap()).unwrap();
    let sections: crate::image_index::Sections =
        decode(&store.get(root.sections).unwrap().unwrap()).unwrap();
    for page in &sections.pages {
        // One payload byte, inside an intact envelope: only the hash tells.
        let mut bytes = std::fs::read(store.path(*page)).unwrap();
        let at = bytes.len() - 41;
        bytes[at] ^= 1;
        std::fs::write(store.path(*page), bytes).unwrap();
    }
    let opened = CoreImage::open(&dir.path().join("store")).unwrap().unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request("fn caller() -> u64 { lib.answer() }\n", app.path());
    req.library = LibraryInput::Image(opened);
    let error = Driver::default()
        .check(req, &mut Meter::new(false))
        .err()
        .expect("a damaged page has no other path to its rows")
        .to_string();
    let name: String = image
        .identity
        .0
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(
        error.contains(&name) && error.contains("fails its hash check"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn directory_source_file_symlinks_are_checked_instead_of_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let package_dir = dir.path().join("package");
    std::fs::create_dir(&package_dir).unwrap();
    let source = dir.path().join("source.hirsel");
    std::fs::write(&source, "fn answer() -> u64 { 6 * 7 }\n").unwrap();
    std::os::unix::fs::symlink(source, package_dir.join("main.hirsel")).unwrap();
    let inputs = CorpusManifest {
        core: package("core"),
        packages: vec![PackageManifest::directory(&package_dir, package("app"), false).unwrap()],
    };
    let product = Driver::default()
        .check(
            CheckRequest {
                manifest: None,
                inputs,
                store: dir.path().join("store"),
                library: LibraryInput::Sources,
                entry: None,
            },
            &mut Meter::new(false),
        )
        .unwrap();
    assert_eq!(names(&product), ["answer"]);
    let body = product.definitions.values().next().unwrap();
    assert_eq!(
        eval::run(
            body.ir().unwrap(),
            body.check.function.unwrap(),
            &[],
            1_000_000
        )
        .outcome,
        eval::Outcome::Returned(vec![42])
    );
}

#[test]
fn installed_image_instances_are_demanded_without_source_rechecks() {
    let lib = tempfile::tempdir().unwrap();
    Driver::default()
        .produce_image(
            library_request("pub fn answer() -> u64 { 42 }\n", lib.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    let image = CoreImage::open(&lib.path().join("store")).unwrap().unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request("fn caller() -> u64 { lib.answer() }\n", app.path());
    req.library = LibraryInput::Image(Arc::clone(&image));
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    assert_eq!(image.decoded_bodies(), 0);
    let key = hirsel_core_ir::Identity::plain(DefPath::from_full("app@1/main/caller").unwrap());
    let instances = product.instances([key], 8).unwrap();
    assert_eq!(instances.len(), 2);
    assert_eq!(image.decoded_bodies(), 1);
    assert_eq!(names(&product), ["caller"]);
}

#[test]
fn prog7_invalid_utf8_diagnostics_keep_raw_byte_spans() {
    let dir = tempfile::tempdir().unwrap();
    let req = request("", dir.path());
    std::fs::write(dir.path().join("main.hirsel"), [0xf0, 0x28, 0x8c, 0x28]).unwrap();
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    let diagnostics = product.machine_diagnostics().unwrap();
    assert!(!diagnostics.is_empty());
    assert_eq!(diagnostics[0].primary.module, "app@1/main");
    assert_eq!(
        diagnostics[0].primary.span,
        hirsel_core_syntax::Span { start: 0, end: 1 }
    );
}

#[test]
fn session_id_changes_reuse_and_rebind_cached_refusals() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn bad() -> u64 { let n = 7; 0 }\n";
    let mut driver = Driver::default();
    let old = driver
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let earlier = dir.path().join("earlier.hirsel");
    std::fs::write(&earlier, "fn extra() -> u64 { 42 }\n").unwrap();
    let configured = || {
        let mut req = request(source, dir.path());
        req.inputs.packages[0]
            .sources
            .push(hirsel_core_resolve::ModuleSource {
                file: earlier.clone(),
                modules: vec!["aaa".into()],
                signatures: false,
            });
        req
    };
    let edited = driver.check(configured(), &mut Meter::new(false)).unwrap();
    assert_eq!(names(&edited), ["extra"]);
    assert_ne!(
        old.diagnostics[0].primary.module,
        edited.diagnostics[0].primary.module
    );
    let disk = Driver::default()
        .check(configured(), &mut Meter::new(false))
        .unwrap();
    assert!(disk.rechecked.is_empty());
    for product in [&edited, &disk] {
        let diagnostics = product.machine_diagnostics().unwrap();
        let refusal = diagnostics.iter().find(|d| d.rule == "LANG-11").unwrap();
        assert_eq!(refusal.primary.module, "app@1/main");
        let span = refusal.primary.span;
        assert_eq!(&source[span.start as usize..span.end as usize], "7");
    }
}

#[test]
fn incomplete_source_is_not_an_emittable_instance() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(
            request("fn root() -> u64 { let _ = b\"bytes\"; 42 }\n", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(!product.incomplete.is_empty());
    let key = hirsel_core_ir::Identity::plain(DefPath::from_full("app@1/main/root").unwrap());
    assert!(matches!(
        product.instances([key], 8),
        Err(InstanceStop::Incomplete { .. })
    ));
}

#[test]
fn reachable_build_ignores_unreached_bodies_and_declarations_after_disk_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn helper() -> u8 { 7u8 }\nfn answer() -> u8 { helper() }\nfn unused[T](take x: T) -> T { x }\ntype Broken = Missing\n";
    let entry = DefPath::from_full("app@1/main/answer").unwrap();
    let product = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(
        !product.accepted(),
        "whole-package check still rejects the input"
    );
    let reachable = product.reachable(&entry, 64).unwrap();
    assert!(reachable.accepted(), "{:?}", reachable.instances);
    assert_eq!(reachable.instances.as_ref().unwrap().len(), 2);
    assert_eq!(reachable.bodies, 2);
    assert!(reachable.first_stops.is_empty());
    let cached = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(cached.rechecked.is_empty());
    assert!(!cached.accepted());
    assert!(cached.reachable(&entry, 64).unwrap().accepted());
}

/// Bug: a resource refusal stored with the product, so a build under a larger instance limit
/// is still refused from the store (or a smaller limit is served an earlier acceptance).
/// The instance limit belongs to the build, not to the stored product: the same stored
/// product is refused at limit 1 (at the second instance's definition, with the amount
/// and the limit) and accepted at 64.
#[test]
fn instance_limit_refusal_is_per_build_and_never_served_from_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn helper() -> u8 { 7u8 }\nfn answer() -> u8 { helper() }\n";
    let entry = DefPath::from_full("app@1/main/answer").unwrap();
    let first = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(first.resources.is_empty());
    let tight = first.reachable(&entry, 1).unwrap();
    assert!(!tight.accepted());
    assert!(tight.incomplete.is_empty() && tight.first_stops.is_empty());
    let [resource] = tight.resources.as_slice() else {
        panic!("{:?}", tight.resources)
    };
    let at = resource.primary.span;
    assert_eq!(&source[at.start as usize..at.end as usize], "helper");
    assert!(
        resource
            .message
            .contains("needs instance 2 past the limit of 1")
    );
    let stored = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(stored.rechecked.is_empty());
    assert!(stored.resources.is_empty());
    assert!(stored.reachable(&entry, 64).unwrap().accepted());
    assert!(!stored.reachable(&entry, 1).unwrap().resources.is_empty());
}

#[test]
fn reachable_build_refuses_and_censuses_the_incomplete_callee() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn answer() -> u8 { unfinished() }\nfn unfinished() -> u8 { let _ = b\"bytes\"; 7u8 }\nfn unused[T](take x: T) -> T { x }\n";
    let entry = DefPath::from_full("app@1/main/answer").unwrap();
    let product = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let reachable = product.reachable(&entry, 64).unwrap();
    assert!(!reachable.accepted());
    let stopped = reachable.require().unwrap_err().to_string();
    assert!(stopped.contains("unfinished"), "{stopped}");
    assert!(
        reachable
            .first_stops
            .iter()
            .any(|s| s.definition == "app@1/main/unfinished")
    );
    assert!(
        !reachable
            .first_stops
            .iter()
            .any(|s| s.definition == "app@1/main/unused")
    );
    assert_eq!(reachable.census().values().sum::<usize>(), 1);
}

#[test]
fn reachable_build_keeps_refusals_in_named_signature_declarations() {
    // The declaration's refusal is kept wherever the declaration sits in its file.
    for source in [
        "type Broken = Missing\nfn inspect(x: Broken) -> u8 { 7u8 }\nfn answer() -> u8 { inspect(7u8) }\n",
        "fn inspect(x: Broken) -> u8 { 7u8 }\nfn answer() -> u8 { inspect(7u8) }\ntype Broken = Missing\n",
    ] {
        reachable_build_keeps_the_declaration_refusal(source);
    }
}
fn reachable_build_keeps_the_declaration_refusal(source: &str) {
    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/answer").unwrap();
    let product = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let reachable = product.reachable(&entry, 64).unwrap();
    assert!(!reachable.accepted());
    assert!(!reachable.diagnostics.is_empty());
    assert!(reachable.declares(&DefPath::from_full("app@1/main/Broken").unwrap()));
    assert!(
        reachable
            .first_stops
            .iter()
            .any(|s| s.definition == "app@1/main/Broken")
    );
}

#[test]
fn reachable_census_follows_names_beyond_an_incomplete_entry() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn answer() -> u8 { let _ = b\"bytes\"; unfinished() }\nfn unfinished() -> u8 { let _ = b\"bytes\"; 7u8 }\n";
    let product = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    let reachable = product
        .reachable(&DefPath::from_full("app@1/main/answer").unwrap(), 64)
        .unwrap();
    assert!(!reachable.accepted());
    assert_eq!(reachable.bodies, 2);
    assert_eq!(reachable.first_stops.len(), 2);
    assert_eq!(reachable.census().values().sum::<usize>(), 2);
}

#[test]
fn reachable_census_retains_a_named_function_without_a_native_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = request("fn answer() -> u8 { external() }\n", dir.path());
    let library = dir.path().join("core.hirsel");
    std::fs::write(&library, "pub fn external() -> u8\n").unwrap();
    req.inputs.packages[0].dependencies.push(package("core"));
    req.inputs.packages.push(PackageManifest {
        package: package("core"),
        sources: vec![hirsel_core_resolve::ModuleSource {
            file: library,
            modules: vec!["core".into()],
            signatures: true,
        }],
        dependencies: vec![],
    });
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    let reachable = product
        .reachable(&DefPath::from_full("app@1/main/answer").unwrap(), 64)
        .unwrap();
    assert!(!reachable.accepted());
    assert!(
        reachable
            .first_stops
            .iter()
            .any(|s| s.definition == "core@1/core/external"
                && s.construct == "native body is unavailable")
    );
    assert!(
        reachable
            .require()
            .unwrap_err()
            .to_string()
            .contains("external")
    );
}

#[test]
fn indirect_instances_follow_the_values_function_references() {
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default().check(request(
        "fn leaf(x: u64) -> u64 { x + 1 }\nfn apply(cb: fn(u64) -> u64, x: u64) -> u64 { cb(x) }\nfn root() -> u64 { let cb: fn(u64) -> u64 = leaf; apply(cb, 41) }\n",
        dir.path()), &mut Meter::new(false)).unwrap();
    assert!(product.accepted());
    let root = hirsel_core_ir::Identity::plain(DefPath::from_full("app@1/main/root").unwrap());
    let instances = product.instances([root], 8).unwrap();
    assert_eq!(
        instances
            .iter()
            .map(|i| i.definition.0.name.as_str())
            .collect::<Vec<_>>(),
        ["apply", "leaf", "root"]
    );
}

#[test]
fn generic_templates_survive_disk_reuse_and_materialize_the_call_closure() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn id[T](take x: T) -> T { x }\nfn forward[T](take x: T) -> T { id(take x) }\nfn discard[T](take x: T) {}\nfn byte() -> u8 { forward(7u8) }\nfn root() -> u64 { forward(72623859790382856u64) }\n";
    let fresh = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(
        fresh.accepted(),
        "{:?} {:?}",
        fresh.diagnostics,
        fresh.incomplete
    );
    let cached = Driver::default()
        .check(request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(cached.rechecked.is_empty());
    let root = DefPath::from_full("app@1/main/root").unwrap();
    let reached = cached.reachable(&root, 16).unwrap();
    assert!(reached.accepted(), "{:?}", reached.instances);
    let keys = reached.instances.as_ref().unwrap();
    assert_eq!(keys.len(), 3);
    for name in ["id", "forward"] {
        let path = DefPath::from_full(&format!("app@1/main/{name}")).unwrap();
        let key = reached.materialized.named(&path, &["u64"]).unwrap();
        assert!(keys.contains(&key));
        let source = cached.demand(&path).unwrap();
        assert!(source.check.template.is_some());
        let ir = reached.materialized.get(&key).ir().unwrap();
        let layouts = hirsel_core_layout::compute(&ir.types).unwrap();
        let object = hirsel_cranelift::core::emit_object_with_layouts(
            ir,
            &layouts,
            &hirsel_cranelift::core::Options {
                target: native::TARGET,
                optimize: false,
                debug_lines: false,
            },
            ContentHash::of_bytes(b"materialized test unit"),
        )
        .unwrap();
        assert!(!object.bytes.is_empty());
    }
    // T remains potentially owned in the once-checked body. Its placeholder
    // must retain cleanup even when no particular instantiation has been requested.
    let discard = cached
        .demand(&DefPath::from_full("app@1/main/discard").unwrap())
        .unwrap();
    assert!(
        discard.ir().unwrap().functions[discard.check.function.unwrap().index()]
            .body
            .as_ref()
            .unwrap()
            .insts
            .iter()
            .any(|i| matches!(i, hirsel_core_ir::Inst::Drop { .. }))
    );
    let byte = hirsel_core_ir::Identity::plain(DefPath::from_full("app@1/main/byte").unwrap());
    let roots = hirsel_core_ir::Identity::plain(root);
    assert_eq!(cached.instances([byte, roots], 16).unwrap().len(), 6);
}

#[test]
fn installed_generic_template_needs_no_source_or_body_recheck() {
    let lib = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request(
                "pub fn identity[T](take value: T) -> T { value }\n",
                lib.path(),
            ),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(image.is_some());
    let image = CoreImage::open(&lib.path().join("store")).unwrap().unwrap();
    std::fs::remove_file(lib.path().join("main.hirsel")).unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request(
        "fn root() -> u64 { lib.identity(72623859790382856u64) }\n",
        app.path(),
    );
    req.library = LibraryInput::Image(Arc::clone(&image));
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    assert_eq!(image.decoded_bodies(), 0);
    let reached = product
        .reachable(&DefPath::from_full("app@1/main/root").unwrap(), 16)
        .unwrap();
    assert!(reached.accepted(), "{:?}", reached.instances);
    assert_eq!(reached.instances.as_ref().unwrap().len(), 2);
    assert_eq!(image.decoded_bodies(), 1);
}

#[test]
fn closure_instances_include_calls_inside_the_generated_body() {
    let dir = tempfile::tempdir().unwrap();
    let product=Driver::default().check(request("fn leaf(x: u64) -> u64 {x + 1}\nfn root() -> u64 {let cb: fn(u64) -> u64 = fn(x: u64) {leaf(x)}; cb(41)}\n",dir.path()),&mut Meter::new(false)).unwrap();
    assert!(product.accepted());
    let root = hirsel_core_ir::Identity::plain(DefPath::from_full("app@1/main/root").unwrap());
    let instances = product.instances([root], 8).unwrap();
    assert_eq!(
        instances
            .iter()
            .map(|i| i.definition.0.name.as_str())
            .collect::<Vec<_>>(),
        ["leaf", "root"]
    );
}

#[test]
fn scalar_constants_survive_installed_images_and_invalidate_body_reuse() {
    let lib = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request(
                "pub const ANSWER: u64 = BASE + 2\nconst BASE: u64 = 40\n",
                lib.path(),
            ),
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(image.is_some());
    let image = CoreImage::open(&lib.path().join("store")).unwrap().unwrap();
    std::fs::remove_file(lib.path().join("main.hirsel")).unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request("fn answer() -> u64 { lib.ANSWER }\n", app.path());
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
    let body = product.definitions.values().next().unwrap();
    assert_eq!(
        eval::run(body.ir().unwrap(), body.check.function.unwrap(), &[], 1000).outcome,
        eval::Outcome::Returned(vec![42])
    );

    let local = tempfile::tempdir().unwrap();
    let before = "const N: u8 = 7\nfn answer() -> u8 { N }\n";
    Driver::default()
        .check(request(before, local.path()), &mut Meter::new(false))
        .unwrap();
    let after = Driver::default()
        .check(
            request(&before.replace("= 7", "= 9"), local.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    assert_eq!(names(&after), ["answer"]);
    let body = after.definitions.values().next().unwrap();
    assert_eq!(
        eval::run(body.ir().unwrap(), body.check.function.unwrap(), &[], 1000).outcome,
        eval::Outcome::Returned(vec![9])
    );
}

#[test]
fn unused_failed_constants_block_entry_build_acceptance_after_disk_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let source = "const OVERFLOW: u8 = 200 + 100\nfn answer() -> u8 { 42 }\n";
    let root = DefPath::from_full("app@1/main/answer").unwrap();
    for _ in 0..2 {
        let product = Driver::default()
            .check(request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        let reached = product.reachable(&root, 16).unwrap();
        assert!(!reached.accepted());
        assert!(reached.diagnostics.iter().any(|d| d.rule == "LANG-20"));
    }
}

#[test]
fn lowered_runtime_operation_needs_no_outside_body() {
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(
        "fn answer() -> u64 { task.check_cancel(); 42u64 }",
        dir.path(),
    );
    let core = dir.path().join("task.hirsel");
    std::fs::write(&core, "pub fn check_cancel()\n").unwrap();
    req.inputs.packages[0].dependencies.push(package("core"));
    req.inputs.packages.push(PackageManifest {
        package: package("core"),
        sources: vec![hirsel_core_resolve::ModuleSource {
            file: core,
            modules: vec!["task".into()],
            signatures: true,
        }],
        dependencies: vec![],
    });
    let product = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    let entry = DefPath::from_full("app@1/main/answer").unwrap();
    let reachable = product.reachable(&entry, 64).unwrap();
    assert!(reachable.accepted(), "{:?}", reachable.instances);
    let instances = reachable.instances.unwrap();
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].definition, entry);
    let definition = &product.definitions[&entry];
    assert_eq!(
        eval::run(
            definition.ir().unwrap(),
            definition.check.function.unwrap(),
            &[],
            1024
        )
        .outcome,
        eval::Outcome::Returned(vec![42])
    );
}

#[test]
fn nested_cost_views_exclude_bookkeeping_without_double_counting() {
    use hirsel_core_front::cost::{Scope, finish, start};
    let clock = std::cell::Cell::new(0);
    start(move || {
        clock.set(clock.get() + 10);
        Ok(clock.get())
    })
    .unwrap();
    {
        let _outer = Scope::enter("phase", || "body".into());
        let _construct = Scope::enter("construct", || "call".into());
        let _inner = Scope::enter("phase", || "layout".into());
    }
    let profile = finish();
    let row = |name: &str| profile.rows.iter().find(|r| r.name == name).unwrap();
    // Six event boundaries, each with ten instructions of bookkeeping and ten
    // of compiler work. Only same-view descendants reduce self cost.
    assert_eq!((row("body").inclusive, row("body").exclusive), (50, 40));
    assert_eq!((row("layout").inclusive, row("layout").exclusive), (10, 10));
    assert_eq!((row("call").inclusive, row("call").exclusive), (30, 30));
    assert_eq!(profile.excluded_instrumentation, 60);
    assert_eq!(row("body").occurrences, 1);
    assert!(profile.counter_refusal.is_none());
}

#[test]
fn aggregate_constants_in_an_image_need_no_source_or_body_decode() {
    let dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default().produce_image(library_request("pub type Point { x: u8, y: u64 }\npub const ORIGIN: Point = Point(x: 2, y: 40)\npub const TABLE: Inline[(u8,u64),2] = [(1,7),(2,11)]\n", dir.path()), &mut Meter::new(false)).unwrap();
    assert!(image.is_some());
    let opened = CoreImage::open(&dir.path().join("store")).unwrap().unwrap();
    std::fs::remove_file(dir.path().join("main.hirsel")).unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut req = request(
        "fn answer(i:u64) -> u64 { let (_, value) = lib.TABLE[i]; value + lib.ORIGIN.y }\n",
        app.path(),
    );
    req.library = LibraryInput::Image(Arc::clone(&opened));
    let checked = Driver::default()
        .check(req, &mut Meter::new(false))
        .unwrap();
    assert!(
        checked.accepted(),
        "{:?} {:?}",
        checked.diagnostics,
        checked.incomplete
    );
    let path = DefPath::from_full("app@1/main/answer").unwrap();
    let body = checked.demand_body(&path).unwrap();
    assert_eq!(
        eval::run(
            body.ir().unwrap(),
            body.check.function.unwrap(),
            &[1],
            1_000_000
        )
        .outcome,
        eval::Outcome::Returned(vec![51])
    );
    assert_eq!((opened.decoded_files(), opened.decoded_bodies()), (0, 0));
}

#[test]
fn indexed_combined_libraries_keep_existing_module_extensions() {
    let dir = tempfile::tempdir().unwrap();
    fn input(
        dir: &Path,
        name: &str,
        module: &str,
        body: &str,
        deps: Vec<PackageId>,
    ) -> PackageManifest {
        let file = dir.join(format!("{name}-{module}.hirsel"));
        std::fs::write(&file, body).unwrap();
        PackageManifest {
            package: package(name),
            dependencies: deps,
            sources: vec![hirsel_core_resolve::ModuleSource {
                file,
                modules: vec![module.into()],
                signatures: name != "app",
            }],
        }
    }
    let core = input(
        dir.path(),
        "core",
        "ops",
        "pub fn base() -> u64 { 7u64 }",
        vec![],
    );
    let std = input(
        dir.path(),
        "std",
        "ops",
        "pub fn extra() -> u64 { 11u64 }",
        vec![package("core")],
    );
    let (_, image) = Driver::default()
        .produce_image(
            CheckRequest {
                manifest: None,
                inputs: CorpusManifest {
                    core: package("core"),
                    packages: vec![core, std],
                },
                store: dir.path().join("image"),
                entry: None,
                library: LibraryInput::Sources,
            },
            &mut Meter::new(false),
        )
        .unwrap();
    let opened = CoreImage::open(&image.unwrap().store_root())
        .unwrap()
        .unwrap();
    let mut app = input(
        dir.path(),
        "app",
        "ops",
        "pub fn other() -> u64 { 13u64 }",
        vec![package("core"), package("std")],
    );
    let mut main = input(
        dir.path(),
        "app",
        "main",
        "pub fn f() -> u64 { ops.base() + ops.extra() + ops.other() }",
        vec![],
    );
    app.sources.append(&mut main.sources);
    let product = Driver::default()
        .check(
            CheckRequest {
                manifest: None,
                inputs: CorpusManifest {
                    core: package("core"),
                    packages: vec![app],
                },
                store: dir.path().join("app"),
                entry: None,
                library: LibraryInput::Image(opened),
            },
            &mut Meter::new(false),
        )
        .unwrap();
    assert!(
        product.accepted(),
        "{}",
        serde_json::to_string(&(
            product.machine_diagnostics().unwrap(),
            product.machine_incomplete().unwrap()
        ))
        .unwrap()
    );
    let reached = product
        .instances(
            [hirsel_core_ir::Identity::plain(
                DefPath::from_full("app@1/main/f").unwrap(),
            )],
            16,
        )
        .unwrap();
    let paths: std::collections::BTreeSet<_> =
        reached.iter().map(|k| k.definition.to_string()).collect();
    assert_eq!(
        paths,
        std::collections::BTreeSet::from([
            "app@1/main/f".into(),
            "app@1/ops/other".into(),
            "core@1/ops/base".into(),
            "std@1/ops/extra".into()
        ])
    );
}

#[test]
fn copied_indexed_image_reads_its_live_store() {
    let dir = tempfile::tempdir().unwrap();
    let (_, image) = Driver::default()
        .produce_image(
            library_request("pub fn answer() -> u64 { 7u64 }", dir.path()),
            &mut Meter::new(false),
        )
        .unwrap();
    let original = image.unwrap();
    let copy = |name: &str| {
        let copied = dir.path().join(name);
        std::fs::create_dir(&copied).unwrap();
        for entry in std::fs::read_dir(original.store_root()).unwrap() {
            let entry = entry.unwrap();
            if entry.path().is_file() {
                std::fs::copy(entry.path(), copied.join(entry.file_name())).unwrap();
            }
        }
        copied
    };
    let answer = DefPath::from_full("core@1/lib/answer").unwrap();
    let reads = |image: &CoreImage| {
        image.resolve_name(&answer).unwrap().is_some() && image.demand_declaration(&answer).unwrap()
    };
    let open = |root: &std::path::Path| CoreImage::open(root).unwrap().unwrap();
    // Another open of the same image, dropped or left without its store, never
    // reaches a cold open's reads.
    let (first, spare) = (open(&original.store_root()), copy("spare"));
    drop(open(&spare));
    assert!(reads(&first));
    let (first, second) = (open(&original.store_root()), open(&spare));
    std::fs::remove_dir_all(&spare).unwrap();
    assert!(reads(&first));
    drop(second);
    // And a copy reads its own store, not the one it was copied from.
    let reopened = open(&copy("copied"));
    std::fs::remove_dir_all(original.store_root()).unwrap();
    assert!(reads(&reopened));
}

#[test]
fn parser_nesting_exhaustion_keeps_its_file_site_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    let limit = hirsel_core_syntax::PARSER_NESTING_LIMIT;
    let source = format!(
        "// header\ntype T = {}u64{}",
        "List[".repeat(limit),
        "]".repeat(limit)
    );
    let at = source.find("u64").unwrap() as u32;
    let product = Driver::default()
        .check(request(&source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(!product.accepted());
    assert!(product.diagnostics.is_empty());
    assert!(product.incomplete.is_empty());
    let resource = product
        .resources
        .iter()
        .find(|r| r.message.starts_with("parser resource refusal:"))
        .unwrap();
    assert_eq!(resource.primary.item, u32::MAX);
    assert_eq!(
        resource.primary.span,
        hirsel_core_syntax::Span {
            start: at,
            end: at + 3
        }
    );
    assert!(
        resource
            .message
            .contains(&format!("nesting limit of {limit}"))
    );
}

#[test]
fn store_paths_keep_two_lowercase_hex_digits_per_digest_byte() {
    let dir = tempfile::tempdir().unwrap();
    let store = store::Store::new(dir.path()).unwrap();
    let pattern = [0, 1, 16, 127, 128, 254, 255, 170];
    let key = ContentHash(std::array::from_fn(|i| pattern[i % pattern.len()]));
    assert_eq!(
        store.path(key),
        dir.path().join("0001107f80feffaa".repeat(4))
    );
}

#[test]
fn prog4_source_std_excludes_contract_directory() {
    let dir = tempfile::tempdir().unwrap();
    let libraries = dir.path().join("libraries");
    let core = libraries.join("core");
    let std = libraries.join("std");
    let app = dir.path().join("app");
    std::fs::create_dir_all(&core).unwrap();
    std::fs::create_dir_all(std.join("tests")).unwrap();
    std::fs::create_dir_all(app.join("tests")).unwrap();
    std::fs::write(
        app.join("tests/local.hirsel"),
        "@test\nfn local() -> Result[(), ()] { Ok(()) }\n",
    )
    .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for source in PackageProduct::bundled_manifest(&root.join("core/core")).sources {
        std::fs::copy(&source.file, core.join(source.file.file_name().unwrap())).unwrap();
    }
    let library = std.join("probe.hirsel");
    std::fs::write(&library, "pub fn answer() -> u64 { 7 }\n").unwrap();
    std::fs::write(
        std.join("tests/private.hirsel"),
        "// test-of: std.probe\nfn deliberately broken(\n",
    )
    .unwrap();
    std::fs::write(
        std.join("tests/public.hirsel"),
        "fn ordinary() -> u64 { 9 }\n",
    )
    .unwrap();
    std::fs::write(
        std.join("tests/later.hirsel"),
        "// ordinary fixture\n// test-of: std.probe\nfn later() -> u64 { 11 }\n",
    )
    .unwrap();
    std::fs::write(app.join("main.hirsel"), "@test\nfn law() -> Result[(), ()] { if std.probe.answer() == 7 { Ok(()) } else { Error(()) } }\n").unwrap();
    let store = dir.path().join("store");
    for (expected_tests, mut request) in [
        CheckRequest::directory(&app, &libraries, &store).unwrap(),
        CheckRequest::libraries(&libraries, &store).unwrap(),
    ]
    .into_iter()
    .enumerate()
    {
        request.entry = None;
        let modules: Vec<_> = request.inputs.packages[1]
            .sources
            .iter()
            .map(|s| s.modules.join("/"))
            .collect();
        assert_eq!(modules, ["probe"]);
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{:?} {:?}",
            product.diagnostics,
            product.incomplete
        );
        assert_eq!(
            product.tests().count(),
            2 * usize::from(expected_tests == 0)
        );
    }
    // A marker on a library source must never hide a real source refusal.
    std::fs::write(
        &library,
        "// test-of: std.probe\npub fn answer() -> u64 { true }\n",
    )
    .unwrap();
    let mut request = CheckRequest::directory(&app, &libraries, &store).unwrap();
    request.entry = None;
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(!product.accepted());
    assert!(
        product.diagnostics.iter().any(|r| r.rule == "LANG-11"
            && product.index.module_path(r.primary.module).modules == ["probe"])
    );
}

#[test]
fn canonical_driver_encoding_visits_the_value_once() {
    struct Counted<'a>(&'a std::cell::Cell<usize>, &'a DefPath);
    impl Serialize for Counted<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.0.set(self.0.get() + 1);
            self.1.serialize(serializer)
        }
    }
    let path = DefPath(hirsel_core_ids::ItemPath {
        package: hirsel_core_ids::PackageId {
            name: "package".repeat(20),
            version: "1".into(),
        },
        modules: vec!["nested".repeat(20), "é\0".into()],
        name: "value".repeat(20),
    });
    let visits = std::cell::Cell::new(0);
    let encoded = encode(&Counted(&visits, &path)).unwrap();
    assert_eq!(encoded, bincode::serialize(&path).unwrap());
    assert_eq!(decode::<DefPath>(&encoded), Some(path));
    assert_eq!(
        visits.get(),
        1,
        "canonical encoding made a sizing traversal"
    );
}

/// Sources of a small program over four modules: a shared helper, a mutually
/// recursive pair, the edited module and an independent reader of the helper.
fn program_sources(kind: &str) -> [(&'static str, String); 4] {
    let helper = "pub fn config(x: u64) -> u64 { x }\npub fn checksum(x: u64) -> u64 { x + 1 }\npub type Settings { n: u64 }\n";
    let shapes = "pub type Even = Zero | Next(Box[Odd])\npub type Odd = One(Box[Even])\npub fn depth(e: Even) -> u64 { 0 }\npub fn top(o: Odd) -> u64 { 1 }\n";
    let main = "pub fn dispatch(n: u64) -> u64 {\n    let total = helper.config(n)\n    total\n}\npub fn main() -> u64 {\n    dispatch(helper.config(3))\n}\npub fn uses_shapes(e: shapes.Even) -> u64 { shapes.depth(e) }\n";
    let other = "pub fn other(s: helper.Settings) -> u64 { helper.config(s.n) }\npub fn more() -> u64 { helper.config(2) }\n";
    let (mut h, s, mut m, mut o) = (
        helper.to_string(),
        shapes.to_string(),
        main.to_string(),
        other.to_string(),
    );
    let d = m.find("pub fn dispatch").unwrap();
    let e = m.find("pub fn main").unwrap();
    let u = m.find("pub fn uses_shapes").unwrap();
    match kind {
        "base" => {}
        "body" => m = m.replace("    total\n", "    let _ = 0u8\n    total\n"),
        "signature" => {
            h = h.replace("config(x: u64)", "config(x: u64, _extra: u64)");
            for t in [&mut m, &mut o] {
                *t = t
                    .replace("helper.config(n)", "helper.config(n, 0)")
                    .replace("helper.config(3)", "helper.config(3, 0)")
                    .replace("helper.config(s.n)", "helper.config(s.n, 0)")
                    .replace("helper.config(2)", "helper.config(2, 0)");
            }
        }
        "add" => m.push_str("pub fn extra_item() -> u32 {\n    7\n}\n"),
        "remove" => m = format!("{}{}", &m[..e], &m[u..]),
        "import" => m = m.replace("    total\n", "    total + helper.checksum(n)\n"),
        "rename" => m = m.replace("pub fn main(", "pub fn main_renamed("),
        "reorder" => m = format!("{}{}{}", &m[e..u], &m[d..e], &m[u..]),
        "comment" => m = format!("// a comment line above dispatch\n{m}"),
        "between" => m = m.replace("}\npub fn main", "}\n\n\npub fn main"),
        "trailing" => m.push_str("\n\n"),
        "same_line_comment" => m = m.replace("}\npub fn main", "} // trailing\npub fn main"),
        "body_blank" => {
            m = m.replace(
                "    let total = helper.config(n)\n    total",
                "    let total = helper.config(n)\n\n    total",
            )
        }
        "whitespace" => m = format!("\n\n{m}"),
        "insert" => m = format!("pub fn inserted() -> u64 {{ 0u64 }}\n{m}"),
        _ => unreachable!("{kind}"),
    }
    [("helper", h), ("shapes", s), ("main", m), ("other", o)]
}
fn program_request(kind: &str, dir: &Path, store: &Path) -> CheckRequest {
    let mut req = request("", dir);
    req.store = store.to_owned();
    req.inputs.packages[0].sources.clear();
    for (name, text) in program_sources(kind) {
        let file = dir.join(format!("{name}.hirsel"));
        std::fs::write(&file, text).unwrap();
        req.inputs.packages[0]
            .sources
            .push(hirsel_core_resolve::ModuleSource {
                file,
                modules: vec![name.into()],
                signatures: false,
            });
    }
    req
}
/// §2d edit-kind gate. Law: an incremental build publishes the root of a
/// fresh build of the same sources: the same rows and file records, and no
/// row for a path the edit took away. Oracle: the fresh build.
#[test]
fn edit_kinds_publish_the_fresh_build() {
    for kind in [
        "body",
        "signature",
        "add",
        "remove",
        "import",
        "rename",
        "reorder",
        "comment",
        "between",
        "trailing",
        "same_line_comment",
        "body_blank",
        "whitespace",
        "insert",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let incremental = dir.path().join("incremental");
        let base = Driver::default()
            .check(
                program_request("base", dir.path(), &incremental),
                &mut Meter::new(false),
            )
            .unwrap();
        let request = program_request(kind, dir.path(), &incremental);
        store::WRITES.take();
        let built = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let writes = store::WRITES.take();
        let fresh = Driver::default()
            .check(
                program_request(kind, dir.path(), &dir.path().join("fresh")),
                &mut Meter::new(false),
            )
            .unwrap();
        assert_eq!(
            encode(&built.diagnostics).unwrap(),
            encode(&fresh.diagnostics).unwrap(),
            "{kind}"
        );
        let open = |store: &Path| {
            let store = Store::new(store).unwrap();
            check_index::ProgramIndex::open(&store).unwrap()
        };
        let index = open(&incremental);
        let fresh_index = open(&dir.path().join("fresh"));
        let rows = |index: &check_index::ProgramIndex| -> BTreeMap<DefPath, check_index::Row> {
            index.rows().map(|(p, r)| (p.clone(), *r)).collect()
        };
        assert_eq!(rows(&index), rows(&fresh_index), "{kind}: rows");
        assert_eq!(index.root, fresh_index.root, "{kind}: root");
        assert_eq!(built.root, fresh.root, "{kind}: product root");
        let modules: Vec<_> = program_sources(kind)
            .iter()
            .map(|(m, _)| ("app".to_string(), "1".to_string(), vec![m.to_string()]))
            .collect();
        for module in &modules {
            assert_eq!(
                index.files().into_iter().find(|(m, _)| m == module),
                fresh_index.files().into_iter().find(|(m, _)| m == module),
                "{kind}: file record {module:?}"
            );
        }
        // Hand-derived fan-out. A signature edit of `helper.config` rechecks
        // config and its four readers. Trivia before an item changes only its
        // file record, as does trivia after its last token; trivia inside an
        // item changes its key.
        let (rechecked, files): (&[&str], usize) = match kind {
            "body" | "import" | "body_blank" => (&["dispatch"], 1),
            "comment" | "between" | "trailing" | "same_line_comment" | "whitespace" => (&[], 1),
            "signature" => (&["config", "dispatch", "main", "more", "other"], 3),
            "add" => (&["extra_item"], 1),
            "insert" => (&["inserted"], 1),
            "rename" => (&["main_renamed"], 1),
            "remove" | "reorder" => (&[], 1),
            _ => unreachable!(),
        };
        assert_eq!(names(&built), rechecked, "{kind}: rechecked");
        // The paths the edit took away: their rows leave their leaves.
        let removed: Vec<DefPath> = base
            .definitions
            .keys()
            .filter(|p| !built.definitions.contains_key(*p))
            .cloned()
            .collect();
        assert_eq!(
            removed.len(),
            usize::from(matches!(kind, "remove" | "rename")),
            "{kind}: removed"
        );
        // Every write the request made. With under 48 rows the trie is the
        // root over one leaf per first nibble of H(path): each changed row
        // rewrites its leaf. Records: the rechecked bodies, the changed file
        // records, those leaves and the root; then the selection. Nothing is
        // synced (`store`).
        let leaves: std::collections::BTreeSet<u8> = built
            .rechecked
            .iter()
            .chain(&removed)
            .map(|p| {
                ContentHash::of_parts(
                    "hirsel.program-index.path.v1",
                    [&bincode::serialize(p).unwrap()[..]],
                )
                .0[0]
                    >> 4
            })
            .collect();
        let mut expected = vec![store::Durable::Record; rechecked.len() + files + leaves.len() + 1];
        expected.push(store::Durable::Selection);
        assert_eq!(writes, expected, "{kind}: writes");
    }
}

/// An envelope with `payload` and a correct length, but the trailer (the
/// checksum and address) of `old`: only the checksum tells it from the
/// record it replaces.
fn under_old_address(old: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut bytes = old[..12].to_vec();
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&old[old.len() - 40..]);
    bytes
}

/// §2g record cases. Law: a damaged record, or a record of another toolchain,
/// is a miss and a rebuild whose publication equals a fresh build's; never a
/// different product. Oracle: a fresh build of the same sources in a new store.
/// The store is never synced, so a power loss can leave any record a live
/// selection names short, zero-filled or missing: each is such a miss.
#[test]
fn damaged_and_foreign_records_rebuild_the_fresh_publication() {
    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh = Driver::default()
        .check(
            program_request("base", fresh_dir.path(), &fresh_dir.path().join("store")),
            &mut Meter::new(false),
        )
        .unwrap();
    let dispatch = DefPath::from_full("app@1/main/dispatch").unwrap();
    let main = DefPath::from_full("app@1/main/main").unwrap();
    for case in [
        "torn root",
        "torn body",
        "truncated leaf, valid envelope",
        "swapped body records",
        "corrupted body payload, valid envelope",
        "another toolchain",
        "body records under each other's rows",
        "zero-filled root",
        "zero-filled body",
        "empty body",
        "missing root",
        "missing body",
        "zero-filled selection",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store");
        if case == "another toolchain" {
            reuse::TOOLCHAIN.set(Some(ContentHash::of_bytes(b"another compiler build")));
        }
        let base = Driver::default()
            .check(
                program_request("base", dir.path(), &path),
                &mut Meter::new(false),
            )
            .unwrap();
        reuse::TOOLCHAIN.set(None);
        let store = Store::new(&path).unwrap();
        let index = check_index::ProgramIndex::open(&store).unwrap();
        let body = |p: &DefPath| store.path(index.live(p).unwrap().1);
        let mut leaf_rows = vec![];
        let expect: Vec<&str> = match case {
            "torn root" => {
                let bytes = std::fs::read(store.path(base.root)).unwrap();
                std::fs::write(store.path(base.root), &bytes[..bytes.len() / 2]).unwrap();
                names(&fresh)
            }
            "torn body" => {
                let bytes = std::fs::read(body(&dispatch)).unwrap();
                std::fs::write(body(&dispatch), &bytes[..bytes.len() / 2]).unwrap();
                vec!["dispatch"]
            }
            "truncated leaf, valid envelope" => {
                // The leaf holding dispatch's row, cut to fewer rows: a valid
                // envelope whose payload decodes, under the old address.
                let leaf = std::fs::read_dir(&path)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|f| {
                        let Some(bytes) = std::fs::read(f).ok().filter(|b| b.len() > 60) else {
                            return false;
                        };
                        let payload = &bytes[12..bytes.len() - 48];
                        // Node::Leaf is the second variant of the node encoding.
                        payload[..4] == [1, 0, 0, 0]
                            && decode::<Vec<(DefPath, check_index::Row)>>(&payload[4..])
                                .is_some_and(|rows| rows.iter().any(|r| r.0 == dispatch))
                    })
                    .unwrap();
                let bytes = std::fs::read(&leaf).unwrap();
                let payload = &bytes[12..bytes.len() - 48];
                let mut rows: Vec<(DefPath, check_index::Row)> = decode(&payload[4..]).unwrap();
                leaf_rows = rows.iter().map(|r| r.0.0.name.clone()).collect();
                leaf_rows.sort_unstable();
                rows.retain(|r| r.0 != dispatch);
                let mut cut = payload[..4].to_vec();
                cut.extend(encode(&rows).unwrap());
                std::fs::write(&leaf, under_old_address(&bytes, &cut)).unwrap();
                // Every row below a damaged node is a miss.
                leaf_rows.iter().map(String::as_str).collect()
            }
            "swapped body records" => {
                std::fs::copy(body(&main), body(&dispatch)).unwrap();
                vec!["dispatch"]
            }
            // A head byte, under the old address and lengths.
            "corrupted body payload, valid envelope" => {
                let mut bytes = std::fs::read(body(&dispatch)).unwrap();
                bytes[20] ^= 1;
                std::fs::write(body(&dispatch), bytes).unwrap();
                vec!["dispatch"]
            }
            "another toolchain" => names(&fresh),
            "zero-filled root" | "zero-filled body" | "empty body" => {
                let file = if case.ends_with("root") {
                    store.path(base.root)
                } else {
                    body(&dispatch)
                };
                let len = if case == "empty body" {
                    0
                } else {
                    std::fs::metadata(&file).unwrap().len() as usize
                };
                std::fs::write(&file, vec![0; len]).unwrap();
                if case.ends_with("root") {
                    names(&fresh)
                } else {
                    vec!["dispatch"]
                }
            }
            "missing root" => {
                std::fs::remove_file(store.path(base.root)).unwrap();
                names(&fresh)
            }
            "missing body" => {
                std::fs::remove_file(body(&dispatch)).unwrap();
                vec!["dispatch"]
            }
            "zero-filled selection" => {
                std::fs::write(path.join(check_index::SELECTION), [0; 40]).unwrap();
                names(&fresh)
            }
            // Valid body records referenced in each other's places: a body
            // record must name the path and key of the row that cites it.
            "body records under each other's rows" => {
                check_index::forge(&store, |_, leaves| {
                    let rows: Vec<_> = leaves.iter_mut().flat_map(|l| &mut l.1).collect();
                    let (mut d, mut m) = (None, None);
                    for r in rows {
                        if r.0 == dispatch {
                            d = Some(r);
                        } else if r.0 == main {
                            m = Some(r);
                        }
                    }
                    // Each row keeps its own key; only the body records it
                    // names trade places.
                    std::mem::swap(&mut d.unwrap().1.body, &mut m.unwrap().1.body);
                });
                vec!["dispatch", "main"]
            }
            _ => unreachable!(),
        };
        let rebuilt = Driver::default()
            .check(
                program_request("base", dir.path(), &path),
                &mut Meter::new(false),
            )
            .unwrap();
        assert_eq!(names(&rebuilt), expect, "{case}");
        assert_eq!(
            published(&path),
            published(&fresh_dir.path().join("store")),
            "{case}"
        );
        let again = Driver::default()
            .check(
                program_request("base", dir.path(), &path),
                &mut Meter::new(false),
            )
            .unwrap();
        assert!(
            again.rechecked.is_empty(),
            "{case}: the rebuild repaired the store"
        );
    }
}

/// FORM #109: rows are reused across instance bodies by name, which is sound only if
/// one name is one type. Before one canonical encoding a substituted `String` field
/// was `Inline(String)` in one body and `Seq(u8)` in another, so a holder's layout
/// came from whichever body published the name first. Every monomorphic definition
/// of LangArena, std and the Core corpus (M1 and M2b programs, front cases) is a root.
///
/// Every unit that carries a body defines it weak under its code identity and the
/// linker keeps one, so each code identity must name one Core IR body, anchors
/// included, across every unit of a product (`carried_law`). A callable adapter
/// anchored at the site that named its function, as before `generated`, broke it on
/// M2b's tx: `transfer$callable` differed between `bench` and `commit`.
#[test]
fn every_reached_instance_imports_its_closures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let files = corpus_files;
    let mut m1: CorpusManifest = serde_json::from_slice(
        &std::fs::read(root.join("core/programs/m1/manifest.json")).unwrap(),
    )
    .unwrap();
    for s in m1.packages.iter_mut().flat_map(|p| &mut p.sources) {
        s.file = root.join(&s.file);
    }
    m1.packages.insert(
        0,
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap(),
    );
    let langarena = langarena_inputs(&root, &root.join("core/core"), &root.join("core/langarena"));
    let dir = tempfile::tempdir().unwrap();
    let mut requests: Vec<(String, CheckRequest)> = [("langarena", langarena), ("m1", m1)]
        .into_iter()
        .map(|(name, inputs)| {
            let store = dir.path().join(name);
            (
                name.to_string(),
                CheckRequest {
                    manifest: None,
                    inputs,
                    store,
                    library: LibraryInput::Sources,
                    entry: None,
                },
            )
        })
        .collect();
    for (i, case) in files(&root.join("core/front/cases"), true)
        .into_iter()
        .enumerate()
    {
        let case_dir = dir.path().join(format!("case{i}"));
        std::fs::create_dir_all(&case_dir).unwrap();
        let source = std::fs::read_to_string(&case.file).unwrap();
        requests.push((case.file.display().to_string(), request(&source, &case_dir)));
    }
    requests.extend(m2b_requests(&root, dir.path()));
    let mut built = 0;
    for (name, request) in requests {
        let mut carried = crate::carried_law::Carried::default();
        let mut added = std::collections::HashSet::new();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let roots: Vec<_> = product
            .definitions
            .keys()
            .filter(|d| {
                product.demand_body(d).is_ok_and(|p| {
                    p.check.refusals.is_empty()
                        && p.incomplete.is_empty()
                        && p.check.function.is_some()
                        && p.check.template.is_none()
                })
            })
            .map(|d| hirsel_core_ir::Identity::plain(d.clone()))
            .collect();
        for root in roots {
            // A root whose reach includes an unfinished body is not a program.
            // Every closure a reached call names imports into its instance; a row of
            // the same name with another closure would stop the walk as an input.
            let mut set = crate::instances::InstanceSet::default();
            match product.instances_into([root], 1 << 20, &mut set) {
                Ok(reached) => {
                    built += 1;
                    for instance in reached {
                        if added.insert(instance.clone()) {
                            carried.add(set.get(&instance).ir().unwrap());
                        }
                    }
                }
                Err(InstanceStop::Incomplete { .. } | InstanceStop::OutsideReached(_)) => {}
                Err(stop) => panic!("{name}: {stop:?}"),
            }
        }
        if let Some(conflict) = carried.conflict.take() {
            panic!("{name}: {conflict}");
        }
    }
    eprintln!("roots built {built}");
}

/// Each M2b workload's program as core/tools/m2b.py builds it: core and std from
/// their installed sources, then the workload's package closure, with an entry's
/// `use` choosing a package by `id`.
fn m2b_requests(root: &Path, dir: &Path) -> Vec<(String, CheckRequest)> {
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("core/programs/m2b/manifest.json")).unwrap(),
    )
    .unwrap();
    let installed = |name: &str, dependencies: Vec<PackageId>| PackageManifest {
        package: package(name),
        sources: corpus_files(&root.join("core").join(name), false)
            .into_iter()
            .map(|s| hirsel_core_resolve::ModuleSource {
                signatures: true,
                ..s
            })
            .collect(),
        dependencies,
    };
    let packages = manifest["packages"].as_array().unwrap();
    let by_name = |name: &str| {
        packages.iter().find(|p| {
            p.get("id")
                .and_then(|i| i.as_str())
                .unwrap_or(p["package"]["name"].as_str().unwrap())
                == name
        })
    };
    let mut requests = Vec::new();
    for entry in manifest["entries"].as_array().unwrap() {
        let workload = entry["workload"].as_str().unwrap();
        let mut inputs = CorpusManifest {
            core: package("core"),
            packages: vec![
                installed("core", vec![]),
                installed("std", vec![package("core")]),
            ],
        };
        let mut pending = vec![entry["package"]["name"].as_str().unwrap().to_string()];
        let mut reached = std::collections::BTreeSet::new();
        while let Some(name) = pending.pop() {
            let name = entry
                .get("use")
                .and_then(|u| u.get(&name))
                .and_then(|n| n.as_str())
                .map_or(name, str::to_string);
            let Some(package) = by_name(&name) else {
                continue;
            };
            if !reached.insert(name) {
                continue;
            }
            for d in package["dependencies"].as_array().unwrap() {
                pending.push(d["name"].as_str().unwrap().to_string());
            }
        }
        for name in reached {
            let mut package = by_name(&name).unwrap().clone();
            package.as_object_mut().unwrap().remove("id");
            let mut package: PackageManifest = serde_json::from_value(package).unwrap();
            for source in &mut package.sources {
                source.file = root.join(&source.file);
            }
            inputs.packages.push(package);
        }
        requests.push((
            format!("m2b {workload}"),
            CheckRequest {
                manifest: None,
                inputs,
                store: dir.join(format!("m2b-{workload}")),
                library: LibraryInput::Sources,
                entry: None,
            },
        ));
    }
    requests
}

/// One module per file; `pkg/m` puts module `m` in package `pkg`, which the
/// app package depends on.
fn modules_request(files: &[(&str, &str)], dir: &Path, store: &Path) -> CheckRequest {
    let mut req = request("", dir);
    req.store = store.to_owned();
    req.inputs.packages[0].sources.clear();
    for (name, text) in files {
        let (owner, module) = name.split_once('/').unwrap_or(("app", name));
        let file = dir.join(format!("{owner}-{module}.hirsel"));
        std::fs::write(&file, text).unwrap();
        let at = match req
            .inputs
            .packages
            .iter()
            .position(|p| p.package == package(owner))
        {
            Some(at) => at,
            None => {
                req.inputs.packages.insert(
                    0,
                    PackageManifest {
                        package: package(owner),
                        sources: vec![],
                        dependencies: vec![],
                    },
                );
                let app = req.inputs.packages.len() - 1;
                req.inputs.packages[app].dependencies.push(package(owner));
                0
            }
        };
        req.inputs.packages[at]
            .sources
            .push(hirsel_core_resolve::ModuleSource {
                file,
                modules: vec![module.to_string()],
                signatures: false,
            });
    }
    req
}

#[test]
fn refused_callee_results_stop_emission_on_cold_cached_and_edited_requests() {
    for body in [
        "match broken() { Yes(n) => n, No => 0u64 }",
        "let f = broken; match f() { Yes(n) => n, No => 0u64 }",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let bad = format!(
            "type Choice = Yes(u64) | No\nfn broken() -> Choice {{ let mut bytes: u64 = 0; Yes(0u64) }}\nfn answer() -> u64 {{ {body} }}\n"
        );
        let good = bad.replace("bytes", "enc");
        for source in [&bad, &bad, &good, &good, &bad, &good] {
            let product = Driver::default()
                .check(request(source, dir.path()), &mut Meter::new(false))
                .unwrap();
            let refused = source == &bad;
            assert_eq!(product.accepted(), !refused, "{:?}", product.diagnostics);
            assert_eq!(
                product
                    .definitions
                    .values()
                    .find(|d| d.definition.0.name == "answer")
                    .unwrap()
                    .check
                    .function
                    .is_none(),
                refused
            );
            if refused {
                assert!(product.diagnostics.iter().any(|r| r.rule == "LANG-2"));
            }
        }
    }
}

#[test]
fn unresolved_closure_patterns_do_not_emit_or_use_undeclared_binders() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn nested() { let f = scoped fn(o: Option[(u64, u64)]) -> u64 { match o { Some((x, _)) => x } } }\nfn captured(x: Missing) -> u64 { match x { Some(y) => { let f = scoped fn() -> u64 { y }; f() } } }\nfn flags(a: bool) -> u64 { 0u64 }\nfn missing_args(a: List[u64], b: List[u64]) -> u64 { flags(a == b) }\nfn peek(h: Missing) -> u64 { flags(h) }\n";
    for _ in 0..2 {
        let product = Driver::default()
            .check(request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(!product.accepted());
        assert!(product.diagnostics.iter().any(|r| r.rule == "LANG-7"));
        assert!(
            product
                .definitions
                .values()
                .filter(|d| d.definition.0.name != "flags")
                .all(|d| d.check.function.is_none())
        );
    }
}

#[test]
fn unresolved_bodies_suppress_cascades_without_hiding_independent_body_errors() {
    let dir = tempfile::tempdir().unwrap();
    let source = "type Choice = Yes | No(u64)\nfn pattern(x: Choice) -> u64 { match x { Yes => 1u64, Missing(_) => 0u64 } }\nfn independent() -> u64 { true }\n";
    for attempt in 0..2 {
        let product = Driver::default()
            .check(request(source, dir.path()), &mut Meter::new(false))
            .unwrap();
        assert!(!product.accepted());
        let rules: Vec<_> = product
            .diagnostics
            .iter()
            .map(|r| r.rule.as_str())
            .collect();
        assert_eq!(
            rules,
            ["LANG-7", "LANG-11"],
            "attempt {attempt}: {:?}",
            product.diagnostics
        );
        assert!(product.incomplete.is_empty());
    }
}

/// Attack 960's resolution counterexamples 1 and 6, and attack 979's H1 both
/// ways and across a package, as edit-kind cases. Law: after an edit, an
/// incremental build reports the diagnostics, holds the products and publishes
/// the root of a fresh build of the same sources. Oracle: the fresh build. Red
/// before an unresolved body stopped recording the resolver's refusals: its
/// record kept the foreign type's old span. Red before a header's digest
/// covered its item's parse failure: a callee whose body fails to parse has an
/// unresolved header, so its caller halts, and that halted record was reused
/// once the body parsed (979 H1), or the caller's whole record once it stopped.
/// Red on a body key reused from its record when only the body's own source
/// and the resolved targets are compared (B2): a variant added to a file the
/// body does not name takes over its construction, a new candidate.
#[test]
fn resolution_edges_rebuild_like_fresh() {
    let cases: [(&str, &[(&str, &str)], &[(&str, &str)]); 7] = [
        (
            "a variant added to another file takes over a record construction",
            &[
                ("a", "type R { x: u64 }\nfn make() -> R { R(x: 1u64) }\n"),
                ("b", "fn other() -> u64 { 0 }\n"),
            ],
            &[
                ("a", "type R { x: u64 }\nfn make() -> R { R(x: 1u64) }\n"),
                ("b", "fn other() -> u64 { 0 }\npub type V = R(u64)\n"),
            ],
        ),
        (
            "a variant added elsewhere takes over a record construction",
            &[(
                "a",
                "type R { x: u64 }\nfn make() -> R { R(x: 1u64) }\nconst K: R = R(x: 1u64)\n",
            )],
            &[
                (
                    "a",
                    "type R { x: u64 }\nfn make() -> R { R(x: 1u64) }\nconst K: R = R(x: 1u64)\n",
                ),
                ("b", "pub type V = R(u64)\n"),
            ],
        ),
        (
            "whitespace moves a foreign private type a refusal names",
            &[
                ("a", "fn f(x: b.Hidden) -> u64 { 0 }\n"),
                ("b", "type Hidden {}\n"),
            ],
            &[
                ("a", "fn f(x: b.Hidden) -> u64 { 0 }\n"),
                ("b", "type     Hidden {}\n"),
            ],
        ),
        (
            "a file removed and a file added: the removed file's names leave",
            &[
                ("a", "pub fn f() -> u64 { 1 }\n"),
                ("b", "fn g() -> u64 { a.f() }\n"),
                ("c", "pub fn h() -> u64 { 2 }\n"),
            ],
            &[
                ("b", "fn g() -> u64 { a.f() }\n"),
                ("c", "pub fn h() -> u64 { 2 }\n"),
                ("d", "pub fn f() -> u64 { 3 }\n"),
            ],
        ),
        (
            "a callee body that failed to parse parses",
            &[(
                "a",
                "fn probe() -> u64 { 0u128 }\nfn main() -> u64 { probe() }\n",
            )],
            &[(
                "a",
                "fn probe() -> u64 { 1u64 }\nfn main() -> u64 { probe() }\n",
            )],
        ),
        (
            "a callee body stops parsing",
            &[(
                "a",
                "fn probe() -> u64 { 1u64 }\nfn main() -> u64 { probe() }\n",
            )],
            &[(
                "a",
                "fn probe() -> u64 { 0u128 }\nfn main() -> u64 { probe() }\n",
            )],
        ),
        (
            "a callee body in another package that failed to parse parses",
            &[
                ("lib/a", "pub fn probe() -> u64 { 0u128 }\n"),
                ("b", "fn main() -> u64 { lib.a.probe() }\n"),
            ],
            &[
                ("lib/a", "pub fn probe() -> u64 { 1u64 }\n"),
                ("b", "fn main() -> u64 { lib.a.probe() }\n"),
            ],
        ),
    ];
    for (case, base, edit) in cases {
        let dir = tempfile::tempdir().unwrap();
        let incremental = dir.path().join("incremental");
        Driver::default()
            .check(
                modules_request(base, dir.path(), &incremental),
                &mut Meter::new(false),
            )
            .unwrap();
        let built = Driver::default()
            .check(
                modules_request(edit, dir.path(), &incremental),
                &mut Meter::new(false),
            )
            .unwrap();
        let fresh = Driver::default()
            .check(
                modules_request(edit, dir.path(), &dir.path().join("fresh")),
                &mut Meter::new(false),
            )
            .unwrap();
        assert_eq!(
            encode(&built.diagnostics).unwrap(),
            encode(&fresh.diagnostics).unwrap(),
            "{case}: diagnostics"
        );
        let records = |p: &CheckProduct| -> BTreeMap<DefPath, ContentHash> {
            p.definitions
                .iter()
                .map(|(k, d)| (k.clone(), d.record))
                .collect()
        };
        assert_eq!(records(&built), records(&fresh), "{case}: products");
        assert_eq!(
            published(&incremental),
            published(&dir.path().join("fresh")),
            "{case}: publication"
        );
    }
}

/// Reuse keys include the codec makers selected during lowering and the implicit
/// codec hooks on closed rows. After an edit, the cached check must match a fresh one.
#[test]
fn program_inputs_rebuild_bodies_like_fresh() {
    const CODEC: &str = "fn c() -> std.codec.Codec[u64] { std.codec.of() }\n";
    const LIST: &str = "fn l(take xs: List[u64]) -> List[u64] { xs }\n";
    const TUPLE: &str = "fn pair() -> (u64, u64) { (1u64, 2u64) }\n";
    let libraries = tempfile::tempdir().unwrap();
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    // Copies of core and std whose std lacks one maker: `of_u64`, or `of_list`.
    let without = |maker: &str, next: char| {
        let copy = libraries.path().join(maker);
        let mut pending = vec![PathBuf::new()];
        while let Some(at) = pending.pop() {
            std::fs::create_dir_all(copy.join(&at)).unwrap();
            for top in ["core", "std"].iter().filter(|_| at.as_os_str().is_empty()) {
                pending.push(PathBuf::from(top));
            }
            if at.as_os_str().is_empty() {
                continue;
            }
            for entry in std::fs::read_dir(shipped.join(&at)).unwrap() {
                let relative = at.join(entry.unwrap().file_name());
                if shipped.join(&relative).is_dir() {
                    pending.push(relative);
                } else if relative.extension().is_some_and(|e| e == "hirsel") {
                    let text = std::fs::read_to_string(shipped.join(&relative)).unwrap();
                    let text = text.replace(
                        &format!("pub fn {maker}{next}"),
                        &format!("pub fn {maker}_renamed{next}"),
                    );
                    std::fs::write(copy.join(&relative), text).unwrap();
                } else {
                    // Test vectors are bytes, some of them deliberately not UTF-8.
                    std::fs::copy(shipped.join(&relative), copy.join(&relative)).unwrap();
                }
            }
        }
        copy
    };
    let renamed = without("of_u64", '(');
    let no_list = without("of_list", '[');
    let no_schema = without("schema_of", '[');
    let cases = [
        (
            "std's u64 maker leaves",
            (&shipped, &CODEC.to_owned()),
            (&renamed, &CODEC.to_owned()),
            false,
        ),
        (
            "std's u64 maker returns",
            (&renamed, &CODEC.to_owned()),
            (&shipped, &CODEC.to_owned()),
            true,
        ),
        (
            "std's List maker leaves",
            (&shipped, &LIST.to_owned()),
            (&no_list, &LIST.to_owned()),
            true,
        ),
        (
            "std's List maker returns",
            (&no_list, &LIST.to_owned()),
            (&shipped, &LIST.to_owned()),
            true,
        ),
        (
            "a tuple's implicit schema protocol leaves",
            (&shipped, &TUPLE.to_owned()),
            (&no_schema, &TUPLE.to_owned()),
            true,
        ),
        (
            "a tuple's implicit schema protocol returns",
            (&no_schema, &TUPLE.to_owned()),
            (&shipped, &TUPLE.to_owned()),
            true,
        ),
    ];
    for (case, base, edit, accepted) in cases {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("app");
        std::fs::create_dir(&app).unwrap();
        let check = |(root, source): (&PathBuf, &String), store: &str| {
            std::fs::write(app.join("main.hirsel"), source).unwrap();
            let mut request = CheckRequest::program(&app, root, &dir.path().join(store)).unwrap();
            request.entry = None;
            Driver::default()
                .check(request, &mut Meter::new(false))
                .unwrap()
        };
        check(base, "incremental");
        let built = check(edit, "incremental");
        let fresh = check(edit, "fresh");
        assert_eq!(
            fresh.accepted() && fresh.incomplete.is_empty(),
            accepted,
            "{case}: the fresh check: {:?} {:?}",
            fresh.diagnostics,
            fresh.incomplete
        );
        assert_eq!(
            encode(&built.diagnostics).unwrap(),
            encode(&fresh.diagnostics).unwrap(),
            "{case}: diagnostics"
        );
        assert_eq!(
            encode(&built.incomplete).unwrap(),
            encode(&fresh.incomplete).unwrap(),
            "{case}: incomplete"
        );
        let records = |p: &CheckProduct| -> BTreeMap<DefPath, ContentHash> {
            p.definitions
                .iter()
                .map(|(k, d)| (k.clone(), d.record))
                .collect()
        };
        assert_eq!(records(&built), records(&fresh), "{case}: products");
    }
}

/// The `.hirsel` files under `dir` as modules named by their relative path;
/// `tests/` modules only when `tests`. Core sources are signatures.
fn corpus_files(dir: &Path, tests: bool) -> Vec<hirsel_core_resolve::ModuleSource> {
    let mut paths = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            } else if p.extension().is_some_and(|x| x == "hirsel") {
                paths.push(p);
            }
        }
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            let rel = p.strip_prefix(dir).unwrap().with_extension("");
            let modules: Vec<String> = rel.iter().map(|m| m.to_string_lossy().into()).collect();
            (tests || modules[0] != "tests").then_some(hirsel_core_resolve::ModuleSource {
                file: p,
                modules,
                signatures: dir.ends_with("core"),
            })
        })
        .collect()
}

/// The suite program: core from `core`, std from `root`, langarena from
/// `langarena`, tests included.
fn langarena_inputs(root: &Path, core: &Path, langarena: &Path) -> CorpusManifest {
    CorpusManifest {
        core: package("core"),
        packages: vec![
            PackageManifest {
                package: package("core"),
                sources: corpus_files(core, false),
                dependencies: vec![],
            },
            PackageManifest {
                package: package("std"),
                sources: corpus_files(&root.join("core/std"), false),
                dependencies: vec![package("core")],
            },
            PackageManifest {
                package: package("langarena"),
                sources: corpus_files(langarena, true),
                dependencies: vec![package("core"), package("std")],
            },
        ],
    }
}

/// The edit-kind gate on the suite program. Law: after each edit, an
/// incremental build reports the diagnostics, holds the products and
/// publishes the fresh build's file records and live rows, tombstones the
/// paths it took away, and rechecks the fan-out hand-derived from the
/// source: an item's own text moves its body alone; a header moves every
/// body that reads it (the direct callers of `helper.checksum_str`, found by
/// reading the corpus); placement moves nothing; a clone hook moved, added or
/// removed moves the hook and every body whose signatures reach its type. Attack 960's resolution cases run here suite-shaped, under the
/// law alone, and so does a core field reorder of a type lowering builds unnamed.
#[test]
fn suite_edit_kinds_recheck_their_fan_out_and_publish_the_fresh_build() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let arena = dir.path().join("langarena");
    let core = dir.path().join("core");
    for (from, into) in [("core/langarena", &arena), ("core/core", &core)] {
        let mut pending = vec![root.join(from)];
        while let Some(d) = pending.pop() {
            let to = into.join(d.strip_prefix(root.join(from)).unwrap());
            std::fs::create_dir_all(&to).unwrap();
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    pending.push(p);
                } else {
                    std::fs::copy(&p, to.join(p.file_name().unwrap())).unwrap();
                }
            }
        }
    }
    let helper = std::fs::read_to_string(arena.join("helper.hirsel")).unwrap();
    let prelude = std::fs::read_to_string(core.join("core.hirsel")).unwrap();
    // `held.P` is a core type and `hooked` its clone hook, which core alone
    // may declare (MEM-17); `probe_copies` clones a `P`.
    let held = "pub type P { value: u64 }\npub fn make(v: u64) -> P { P(value: v) }\npub fn value(p: P) -> u64 { p.value }\n";
    let hooked = |other: &str| {
        format!("{other}clone(p: held.P) {{ Ok(held.make(held.value(p) + 1u64)) }}\n")
    };
    let (other_a, other_b) = (
        "pub fn other() -> u64 { 0u64 }\n",
        "pub fn other() -> u64 { 1u64 }\n",
    );
    let copies = "pub fn copies() -> u64 {\n    let p = held.make(7u64)\n    let copied: held.P = result.unwrap(clone(p))\n    held.value(copied)\n}\n";
    let hook_files = |a: String, b: String| -> Vec<(&'static str, Option<String>)> {
        vec![
            ("core/held.hirsel", Some(held.into())),
            ("core/hook_a.hirsel", Some(a)),
            ("core/hook_b.hirsel", Some(b)),
            ("probe_copies.hirsel", Some(copies.into())),
        ]
    };
    // A capacity refusal `bound` builds: the reader names no core type.
    let refused = "fn bounded() -> u64 { 7u64 }\npub fn refused() -> u64 {\n    let r = bound bytes 1152921504606846976u64 { bounded() }\n    match r {\n        Ok(_) => 1u64,\n        Error(c) => c.requested\n    }\n}\n";
    let edited = |from: &str, to: &str| {
        assert!(helper.contains(from), "{from}");
        helper.replacen(from, to, 1)
    };
    let spare = format!("{helper}\npub fn spare() -> u32 {{ 0u32 }}\n");
    let probe_a = "type R { x: u64 }\nfn make() -> R { R(x: 1u64) }\nconst K: R = R(x: 1u64)\n";
    let hidden_a = "fn f(x: probe_hidden.Hidden) -> u64 { 0 }\n";
    // A local binding that shadows a module function, and a local a later
    // module function shadows: the lookup rows outside the item carry both.
    let shadow = "fn f() -> u64 {\n    let g = 1u64\n    g\n}\n";
    let local = "fn h() -> u64 {\n    let y = 1u64\n    y\n}\n";
    type Files = Vec<(&'static str, Option<String>)>;
    let only = |text: String| -> Files { vec![("helper.hirsel", Some(text))] };
    let cases: Vec<(&str, Files, Files, Option<Vec<&str>>)> = vec![
        (
            "body",
            vec![],
            only(edited(
                "    checksum_bytes(string.as_bytes(s))\n",
                "    int.wrapping_add(checksum_bytes(string.as_bytes(s)), 0u32)\n",
            )),
            Some(vec!["checksum_str"]),
        ),
        // A parameter's name is no reader's input: calls are positional.
        (
            "parameter_name",
            vec![],
            only(edited(
                "pub fn checksum_str(s: String) -> u32 {\n    checksum_bytes(string.as_bytes(s))\n",
                "pub fn checksum_str(t: String) -> u32 {\n    checksum_bytes(string.as_bytes(t))\n",
            )),
            Some(vec!["checksum_str"]),
        ),
        (
            "comment",
            vec![],
            only(edited(
                "    checksum_bytes(string.as_bytes(s))\n",
                "    // the bytes' checksum\n    checksum_bytes(string.as_bytes(s))\n",
            )),
            Some(vec!["checksum_str"]),
        ),
        (
            "whitespace",
            vec![],
            only(edited(
                "checksum_bytes(string.as_bytes(s))",
                "checksum_bytes( string.as_bytes(s) )",
            )),
            Some(vec!["checksum_str"]),
        ),
        ("add", vec![], only(spare.clone()), Some(vec!["spare"])),
        (
            "remove",
            only(spare.clone()),
            only(helper.clone()),
            Some(vec![]),
        ),
        (
            "rename",
            only(spare.clone()),
            only(spare.replacen("fn spare()", "fn spared()", 1)),
            Some(vec!["spared"]),
        ),
        (
            "reorder",
            vec![],
            only(edited(
                "pub fn lcg() -> Lcg {\n    Lcg(state: 42)\n}\n\npub fn reset(mut g: Lcg) {\n    g.state = 42\n}\n",
                "pub fn reset(mut g: Lcg) {\n    g.state = 42\n}\n\npub fn lcg() -> Lcg {\n    Lcg(state: 42)\n}\n",
            )),
            Some(vec![]),
        ),
        (
            "a variant added elsewhere takes over a record construction",
            vec![("probe_record.hirsel", Some(probe_a.into()))],
            vec![
                ("probe_record.hirsel", Some(probe_a.into())),
                (
                    "probe_variant.hirsel",
                    Some("pub type V = probe_record.R(u64)\n".into()),
                ),
            ],
            None,
        ),
        (
            "a module function a local shadows is removed",
            vec![(
                "probe_shadow.hirsel",
                Some(format!("fn g() -> u64 {{ 0 }}\n{shadow}")),
            )],
            vec![("probe_shadow.hirsel", Some(shadow.into()))],
            Some(vec!["f"]),
        ),
        (
            "a module function takes the name of another item's local",
            vec![("probe_shadow.hirsel", Some(local.into()))],
            vec![(
                "probe_shadow.hirsel",
                Some(format!("{local}fn y() -> u64 {{ 0 }}\n")),
            )],
            Some(vec!["h", "y"]),
        ),
        (
            "whitespace moves a foreign private type a refusal names",
            vec![
                ("probe_user.hirsel", Some(hidden_a.into())),
                ("probe_hidden.hirsel", Some("type Hidden {}\n".into())),
            ],
            vec![
                ("probe_user.hirsel", Some(hidden_a.into())),
                ("probe_hidden.hirsel", Some("type     Hidden {}\n".into())),
            ],
            None,
        ),
        (
            "a clone hook moves to another core module",
            hook_files(hooked(other_a), other_b.into()),
            hook_files(other_a.into(), hooked(other_b)),
            Some(vec!["P.$clone", "copies", "make", "value"]),
        ),
        (
            "a clone hook is added",
            hook_files(other_a.into(), other_b.into()),
            hook_files(hooked(other_a), other_b.into()),
            Some(vec!["P.$clone", "copies", "make", "value"]),
        ),
        (
            "a clone hook is removed",
            hook_files(hooked(other_a), other_b.into()),
            hook_files(other_a.into(), other_b.into()),
            Some(vec!["copies", "make", "value"]),
        ),
        (
            "core reorders the fields of a capacity refusal a reader never names",
            vec![("probe_refused.hirsel", Some(refused.into()))],
            vec![
                ("probe_refused.hirsel", Some(refused.into())),
                (
                    "core/core.hirsel",
                    Some(prelude.replacen(
                        "    requested: u64,\n    available: u64\n",
                        "    available: u64,\n    requested: u64\n",
                        1,
                    )),
                ),
            ],
            None,
        ),
    ];
    // Inputs are copied read-only: a write replaces the file.
    let put = |path: &Path, text: &str| {
        let _ = std::fs::remove_file(path);
        std::fs::write(path, text).unwrap();
    };
    let at = |name: &str| match name.strip_prefix("core/") {
        Some(name) => core.join(name),
        None => arena.join(name),
    };
    let set = |files: &Files| {
        put(&arena.join("helper.hirsel"), &helper);
        put(&core.join("core.hirsel"), &prelude);
        for probe in [
            "probe_record",
            "probe_variant",
            "probe_user",
            "probe_hidden",
            "probe_shadow",
            "probe_copies",
            "probe_refused",
            "core/held",
            "core/hook_a",
            "core/hook_b",
        ] {
            let _ = std::fs::remove_file(at(&format!("{probe}.hirsel")));
        }
        for (name, text) in files {
            if let Some(text) = text {
                put(&at(name), text);
            }
        }
    };
    let build = |store: &Path| {
        Driver::default()
            .check(
                CheckRequest {
                    manifest: None,
                    inputs: langarena_inputs(&root, &core, &arena),
                    store: store.to_owned(),
                    library: LibraryInput::Sources,
                    entry: None,
                },
                &mut Meter::new(false),
            )
            .unwrap()
    };
    let records = |p: &CheckProduct| -> BTreeMap<DefPath, ContentHash> {
        p.definitions
            .iter()
            .map(|(k, d)| (k.clone(), d.record))
            .collect()
    };
    for (i, (case, base, edit, expect)) in cases.into_iter().enumerate() {
        let incremental = dir.path().join(format!("incremental{i}"));
        let fresh_store = dir.path().join(format!("fresh{i}"));
        set(&base);
        build(&incremental);
        set(&edit);
        let built = build(&incremental);
        let fresh = build(&fresh_store);
        assert_eq!(
            encode(&built.diagnostics).unwrap(),
            encode(&fresh.diagnostics).unwrap(),
            "{case}: diagnostics"
        );
        assert_eq!(records(&built), records(&fresh), "{case}: products");
        // The publication is the fresh one, root and all: a path the edit
        // took away has no row.
        let rows = |store: &Path| {
            let index = check_index::ProgramIndex::open(&Store::new(store).unwrap()).unwrap();
            let rows: BTreeMap<DefPath, check_index::Row> =
                index.rows().map(|(p, r)| (p.clone(), *r)).collect();
            (rows, index.root)
        };
        let files = |store: &Path| {
            check_index::ProgramIndex::open(&Store::new(store).unwrap())
                .unwrap()
                .files()
        };
        assert_eq!(
            (rows(&incremental), files(&incremental)),
            (rows(&fresh_store), files(&fresh_store)),
            "{case}: publication"
        );
        if let Some(expect) = expect {
            assert_eq!(names(&built), expect, "{case}: rechecked");
        }
    }
}

/// FORM #111: the growth arm's calls stay cold through every rebuild between the
/// front and the LLVM emitter: in a generic core body reached as a materialised
/// instance (deque push_back[u64]: grow when len == capacity) and in a published
/// std body (io write: flush when the buffer is full). A rebuild that writes
/// `cold: false` instead of copying the flag drops the fact on exactly these.
/// FORM #113 in the same program: a List[(u64, u8)] len read is ranged by the
/// element's logical size 9, floor((2^64 - 8) / 9) + 1, not by its stride 16
/// (2^60 + 1); a Bytes len (size 1) is not ranged, since a view's length may
/// exceed its capacity.
#[test]
fn growth_calls_are_cold_and_len_reads_ranged_in_emitted_ir() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(
        app.join("main.hirsel"),
        "pub fn main(out: Stdout) -> Result[(), Exit] waits {\n\
         let mut d: Deque[u64] = deque.new()\n\
         let _ = deque.push_back(mut d, 1)\n\
         let mut w = std.io.out(out, 16)\n\
         result.unwrap(std.io.write(mut w, \"x\"))\n\
         let xs: List[(u64, u8)] = list.new()\n\
         let b: Bytes = list.new()\n\
         if lens(xs, b) == 1 { return Ok(()) }\n\
         Ok(())\n\
         }\n\
         fn lens(xs: List[(u64, u8)], b: Bytes) -> u64 { list.len(xs) + list.len(b) }\n",
    )
    .unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let request = CheckRequest::program(&app, &root, &dir.path().join("store")).unwrap();
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let reached = product
        .reachable(&DefPath::from_full("app@1/main/main").unwrap(), 4096)
        .unwrap();
    let keys = reached.instances.as_ref().unwrap();
    for (definition, types) in [
        ("core@1/deque/push_back", vec!["u64"]),
        ("std@1/io/write", vec![]),
    ] {
        let key = reached
            .materialized
            .named(&DefPath::from_full(definition).unwrap(), &types)
            .unwrap();
        assert!(keys.contains(&key), "{definition} not reached: {keys:?}");
        let ir =
            hirsel_llvm_lazy::core::emit_ir(reached.materialized.get(&key).ir().unwrap()).unwrap();
        assert!(
            ir.lines()
                .any(|l| l.contains(" call ") && l.contains(" noinline cold")),
            "{definition}: no cold call\n{ir}"
        );
    }
    let key = hirsel_core_ir::Identity::plain(DefPath::from_full("app@1/main/lens").unwrap());
    let ir = hirsel_llvm_lazy::core::emit_ir(reached.materialized.get(&key).ir().unwrap()).unwrap();
    let ranges: Vec<_> = ir
        .lines()
        .filter_map(|l| l.split_once("!range "))
        .map(|(_, r)| &r[..=r.find('}').unwrap()])
        .collect();
    // Exact step grouping emits checked and prepaid copies of this load.
    // Both retain the element-size-derived length bound.
    assert_eq!(ranges, ["!{i64 0, i64 2049638230412172401}"; 2], "{ir}");
}

/// LANG-12 at `std.codec.of()`: a closed type without `codec`, and a generic body's
/// parameter that does not declare it, are refused at the call that fixes T, not
/// resolved to a maker nor left as a gap; so is a List or Map whose element is not
/// `codec`, which has a std maker (`of_list`, `of_map`) otherwise. The call is rewritten to T's codec, so a
/// rewrite placed before the bound check would accept or gap instead.
#[test]
fn codec_of_without_the_bound_is_refused_at_the_call() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    let source = "type Plain { n: u64 }\n\
                  fn closed() -> std.codec.Codec[Plain] { std.codec.of() }\n\
                  fn open[T]() -> std.codec.Codec[T] { std.codec.of() }\n\
                  fn listed() -> std.codec.Codec[List[Plain]] { std.codec.of() }\n\
                  fn mapped() -> std.codec.Codec[Map[u64, Option[Plain]]] { std.codec.of() }\n";
    std::fs::write(app.join("main.hirsel"), source).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut request = CheckRequest::program(&app, &root, &dir.path().join("store")).unwrap();
    request.entry = None;
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let mut sites: Vec<_> = product
        .machine_diagnostics()
        .unwrap()
        .into_iter()
        .map(|d| {
            (
                d.rule.to_string(),
                d.primary.span.start as usize,
                d.primary.span.end as usize,
            )
        })
        .collect();
    sites.sort();
    let calls: Vec<_> = source
        .match_indices("std.codec.of()")
        .map(|(at, call)| ("LANG-12".to_string(), at, at + call.len()))
        .collect();
    assert_eq!(sites, calls);
}

// A codec declaration's generated items repeat its refusals at text no one wrote, past the
// end of the file. The declaration's own refusals stand (a field that is not codec, at the
// field; a default of another type, at the default) and the generated ones are dropped; a
// gap only the generated items meet (`of` at a recursive type the frame loop does not take,
// a member on both sides of a Result) is reported once, at the
// authored field, with the declaration and operation as causal sites.
// Spans by hand from the source.
#[test]
fn derived_codec_refusals_point_at_the_declaration() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    let source = "type Plain { n: u64 }\n\
                  type Held: codec { a: u64, f: Plain }\n\
                  type Sum: codec = A | B(Plain)\n\
                  type Late: codec { s: i64 = \"x\" }\n\
                  type Both: codec { r: List[Result[Both, Both]] }\n\
                  type Pairs: codec { t: Both }\n\
                  fn nested() -> std.codec.Codec[List[Option[Both]]] { std.codec.of() }\n";
    std::fs::write(app.join("main.hirsel"), source).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut request = CheckRequest::program(&app, &root, &dir.path().join("store")).unwrap();
    request.entry = None;
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    let span = |text: &str| {
        let at = source.find(text).unwrap();
        (at, at + text.len())
    };
    let mut refusals: Vec<_> = product
        .machine_diagnostics()
        .unwrap()
        .into_iter()
        .map(|d| {
            let s = d.primary.span;
            (d.rule.to_string(), (s.start as usize, s.end as usize))
        })
        .collect();
    refusals.sort();
    let b = source.find("B(Plain)").unwrap() + 2;
    assert_eq!(
        refusals,
        vec![
            ("LANG-11".to_string(), span("\"x\"")),
            ("OWN-4".to_string(), span("f: Plain")),
            ("OWN-4".to_string(), (b, b + 5)),
        ]
    );
    let mut gaps: Vec<_> = product
        .machine_incomplete()
        .unwrap()
        .into_iter()
        .map(|g| {
            let s = g.primary.span;
            ((s.start as usize, s.end as usize), g.message.to_string())
        })
        .collect();
    gaps.sort();
    let gap = "the codec of this type is not built".to_string();
    assert_eq!(
        gaps,
        vec![
            (
                span("t: Both"),
                format!("deriving encode for Pairs, field/variant path t: {gap}")
            ),
            (span("std.codec.of()"), gap)
        ]
    );
}

#[test]
fn regex_rx3_search_reachability_admits_nothing() {
    use hirsel_core_ir::{Callee, Identity, Inst, RtOp};

    let directory = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let inputs = CheckRequest::libraries(&root, &directory.path().join("requests")).unwrap();
    let product = Driver::default()
        .check(inputs, &mut Meter::new(false))
        .unwrap();
    let entries = [
        "is_match",
        "find",
        "captures",
        "next",
        "next_captures",
        "count",
    ];
    let roots = entries
        .map(|name| Identity::plain(DefPath::from_full(&format!("std@1/regex/{name}")).unwrap()));
    let units = product.units(roots, 65_536).unwrap();
    for unit in units.modules().unwrap() {
        let body = unit.module.functions[unit.function.index()]
            .body
            .as_ref()
            .unwrap();
        assert!(
            body.insts
                .iter()
                .all(|i| !matches!(i, Inst::Alloc { .. } | Inst::Grow { .. })),
            "regex search can allocate in {:?}",
            unit.identity,
        );
        for call in &body.calls {
            match call.callee {
                Callee::Direct(_) | Callee::Runtime(RtOp::CompareBytes) => {}
                other => panic!(
                    "unaudited regex search callee {other:?} in {:?}",
                    unit.identity
                ),
            }
        }
    }
}
