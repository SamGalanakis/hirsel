// Request sequences (L0). Law: a request served from a store that earlier requests
// of the sequence primed answers as the same request against an empty store. It has
// the same diagnostics, incomplete and resource rows, the same check root and, for a
// build, the same executable bytes and exit. Both primed paths are held to it: one
// driver across the sequence (memory and disk) and a new driver per request (disk
// alone). Sources are edited in place, as an editor does. Oracle: the empty store,
// which no earlier request can reach. Each sequence is primed as A, A', B...: A' is
// A with a newline appended to every source outside core and std, the same shape in
// other bytes, so a store that publishes on the second request with one shape holds
// a generation before B. Cases: the programs of option-l-review-1034, the request
// sequences of l-body-review-1036 and the shapes of l-body-review-1039 (fixtures
// and RUN receipts in the corresponding review's evidence directory).

/// A source's text: literal, or a file of this checkout with one replacement
/// (`find` empty: `with` appended).
enum Text {
    Lit(&'static str),
    Repo(&'static str, &'static str, &'static str),
}
struct Src {
    modules: &'static str,
    file: String,
    signatures: bool,
    text: Text,
}
/// A package: `core` and `std` are the bundled packages, `core` with its sources
/// replacing bundled modules of the same name and the rest added. Two sources of
/// one module are both kept.
struct Pkg {
    name: &'static str,
    deps: &'static [&'static str],
    sources: Vec<Src>,
}
struct Req {
    packages: Vec<Pkg>,
    entry: Option<&'static str>,
    build: bool,
    /// The directory under the sequence's root that holds the sources.
    dir: &'static str,
    /// This request's directory is a link to the named directory.
    link: Option<&'static str>,
    /// A change to the native options of a build.
    native: Option<fn(&mut NativeOptions)>,
    /// Where the libraries come from.
    product: Product,
}
/// The libraries of a request: checked from source, or installed from a
/// package product cache kept beside the store.
#[derive(Clone, Copy, PartialEq)]
enum Product {
    Sources,
    /// Core from its product, as `check --core-product` does.
    Core,
    /// Core and std from the bundled libraries' image.
    Libraries,
}
#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    /// The selected check root's record is removed before the first request after A'.
    MissingRoot,
    /// An unpublished partial record sits in the store's temporary directory.
    TornTemp,
}
/// The gate decision OPTION-L v3.1 predicts for a sequence's last request after
/// its priming: l-body-review-1039's settled column, falling back where a module of
/// the generation before it is refused (v3.1 gate condition 4).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Gate {
    Fast,
    Fallback,
}
struct Seq {
    name: &'static str,
    steps: Vec<Req>,
    fault: Fault,
    gate: Gate,
}
fn lit(text: &'static str) -> Text {
    Text::Lit(text)
}
fn repo(path: &'static str, find: &'static str, with: &'static str) -> Text {
    Text::Repo(path, find, with)
}
fn src(modules: &'static str, file: &str, signatures: bool, text: Text) -> Src {
    Src {
        modules,
        file: file.into(),
        signatures,
        text,
    }
}
fn core(sources: Vec<Src>) -> Pkg {
    Pkg {
        name: "core",
        deps: &[],
        sources,
    }
}
fn std_pkg() -> Pkg {
    Pkg {
        name: "std",
        deps: &["core"],
        sources: vec![],
    }
}
fn pkg(name: &'static str, deps: &'static [&'static str], sources: Vec<Src>) -> Pkg {
    Pkg {
        name,
        deps,
        sources,
    }
}
fn check(packages: Vec<Pkg>) -> Req {
    Req {
        packages,
        entry: None,
        build: false,
        dir: "src",
        link: None,
        native: None,
        product: Product::Sources,
    }
}
fn build(packages: Vec<Pkg>) -> Req {
    Req {
        entry: Some("app@1/main/main"),
        build: true,
        ..check(packages)
    }
}
/// An app over core and std, both installed from the libraries' image.
fn libraries(files: &[(&'static str, &'static str, &'static str)]) -> Req {
    let mut packages = app(files);
    packages.insert(1, std_pkg());
    packages[2].deps = &["core", "std"];
    Req {
        product: Product::Libraries,
        ..check(packages)
    }
}
fn seq(name: &'static str, steps: Vec<Req>) -> Seq {
    let gate = match GATES.binary_search_by_key(&name, |g| g.0) {
        Ok(i) => GATES[i].1,
        Err(_) => panic!("{name}: no recorded gate class"),
    };
    Seq {
        name,
        steps,
        fault: Fault::None,
        gate,
    }
}
fn llvm(options: &mut NativeOptions) {
    options.backend = Backend::Llvm;
}
fn optimize(options: &mut NativeOptions) {
    options.optimize = true;
}
fn debug_lines(options: &mut NativeOptions) {
    options.debug_lines = true;
}
fn root_limit(options: &mut NativeOptions) {
    options.root_limit = 4 * 1_048_576;
}
/// `name` or `name@version`.
fn package_id(spec: &str) -> PackageId {
    match spec.split_once('@') {
        Some((name, version)) => PackageId {
            name: name.into(),
            version: version.into(),
        },
        None => package(spec),
    }
}
/// One app package over core.
fn app(files: &[(&'static str, &'static str, &'static str)]) -> Vec<Pkg> {
    vec![
        core(vec![]),
        pkg(
            "app",
            &["core"],
            files
                .iter()
                .map(|(file, modules, text)| src(modules, file, false, lit(*text)))
                .collect(),
        ),
    ]
}

/// The checkout, absolute: a request runs from its own working directory,
/// and Buck gives a relative `CARGO_MANIFEST_DIR`.
fn checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
/// Write `req`'s sources under `root`, removing the previous request's files it
/// no longer has, and return its manifest.
fn materialize(
    req: &Req,
    pad: bool,
    root: &Path,
    written: &mut BTreeSet<PathBuf>,
) -> CorpusManifest {
    let dir = root.join(req.dir);
    if let Some(target) = req.link {
        if !dir.exists() {
            std::os::unix::fs::symlink(root.join(target), &dir).unwrap();
        }
    }
    let mut now = BTreeSet::new();
    let mut packages = vec![];
    for p in &req.packages {
        let mut manifest = match p.name {
            "core" => PackageProduct::bundled_manifest(&checkout().join("core/core")),
            "std" => PackageProduct::bundled_std_manifest(&checkout().join("core/std")),
            spec => PackageManifest {
                    namespace: None,
                package: package_id(spec),
                dependencies: vec![],
                sources: vec![],
                toolchain: false,
            },
        };
        manifest.dependencies = p.deps.iter().map(|d| package_id(d)).collect();
        let bundled = manifest.sources.len();
        for s in &p.sources {
            let mut text = match s.text {
                Text::Lit(text) => text.to_owned(),
                Text::Repo(path, find, with) => {
                    let base = std::fs::read_to_string(checkout().join(path)).unwrap();
                    if find.is_empty() {
                        base + with
                    } else {
                        assert_eq!(base.matches(find).count(), 1, "{path}: one place to edit");
                        base.replace(find, with)
                    }
                }
            };
            if pad && !matches!(p.name, "core" | "std") {
                text.push('\n');
            }
            let file = dir.join(&manifest.package.name).join(&s.file);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            if std::fs::read_to_string(&file).ok().as_deref() != Some(&text) {
                std::fs::write(&file, text).unwrap();
            }
            now.insert(file.canonicalize().unwrap());
            let modules: Vec<String> = s.modules.split('/').map(str::to_owned).collect();
            let source = hirsel_core_resolve::ModuleSource {
                    foreign: None,
                    captured: None,
                file,
                modules,
                signatures: s.signatures,
            };
            match manifest.sources[..bundled]
                .iter_mut()
                .find(|b| b.modules == source.modules)
            {
                Some(bundled) => *bundled = source,
                None => manifest.sources.push(source),
            }
        }
        packages.push(manifest);
    }
    for old in written.difference(&now) {
        std::fs::remove_file(old).unwrap();
    }
    *written = now;
    CorpusManifest {
        core: package("core"),
        packages,
    }
}

/// What a request answers: its machine diagnostics, its check root and, for a
/// build, the reach's diagnostics and the executable's bytes and exit, or the
/// native stop.
#[derive(Debug, PartialEq)]
struct Answer {
    check: serde_json::Value,
    /// None when the core package product refuses before the check.
    root: Option<ContentHash>,
    native: Option<serde_json::Value>,
    /// The declared-state record's part (crate::layers::Probe).
    layer: crate::layers::Probe,
    /// Whether the request took the fast path, by the meter's
    /// `declared_installs`; None when it declared nothing.
    installed: Option<bool>,
    /// The gate's logged class (`Measurements::layer_class`); None when the
    /// request reached no front.
    class: Option<crate::meter::LayerClass>,
}
fn answer(product: &CheckProduct, req: &Req, options: Option<&NativeOptions>) -> Answer {
    let check = serde_json::json!({
        "diagnostics": product.machine_diagnostics().unwrap(),
        "incomplete": product.machine_incomplete().unwrap(),
        "resources": product.machine_resources().unwrap(),
    });
    let native = options.filter(|_| req.build).map(|options| {
        let entry = DefPath::from_full(req.entry.unwrap()).unwrap();
        let reach = product.reachable_recipe(&entry, options).unwrap();
        let refused = serde_json::json!({
            "diagnostics": product.diagnostics_for(reach.diagnostics.iter().copied()).unwrap(),
            "incomplete": product.incomplete_for(reach.incomplete.iter().copied()).unwrap(),
            "resources": product.resources_for(reach.resources.iter()).unwrap(),
            "instances": reach.instances.as_ref().err().map(ToString::to_string),
        });
        if !reach.accepted() {
            return serde_json::json!({ "refused": refused });
        }
        drop(reach);
        match product.build(&entry, options) {
            Ok(built) => {
                let status = Command::new(&built.executable).status().unwrap().code();
                let bytes = ContentHash::of_bytes(&std::fs::read(&built.executable).unwrap());
                serde_json::json!({ "reach": refused, "exit": status, "executable": format!("{bytes:?}") })
            }
            Err(stop) => serde_json::json!({ "reach": refused, "stop": format!("{stop:?}") }),
        }
    });
    Answer {
        check,
        root: Some(product.root),
        native,
        layer: Default::default(),
        installed: None,
        class: None,
    }
}
/// Answer one request from the store at `store`. A core package product that
/// is incomplete refuses the request with an error, and that error is its answer.
fn request(
    driver: &mut Driver,
    req: &Req,
    inputs: CorpusManifest,
    store: PathBuf,
    options: Option<NativeOptions>,
) -> Answer {
    driver.layers.law = true;
    let products = store.with_extension("products");
    let request = CheckRequest {
        manifest: None,
        inputs,
        store,
        library: LibraryInput::Sources,
        entry: req.entry.map(|e| DefPath::from_full(e).unwrap()),
    };
    let meter = &mut Meter::new(false);
    let product = if req.product != Product::Sources {
        let checked = if req.product == Product::Core {
            driver.check_with_core_product(request, &products, meter)
        } else {
            driver.check_with_product(request, &products, meter)
        };
        match checked {
            Ok(product) => product,
            Err(refused) => {
                return Answer {
                    check: serde_json::json!({ "product": refused.to_string() }),
                    root: None,
                    native: None,
                    layer: std::mem::take(&mut driver.layers),
                    installed: None,
                    class: meter.result.layer_class,
                };
            }
        }
    } else {
        driver.check(request, meter).unwrap()
    };
    let layer = std::mem::take(&mut driver.layers);
    Answer {
        installed: layer
            .shape
            .map(|_| meter.result.counters.declared_installs == 1),
        layer,
        class: meter.result.layer_class,
        ..answer(&product, req, options.as_ref())
    }
}
fn fault(store: &Path, fault: Fault) {
    match fault {
        Fault::None => {}
        Fault::MissingRoot => {
            let store = crate::store::Store::new(store).unwrap();
            let root = store
                .selection(crate::check_index::SELECTION)
                .unwrap()
                .unwrap();
            std::fs::remove_file(store.path(root)).unwrap();
        }
        Fault::TornTemp => {
            let tmp = store.join("tmp");
            std::fs::create_dir_all(&tmp).unwrap();
            std::fs::write(tmp.join("unpublished"), b"HCORE\0").unwrap();
        }
    }
}
/// The declared-state record's law (OPTION-L v3.2, fast path on). The gate
/// logs its class before resolve (L2). A primed request the gate logs Fast
/// installs the store's record in place of declaration, and the meter counts
/// it; one logged Fallback installs nothing; an installed program's tables
/// equal an empty store's declared ones, table by table. A last request (the
/// third request of A, A', B) logs its recorded gate class.
#[derive(Default)]
struct LayerLaw {
    /// Per table: the installed requests compared, and those unequal.
    tables: std::collections::BTreeMap<&'static str, (usize, usize)>,
    /// Last requests whose logged class disagrees with the recorded one.
    classes: Vec<String>,
    /// Requests logged Fast that did not install, or logged Fallback that
    /// installed: (sequence, step, store).
    refused: Vec<String>,
    /// Declared-state records compared byte for byte across the two stores.
    records: usize,
}
/// The declared-state record a store selects: its key and its file's bytes.
fn layer_record(store: &Path) -> Option<(ContentHash, Vec<u8>)> {
    let store = crate::store::Store::new(store).unwrap();
    let key = store.selection(crate::layers::SELECTION).unwrap()?;
    Some((key, std::fs::read(store.path(key)).unwrap()))
}
/// Run one sequence; return each step's failures of the law and, separately, the
/// steps whose roots differ while everything else agrees.
fn run_sequence(
    seq: &Seq,
    backend: Option<Backend>,
    law: &mut LayerLaw,
) -> (Vec<String>, Vec<String>) {
    let mut first_shape = None;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("checkout");
    let mut written = BTreeSet::new();
    let mut resident = Driver::default();
    let (mut failures, mut roots) = (vec![], vec![]);
    let options = |tag: &str, req: &Req| {
        backend.map(|backend| {
            let mut options = NativeOptions {
                cas: dir.path().join(format!("{tag}-cas")),
                output: dir.path().join(format!("{tag}-program")),
                ..native_options(backend, dir.path())
            };
            if let Some(change) = req.native {
                change(&mut options);
            }
            options
        })
    };
    // A, A', then the rest; the fault lands before the first request after A'.
    let steps = [(&seq.steps[0], false), (&seq.steps[0], true)]
        .into_iter()
        .chain(seq.steps[1..].iter().map(|req| (req, false)));
    for (n, (req, pad)) in steps.enumerate() {
        if n == 2 {
            fault(&dir.path().join("resident"), seq.fault);
            fault(&dir.path().join("disk"), seq.fault);
        }
        let inputs = materialize(req, pad, &root, &mut written);
        let resident_answer = request(
            &mut resident,
            req,
            inputs.clone(),
            dir.path().join("resident"),
            options("resident", req));
        let disk_answer = request(
            &mut Driver::default(),
            req,
            inputs.clone(),
            dir.path().join("disk"),
            options("disk", req));
        // The two stores saw the same requests, so a record one shape keys
        // holds the same bytes in both: its name is not its content's hash,
        // and nothing else would tell two writers' records apart.
        if let (Some(resident), Some(disk)) = (
            layer_record(&dir.path().join("resident")),
            layer_record(&dir.path().join("disk")),
        ) && resident.0 == disk.0
        {
            law.records += 1;
            if resident.1 != disk.1 {
                failures.push(format!(
                    "{} step {n}: the declared-state record {:?} has unlike bytes in the resident and disk stores",
                    seq.name, resident.0
                ));
            }
        }
        if n == 0 {
            first_shape = resident_answer.layer.shape;
            continue;
        }
        let fresh_store = dir.path().join(format!("fresh{n}"));
        let fresh = request(
            &mut Driver::default(),
            req,
            inputs,
            fresh_store,
            options(&format!("fresh{n}"), req),
                    );
        for (path, primed) in [("resident", &resident_answer), ("disk", &disk_answer)] {
            let installed = primed.installed == Some(true);
            // A request given the image declares over the image's
            // declarations whether its program reads them or not, so an
            // installed record's rows sit above the library's.
            if installed && req.product == Product::Libraries {
                let library = primed.layer.tables.iter().find(|(t, _)| *t == "library");
                if !library.is_some_and(|(_, base)| base.iter().all(|b| b != "0")) {
                    failures.push(format!(
                        "{} step {n} ({path} store): an image-backed request installed rows on library base {library:?}",
                        seq.name
                    ));
                }
            }
            if installed {
                for ((table, rows), (_, fresh_rows)) in
                    primed.layer.tables.iter().zip(&fresh.layer.tables)
                {
                    let count = law.tables.entry(table).or_default();
                    count.0 += 1;
                    if rows != fresh_rows {
                        count.1 += 1;
                        let row = (0..rows.len().max(fresh_rows.len()))
                            .find(|&i| rows.get(i) != fresh_rows.get(i))
                            .unwrap();
                        failures.push(format!(
                            "{} step {n} ({path} store): installed {table} differ at row {row}: {:?}\n  fresh {:?}",
                            seq.name, rows.get(row), fresh_rows.get(row)
                        ));
                    }
                }
            }
            if n == seq.steps.len() && path == "resident" {
                eprintln!(
                    "LAYER-SEQ {} recorded {:?} logged {:?} installed {:?} shape {} A's",
                    seq.name,
                    seq.gate,
                    primed.class,
                    primed.installed,
                    if primed.layer.shape == first_shape { "equal to" } else { "unlike" }
                );
            }
            // A request served whole from a record reaches no front.
            if let Some(class) = primed.class {
                let fast = class == crate::meter::LayerClass::Fast;
                if fast != installed {
                    law.refused.push(format!("{} step {n} ({path} store)", seq.name));
                }
                if n == seq.steps.len() && fast != (seq.gate == Gate::Fast) {
                    law.classes.push(format!(
                        "{} ({path} store): recorded {:?}, logged {class:?}, shape {} A's",
                        seq.name,
                        seq.gate,
                        if primed.layer.shape == first_shape { "equal to" } else { "unlike" }
                    ));
                }
            }
            if (&primed.check, &primed.native) != (&fresh.check, &fresh.native) {
                failures.push(format!(
                    "{} step {n} ({path} store, predicted gate {:?}): primed {:#} {:?}\n  fresh {:#} {:?}",
                    seq.name, seq.gate, primed.check, primed.native, fresh.check, fresh.native
                ));
            } else if primed.root != fresh.root {
                roots.push(format!("{} step {n} ({path} store)", seq.name));
            }
        }
    }
    (failures, roots)
}
/// Every sequence of `seqs` keeps the law; the steps whose roots differ are
/// exactly `unequal_roots` (name, step, store). Every request installs exactly
/// when its gate logs Fast.
fn run_sequences(seqs: Vec<Seq>, backend: Option<Backend>, unequal_roots: &[&str]) {
    let (mut failures, mut roots) = (vec![], vec![]);
    let mut law = LayerLaw::default();
    for seq in &seqs {
        let (f, r) = run_sequence(seq, backend, &mut law);
        failures.extend(f);
        roots.extend(r);
    }
    for (table, (compared, unequal)) in &law.tables {
        eprintln!("LAYER-LAW {table}: {compared} installed requests, {unequal} unequal");
    }
    eprintln!("LAYER-BYTES {} records compared across stores", law.records);
    for class in &law.classes {
        eprintln!("LAYER-CLASS {class}");
    }
    for refused in &law.refused {
        eprintln!("LAYER-REFUSED {refused}");
    }
    assert!(
        failures.is_empty(),
        "{} primed steps over {} sequences answer unlike an empty store:\n{}",
        failures.len(),
        seqs.len(),
        failures.join("\n")
    );
    assert_eq!(
        roots, unequal_roots,
        "steps whose check root differs from an empty store's"
    );
    assert!(
        law.classes.is_empty(),
        "last requests whose logged class disagrees with the recorded one:\n{}",
        law.classes.join("\n")
    );
    assert!(
        law.refused.is_empty(),
        "requests whose install disagrees with their logged class:\n{}",
        law.refused.join("\n")
    );
}

/// The programs of option-l-review-1034 and l-body-review-1036, two or more requests each.
#[rustfmt::skip]
fn review_sequences() -> Vec<Seq> {
    vec![
        seq("alias_cycle_added", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A=b.B"###)), src("b", "b.hirsel", false, lit(r###"pub type B=u64"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A=b.B"###)), src("b", "b.hirsel", false, lit(r###"pub type B=a.A"###))])])]),
        seq("alias_cycle_body", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A=b.B
fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"pub type B=a.A"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A=b.B
fn f()->u8{2u8}"###)), src("b", "b.hirsel", false, lit(r###"pub type B=a.A"###))])])]),
        seq("alias_target", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A=u8"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:a.A)->u8{x}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A=u16"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:a.A)->u8{x}"###))])])]),
        seq("assertion_value", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub const V:u8=1u8"###)), src("b", "b.hirsel", false, lit(r###"const _:u8=a.V+254u8"###)), src("b", "b#2.hirsel", false, lit(r###"const _:u8=a.V"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub const V:u8=2u8"###)), src("b", "b.hirsel", false, lit(r###"const _:u8=a.V+254u8"###)), src("b", "b#2.hirsel", false, lit(r###"const _:u8=a.V"###))])])]),
        seq("body_parse_refusal", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.f()}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{let =}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.f()}"###))])])]),
        seq("body_test_reference", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"@test fn law()->Result[(),u8]{Ok(())}
fn f(){}"###)), src("b", "b.hirsel", false, lit(r###"fn g(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"@test fn law()->Result[(),u8]{Ok(())}
fn f(){law()}"###)), src("b", "b.hirsel", false, lit(r###"fn g(){}"###))])])]),
        seq("bound_transitive", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A:key {x:b.B}"###)), src("b", "b.hirsel", false, lit(r###"pub type B {x:u8}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A:key {x:b.B}"###)), src("b", "b.hirsel", false, lit(r###"pub type B {x:List[u8]}"###))])])]),
        seq("clone_added", vec![
        check(vec![
            core(vec![src("held", "core_held.hirsel", false, lit(r###"pub type P { value:u64 }
pub fn make(v:u64)->P{P(value:v)}
pub fn value(p:P)->u64{p.value}"###)), src("hook_a", "core_hook_a.hirsel", false, lit(r###""###)), src("hook_b", "core_hook_b.hirsel", false, lit(r###""###))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(p:held.P){let _=clone(p)}"###))])]),
        check(vec![
            core(vec![src("held", "core_held.hirsel", false, lit(r###"pub type P { value:u64 }
pub fn make(v:u64)->P{P(value:v)}
pub fn value(p:P)->u64{p.value}"###)), src("hook_a", "core_hook_a.hirsel", false, lit(r###"clone(p:held.P){Ok(held.make(held.value(p)))}"###)), src("hook_b", "core_hook_b.hirsel", false, lit(r###""###))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(p:held.P){let _=clone(p)}"###))])])]),
        seq("clone_body", vec![
        check(vec![
            core(vec![src("deque", "core_deque.hirsel", true, repo("core/core/deque.hirsel", "", ""))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(x:Deque[u8])->Deque[u8]{clone(x)}"###))])]),
        check(vec![
            core(vec![src("deque", "core_deque.hirsel", true, repo("core/core/deque.hirsel", r###"clone[T](d: Deque[T]) {
    let count = list.len(d.ring.raw)"###, r###"clone[T](d: Deque[T]) {
    let _=0u8
    let count = list.len(d.ring.raw)"###))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(x:Deque[u8])->Deque[u8]{clone(x)}"###))])])]),
        seq("clone_winner_order", vec![
        check(vec![
            core(vec![src("held", "core_held.hirsel", false, lit(r###"pub type P { value:u64 }
pub fn make(v:u64)->P{P(value:v)}
pub fn value(p:P)->u64{p.value}"###)), src("hook_a", "core_hook_a.hirsel", false, lit(r###"clone(p:held.P){Ok(held.make(held.value(p)))}"###)), src("hook_b", "core_hook_b.hirsel", false, lit(r###"clone(p:held.P){Ok(held.make(held.value(p)))}"###))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(p:held.P){let _=clone(p)}"###))])]),
        check(vec![
            core(vec![src("hook_b", "core_hook_b.hirsel", false, lit(r###"clone(p:held.P){Ok(held.make(held.value(p)))}"###)), src("hook_a", "core_hook_a.hirsel", false, lit(r###"clone(p:held.P){Ok(held.make(held.value(p)))}"###)), src("held", "core_held.hirsel", false, lit(r###"pub type P { value:u64 }
pub fn make(v:u64)->P{P(value:v)}
pub fn value(p:P)->u64{p.value}"###))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(p:held.P){let _=clone(p)}"###))])])]),
        seq("clone_winner_removed", vec![
        check(vec![
            core(vec![src("held", "core_held.hirsel", false, lit(r###"pub type P { value:u64 }
pub fn make(v:u64)->P{P(value:v)}
pub fn value(p:P)->u64{p.value}"###)), src("hook_a", "core_hook_a.hirsel", false, lit(r###"clone(p:held.P){Ok(held.make(held.value(p)))}"###)), src("hook_b", "core_hook_b.hirsel", false, lit(r###"clone(p:held.P){Ok(held.make(held.value(p)))}"###))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(p:held.P){let _=clone(p)}"###))])]),
        check(vec![
            core(vec![src("held", "core_held.hirsel", false, lit(r###"pub type P { value:u64 }
pub fn make(v:u64)->P{P(value:v)}
pub fn value(p:P)->u64{p.value}"###)), src("hook_a", "core_hook_a.hirsel", false, lit(r###""###)), src("hook_b", "core_hook_b.hirsel", false, lit(r###"clone(p:held.P){Ok(held.make(held.value(p)))}"###))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(p:held.P){let _=clone(p)}"###))])])]),
        seq("codec_context_added", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type Value:codec {x:u8}"###))])]),
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub type Value:codec {x:u8}"###))])])]),
        seq("codec_default", vec![
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub type R:codec{x:u8=1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use(x:a.R){}"###))])]),
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub type R:codec{x:u8=2u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use(x:a.R){}"###))])])]),
        seq("codec_field_changed", vec![
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub type A:codec {x:b.B}"###)), src("b", "b.hirsel", false, lit(r###"pub type B:codec {x:u8}"###))])]),
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub type A:codec {x:b.B}"###)), src("b", "b.hirsel", false, lit(r###"pub type B {x:u8}"###))])])]),
        // A syntax refusal elsewhere in a's file skips its codec derive, so P has no
        // maker; the fix restores it, and b's reader must see it (stale-codec-1910).
        seq("codec_maker_parse_refusal", vec![
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub type P:codec{x:u64}
fn broken(){let =}"###)), src("b", "b.hirsel", false, lit(r###"fn use(){let _c:std.codec.Codec[a.P]=std.codec.of()}"###))])]),
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub type P:codec{x:u64}
fn broken(){}"###)), src("b", "b.hirsel", false, lit(r###"fn use(){let _c:std.codec.Codec[a.P]=std.codec.of()}"###))])])]),
        // a.hirsel declares T at item 1, past the one item of a#2.hirsel, a's first file
        // in source order: the declared record binds each site in its own file
        // (stale-codec-1910).
        seq("declared_second_file", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn g(){}
pub type T{x:u8}"###)), src("a", "a#2.hirsel", false, lit(r###"fn f(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn g(){}
pub type T{x:u8}"###)), src("a", "a#2.hirsel", false, lit(r###"fn f(){}"###))])])]),
        seq("comptime_absent", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"comptime fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"comptime fn f()->u8{2u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use(){}"###))])])]),
        seq("constant_call_absent", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}
pub const X:u8=f()"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.X}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{2u8}
pub const X:u8=f()"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.X}"###))])])]),
        seq("constant_call_equal", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}
pub const X:u8=f()"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.X}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{0u8+1u8}
pub const X:u8=f()"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.X}"###))])])]),
        seq("constant_call_unrelated", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}
pub const X:u8=f()"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.X}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}
pub const X:u8=f()"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.X}
fn other()->u8{2u8}"###))])])]),
        constant_call_unrelated_body_sequence(),
        seq("constant_cycle", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub const A:u8=b.B"###)), src("b", "b.hirsel", false, lit(r###"pub const B:u8=1u8"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub const A:u8=b.B"###)), src("b", "b.hirsel", false, lit(r###"pub const B:u8=a.A"###))])])]),
        seq("constant_dfs_order", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"const A:u8=b.B"###)), src("b", "b.hirsel", false, lit(r###"pub const B:u8=false"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"const A:u8=b.B"###)), src("b", "b.hirsel", false, lit(r###"pub const B:u8=false"###))])])]),
        seq("constant_projection", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type R{x:u8}
pub const V:R=R(x:1u8)"###)), src("b", "b.hirsel", false, lit(r###"const K:u8=a.V.x"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type R{x:u16}
pub const V:R=R(x:1u16)"###)), src("b", "b.hirsel", false, lit(r###"const K:u8=a.V.x"###))])])]),
        seq("constant_ready_queue", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"const A:u8=c.C + false"###)), src("b", "b.hirsel", false, lit(r###"const B:u8=c.C + false"###)), src("c", "c.hirsel", false, lit(r###"pub const C:u8=1u8"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"const A:u8=c.C + false"###)), src("b", "b.hirsel", false, lit(r###"const B:u8=c.C + false"###)), src("c", "c.hirsel", false, lit(r###"pub const C:u8=1u8"###))])])]),
        seq("constant_value", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub const V:u8=1u8"###)), src("b", "b.hirsel", false, lit(r###"fn f()->u8{a.V}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub const V:u8=2u8"###)), src("b", "b.hirsel", false, lit(r###"fn f()->u8{a.V}"###))])])]),
        seq("constructor_body", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type R { x:u64 }
fn f()->R{R(x:1u64)}"###)), src("b", "b.hirsel", false, lit(r###""###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type R { x:u64 }
fn f()->R{R(x:1u64)}"###)), src("b", "b.hirsel", false, lit(r###"pub type V = R(u64)"###))])])]),
        seq("constructor_body_fixed", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type R { x:u64 }
fn f()->R{R(x:1u64)}"###)), src("b", "b.hirsel", false, lit(r###""###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type R { x:u64 }
fn f()->R{R(x:1u64)}"###)), src("b", "b.hirsel", false, lit(r###"pub type V = R(u64)"###))])])]),
        seq("constructor_const", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type R { x:u64 }
const K:R=R(x:1u64)"###)), src("b", "b.hirsel", false, lit(r###""###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type R { x:u64 }
const K:R=R(x:1u64)"###)), src("b", "b.hirsel", false, lit(r###"pub type V = R(u64)"###))])])]),
        seq("constructor_const_fixed", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type R { x:u64 }
const K:R=R(x:1u64)"###)), src("b", "b.hirsel", false, lit(r###""###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type R { x:u64 }
const K:R=R(x:1u64)"###)), src("b", "b.hirsel", false, lit(r###"pub type V = R(u64)"###))])])]),
        seq("constructor_pattern", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:u8)->u8{match x {v=>v}}"###)), src("b", "b.hirsel", false, lit(r###""###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:u8)->u8{match x {v=>v}}"###)), src("b", "b.hirsel", false, lit(r###"pub type V = v"###))])])]),
        seq("default_argument_absent", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f(x:u8=1u8)->u8{x}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.f()}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f(x:u8=2u8)->u8{x}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.f()}"###))])])]),
        // The anchor names a parameter, so swapping the bindings' names moves
        // the result's anchor from the first argument to the second: the reader
        // that writes its first argument under the view is refused, then not.
        seq("parameter_anchor_swapped", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type P{a:u64}
pub fn touch(mut p:P){}
pub fn pick(x:P, y:P)->View[P] in x {view x}"###)), src("b", "b.hirsel", false, lit(r###"fn use(mut q:a.P, r:a.P){let v=a.pick(q, r); a.touch(mut q); let _=v}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type P{a:u64}
pub fn touch(mut p:P){}
pub fn pick(y:P, x:P)->View[P] in x {view x}"###)), src("b", "b.hirsel", false, lit(r###"fn use(mut q:a.P, r:a.P){let v=a.pick(q, r); a.touch(mut q); let _=v}"###))])])]),
        // `bad` is the last item and gains the line break that ends it when an
        // item follows: its record is reused and its refusal still renders at
        // `false`, byte 23 of the file (line 2, column 14).
        seq("trailing_line_added", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn ok(){}
fn bad()->u8{false}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn ok(){}
fn bad()->u8{false}
fn more(){}"###))])])]),
        seq("doc_trivia", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"// doc old
pub type T{x:u8}"###)), src("b", "b.hirsel", false, lit(r###"fn bad(x:a.T)->u8{false}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"// doc new longer
pub type T{x:u8}"###)), src("b", "b.hirsel", false, lit(r###"fn bad(x:a.T)->u8{false}"###))])])]),
        seq("extension_candidate", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core", "lib"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(x:List[u8])->u64{list.len(x)}"###))]),
            pkg("lib", &["core", "app"], vec![src("list", "lib_list.hirsel", false, lit(r###"pub fn value()->u8{1u8}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core", "lib"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(x:List[u8])->u64{list.len(x)}"###))]),
            pkg("lib", &["core", "app"], vec![src("list", "lib_list.hirsel", false, lit(r###"pub fn len(x:List[u8])->u64{0u64}"###))])])]),
        seq("extension_new_ambiguous", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core", "lib1", "lib2"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f()->u8{list.extra()}"###))]),
            pkg("lib1", &["core", "app", "lib2"], vec![src("list", "lib1_list.hirsel", false, lit(r###"pub fn extra()->u8{1u8}"###))]),
            pkg("lib2", &["core", "app", "lib1"], vec![src("list", "lib2_list.hirsel", false, lit(r###""###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core", "lib1", "lib2"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f()->u8{list.extra()}"###))]),
            pkg("lib1", &["core", "app", "lib2"], vec![src("list", "lib1_list.hirsel", false, lit(r###"pub fn extra()->u8{1u8}"###))]),
            pkg("lib2", &["core", "app", "lib1"], vec![src("list", "lib2_list.hirsel", false, lit(r###"pub fn extra()->u8{2u8}"###))])])]),
        seq("failure_body", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->Result[u8,u8]{Ok(1u8)}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->Result[u8,u8]{a.f()}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->Result[u8,u8]{Error(7u8)}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->Result[u8,u8]{a.f()}"###))])])]),
        seq("finalizer_body", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub resource type R{x:u8} drop(r){}"###)), src("b", "b.hirsel", false, lit(r###"fn f(take r:a.R){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub resource type R{x:u8} drop(r){let _=0u8}"###)), src("b", "b.hirsel", false, lit(r###"fn f(take r:a.R){}"###))])])]),
        seq("foreign_duplicate_span", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(){}"###)), src("a", "a#2.hirsel", false, lit(r###"fn f(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit("// shift\nfn f(){}")), src("a", "a#2.hirsel", false, lit(r###"fn f(){}"###))])])]),
        seq("foreign_private_span", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:b.Hidden){}"###)), src("b", "b.hirsel", false, lit(r###"type Hidden{}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:b.Hidden){}"###)), src("b", "b.hirsel", false, lit("// shifted\ntype Hidden{}"))])])]),
        seq("generic_body", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn twice[T:plain](x:T)->T{x}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.twice(7u8)))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn twice[T:plain](x:T)->T{let y=x; y}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.twice(7u8)))}"###))])])]),
        seq("generic_body_result", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f[T](x:T)->u8{1u8}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f(0u8)))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f[T](x:T)->u8{7u8}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f(0u8)))}"###))])])]),
        seq("generic_transitive", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f[T](x:T)->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"pub fn f[T](x:T)->u8{a.f(x)}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:b.f(0u8)))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f[T](x:T)->u8{7u8}"###)), src("b", "b.hirsel", false, lit(r###"pub fn f[T](x:T)->u8{a.f(x)}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:b.f(0u8)))}"###))])])]),
        seq("header_type_before_function", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:Inline[u8]){}"###)), src("b", "b.hirsel", false, lit(r###"type R{x:Inline[u8]}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:Inline[u8]){}"###)), src("b", "b.hirsel", false, lit(r###"type R{x:Inline[u8]}"###))])])]),
        seq("header_type_before_function_order", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:Inline[u8]){}"###)), src("b", "b.hirsel", false, lit(r###"type R{x:Inline[u8]}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("b", "b.hirsel", false, lit(r###"type R{x:Inline[u8]}"###)), src("a", "a.hirsel", false, lit(r###"fn f(x:Inline[u8]){}"###))])])]),
        seq("holds_transitive", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A {x:b.B}"###)), src("b", "b.hirsel", false, lit(r###"pub type B {x:u8}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A {x:b.B}"###)), src("b", "b.hirsel", false, lit(r###"resource type B {x:u8} drop(x){}"###))])])]),
        seq("implicit_capacity", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f()->u64{let r=bound bytes 0 {1u64}; match r{Ok(x)=>x,Error(c)=>c.requested}}"###))])]),
        check(vec![
            core(vec![src("core", "core_capacity.hirsel", true, repo("core/core/core.hirsel", r###"    requested: u64,
    available: u64"###, r###"    available: u64,
    requested: u64"###))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f()->u64{let r=bound bytes 0 {1u64}; match r{Ok(x)=>x,Error(c)=>c.requested}}"###))])])]),
        seq("index_builtin", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type Fine {}"###)), src("b", "b.hirsel", false, lit(r###""###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type Fine {}"###)), src("b", "b.hirsel", false, lit(r###"type u8 {}"###))])])]),
        seq("index_duplicate", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}"###)), src("a", "a#2.hirsel", false, lit(r###"pub fn g()->u8{2u8}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}"###)), src("a", "a#2.hirsel", false, lit(r###"pub fn f()->u8{2u8}"###))])])]),
        seq("inline_cycle", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A {x:b.B}"###)), src("b", "b.hirsel", false, lit(r###"pub type B {x:u64}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A {x:b.B}"###)), src("b", "b.hirsel", false, lit(r###"pub type B {x:a.A}"###))])])]),
        seq("keyword_module", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(){}"###)), src("if", "if.hirsel", false, lit(r###"fn g(){}"###))])])]),
        seq("lang12_direct", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type T[a]{x:Box[T[Box[a]]]}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type T[a]{x:Box[T[Box[a]]]}"###))])])]),
        seq("lang12_mutual_break", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A[t]{x:Box[b.B[Box[t]]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type B[t]{x:Box[a.A[t]]}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A[t]{x:Box[b.B[Box[t]]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type B[t]{x:t}"###))])])]),
        seq("lang12_mutual_growth", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A[t]{x:Box[b.B[Box[t]]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type B[t]{x:Box[a.A[t]]}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A[t]{x:Box[b.B[Box[t]]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type B[t]{x:Box[a.A[t]]}"###))])])]),
        seq("lang12_permutation", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A[t,u]{x:Box[b.B[u,t]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type B[t,u]{x:Box[a.A[u,t]]}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A[t,u]{x:Box[b.B[u,t]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type B[t,u]{x:Box[a.A[u,t]]}"###))])])]),
        seq("lang12_reset", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A[t]{x:Box[b.B[u8]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type B[t]{x:Box[a.A[t]]}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A[t]{x:Box[b.B[u8]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type B[t]{x:Box[a.A[t]]}"###))])])]),
        seq("lang12_unused_parameter", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type T[a]{x:b.W[T[Box[a]]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type W[a]{}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type T[a]{x:b.W[T[Box[a]]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type W[a]{}"###))])])]),
        seq("lang12_wrapper_module", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type T[a]{x:b.W[T[Box[a]]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type W[a]{x:a}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type T[a]{x:b.W[T[Box[a]]]}"###)), src("b", "b.hirsel", false, lit(r###"pub type W[a]{x:a}"###))])])]),
        seq("lang12_wrapper_own", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type T[a]{x:W[T[Box[a]]]}
type W[a]{x:a}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type T[a]{x:W[T[Box[a]]]}
type W[a]{x:a}"###))])])]),
        seq("lang12_wrapper_package", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core", "lib"], vec![src("a", "app_a.hirsel", false, lit(r###"type T[a]{x:lib.b.W[T[Box[a]]]}"###))]),
            pkg("lib", &["core", "app"], vec![src("b", "lib_b.hirsel", false, lit(r###"pub type W[a]{x:a}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core", "lib"], vec![src("a", "app_a.hirsel", false, lit(r###"type T[a]{x:lib.b.W[T[Box[a]]]}"###))]),
            pkg("lib", &["core", "app"], vec![src("b", "lib_b.hirsel", false, lit(r###"pub type W[a]{x:a}"###))])])]),
        seq("layout_embedded", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type R {x:u8}"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:a.R)->u8{x.x}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type R {x:u16}"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:a.R)->u8{x.x}"###))])])]),
        seq("layout_failure_removed", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type Good { x:u64 }"###)), src("b", "b.hirsel", false, lit(r###"type Huge { x:Inline[u64,18446744073709551615u64] }"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type Good { x:u64 }"###)), src("b", "b.hirsel", false, lit(r###""###))])])]),
        seq("layout_first_failure_order", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type Huge {x:Inline[u64,18446744073709551615u64]}"###)), src("b", "b.hirsel", false, lit(r###"type Other {x:Inline[u64,18446744073709551615u64]}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("b", "b.hirsel", false, lit(r###"type Other {x:Inline[u64,18446744073709551615u64]}"###)), src("a", "a.hirsel", false, lit(r###"type Huge {x:Inline[u64,18446744073709551615u64]}"###))])])]),
        seq("layout_global_failure", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type Good { x:u64 }"###)), src("b", "b.hirsel", false, lit(r###""###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type Good { x:u64 }"###)), src("b", "b.hirsel", false, lit(r###"type Huge { x:Inline[u64,18446744073709551615u64] }"###))])])]),
        seq("macro_absent", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"macro f() {1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"macro f() {2u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use(){}"###))])])]),
        seq("module_split", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A=b.B"###)), src("b", "b.hirsel", false, lit(r###"pub type B=a.A"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A=b.B"###)), src("b", "b.hirsel", false, lit(r###"pub type B=u8"###))])])]),
        seq("native_const", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:other.V))}"###)), src("other", "other.hirsel", false, lit(r###"pub const V:u8=1u8"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:other.V))}"###)), src("other", "other.hirsel", false, lit(r###"pub const V:u8=7u8"###))])])]),
        seq("native_foreign_body", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:other.value()))}"###)), src("other", "other.hirsel", false, lit(r###"pub fn value()->u8{1u8}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:other.value()))}"###)), src("other", "other.hirsel", false, lit(r###"pub fn value()->u8{7u8}"###))])])]),
        seq("native_reorder", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:other.value()))}"###)), src("other", "other.hirsel", false, lit(r###"pub fn value()->u8{7u8}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("other", "other.hirsel", false, lit(r###"pub fn value()->u8{7u8}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:other.value()))}"###))])])]),
        seq("negative_module_head", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f()->u8{other.value()}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f()->u8{other.value()}"###)), src("other", "other.hirsel", false, lit(r###"pub fn value()->u8{1u8}"###))])])]),
        seq("new_body_refusal", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.f()}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{false}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.f()}"###))])])]),
        seq("opaque_private_field", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub opaque type R{x:u8}"###)), src("b", "b.hirsel", false, lit(r###"type Boxed{x:a.R}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub opaque type R{x:u64}"###)), src("b", "b.hirsel", false, lit(r###"type Boxed{x:a.R}"###))])])]),
        seq("prelude_negative_type", vec![
        check(vec![
            core(vec![src("core", "core_core.hirsel", false, repo("core/core/core.hirsel", "", ""))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(x:Absent){}"###))])]),
        check(vec![
            core(vec![src("core", "core_core.hirsel", false, repo("core/core/core.hirsel", "", r###"
pub type Absent {}"###))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(x:Absent){}"###))])])]),
        seq("private_alias", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type Hidden=u8
pub type Public=Hidden"###)), src("b", "b.hirsel", false, lit(r###"fn use(x:a.Public)->u8{x}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type Hidden=u16
pub type Public=Hidden"###)), src("b", "b.hirsel", false, lit(r###"fn use(x:a.Public)->u8{x}"###))])])]),
        seq("private_function_body", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn hidden()->u8{1u8}
pub fn value()->u8{hidden()}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.value()))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn hidden()->u8{7u8}
pub fn value()->u8{hidden()}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.value()))}"###))])])]),
        seq("reexport_alias", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A=u8"###)), src("b", "b.hirsel", false, lit(r###"pub type B=a.A"###)), src("c", "c.hirsel", false, lit(r###"fn f(x:b.B)->u8{x}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A=u16"###)), src("b", "b.hirsel", false, lit(r###"pub type B=a.A"###)), src("c", "c.hirsel", false, lit(r###"fn f(x:b.B)->u8{x}"###))])])]),
        seq("repair_one_body_refusal", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn bad()->u8{false}"###)), src("b", "b.hirsel", false, lit(r###"fn bad()->u8{false}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn bad()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn bad()->u8{false}"###))])])]),
        seq("same_module_file_tie", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a#z.hirsel", false, lit(r###"pub fn f(x:Inline[u8]){}"###)), src("a", "a#a.hirsel", false, lit(r###"pub fn f(x:Inline[u16]){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a#a.hirsel", false, lit(r###"pub fn f(x:Inline[u8]){}"###)), src("a", "a#z.hirsel", false, lit(r###"pub fn f(x:Inline[u16]){}"###))])])]),
        seq("symbol_body_insertion", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn a()->u8{1u8}
fn b(x:u8)->u8{x}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn a()->u8{let z=1u8;z}
fn b(x:u8)->u8{x}"###))])])]),
        seq("test_added", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn law()->Result[(),u8]{Ok(())}"###)), src("b", "b.hirsel", false, lit(r###"fn g(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"@test fn law()->Result[(),u8]{Ok(())}"###)), src("b", "b.hirsel", false, lit(r###"fn g(){}"###))])])]),
        seq("test_body_added", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"@test fn law(){}
fn f(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"@test fn law(){}
fn f(){law()}"###))])])]),
        seq("test_header_flip", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"const K:u8=b.law()"###)), src("b", "b.hirsel", false, lit(r###"pub fn law()->u8{1u8}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"const K:u8=b.law()"###)), src("b", "b.hirsel", false, lit(r###"@test pub fn law()->u8{1u8}"###))])])]),
        seq("test_signature_before_reference_order", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(){b.law()}"###)), src("b", "b.hirsel", false, lit(r###"@test pub fn law(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(){b.law()}"###)), src("b", "b.hirsel", false, lit(r###"@test pub fn law(){}"###))])])]),
        seq("two_body_refusals_order", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn bad()->u8{false}"###)), src("b", "b.hirsel", false, lit(r###"fn bad()->u8{false}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn bad()->u8{true}"###)), src("b", "b.hirsel", false, lit(r###"fn bad()->u8{false}"###))])])]),
        seq("unchanged_body_refusal", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn bad()->u8{false}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{2u8}"###)), src("b", "b.hirsel", false, lit(r###"fn bad()->u8{false}"###))])])]),
        seq("unreachable_body_refusal", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn bad()->u8{false}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{2u8}"###)), src("b", "b.hirsel", false, lit(r###"fn bad()->u8{false}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###))])])]),
        seq("via_conversion_body", vec![
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub opaque type Email: codec via String(to: text, from: parse) {
    text: String
}

fn text(e: Email) -> View[String] in e {
    view e.text
}

fn parse(take s: String) -> Result[Email, String] {
    match string.next_scalar(s, 0) {
        Some(_) => Ok(Email(text: s)),
        None => Error(s)
    }
}

"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:a.Email){}"###))])]),
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub opaque type Email: codec via String(to: text, from: parse) {
    text: String
}

fn text(e: Email) -> View[String] in e {
    view e.text
}

fn parse(take s: String) -> Result[Email, String] {
    let _=0u8
    match string.next_scalar(s, 0) {
        Some(_) => Ok(Email(text: s)),
        None => Error(s)
    }
}

"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:a.Email){}"###))])])]),
        seq("via_representation", vec![
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub opaque type Email: codec via String(to: text, from: parse) {
    text: String
}

fn text(e: Email) -> View[String] in e {
    view e.text
}

fn parse(take s: String) -> Result[Email, String] {
    match string.next_scalar(s, 0) {
        Some(_) => Ok(Email(text: s)),
        None => Error(s)
    }
}

"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:a.Email){}"###))])]),
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub opaque type Email: codec via u64(to: text, from: parse) {
    text: String
}

fn text(e: Email) -> View[String] in e {
    view e.text
}

fn parse(take s: String) -> Result[Email, String] {
    match string.next_scalar(s, 0) {
        Some(_) => Ok(Email(text: s)),
        None => Error(s)
    }
}

"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:a.Email){}"###))])])]),
        seq("waits_body", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8 waits{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8 waits{a.f()}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8 waits{2u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8 waits{a.f()}"###))])])]),
        seq("waits_header", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.f()}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8 waits{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use()->u8{a.f()}"###))])])]),
    ]
}

/// The shapes of l-body-review-1039 that a driver request expresses, as two- and
/// three-request histories.
#[rustfmt::skip]
fn review_1039_sequences() -> Vec<Seq> {
    vec![
        seq("dependency_version", vec![
        check(vec![
            core(vec![]),
            pkg("lib", &["core", "app"], vec![src("a", "lib_a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))]),
            pkg("app", &["core", "lib"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:lib.a.f()))}"###))])]),
        check(vec![
            core(vec![]),
            pkg("lib@2", &["core", "app"], vec![src("a", "lib_a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))]),
            pkg("app", &["core", "lib@2"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:lib.a.f()))}"###))])])]),
        seq("dependency_order", vec![
        check(vec![
            core(vec![]),
            pkg("lib", &["core", "app"], vec![src("a", "lib_a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))]),
            pkg("app", &["core", "lib"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:lib.a.f()))}"###))])]),
        check(vec![
            core(vec![]),
            pkg("lib", &["core", "app"], vec![src("a", "lib_a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))]),
            pkg("app", &["lib", "core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:lib.a.f()))}"###))])])]),
        seq("core_source_edit", vec![
        check(vec![
            core(vec![src("review", "core_review.hirsel", true, lit(r###"pub fn spare()->u8{1u8}"###))]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        check(vec![
            core(vec![src("review", "core_review.hirsel", true, lit(r###"pub fn spare()->u8{2u8}"###))]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])])]),
        seq("std_dependency_added", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type R:codec{x:u8}"###))])]),
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub type R:codec{x:u8}"###))])])]),
        seq("finalizer_cross_module", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub resource type R{x:u8} drop(r){}
pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use(take r:a.R){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub resource type R{x:u8} drop(r){}
pub fn f()->u8{2u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use(take r:a.R){}"###))])])]),
        seq("codec_owner_body", vec![
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub type R:codec{x:u8=1u8}
pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use(x:a.R){}"###))])]),
        check(vec![
            core(vec![]),
            std_pkg(),
            pkg("app", &["core", "std"], vec![src("a", "a.hirsel", false, lit(r###"pub type R:codec{x:u8=1u8}
pub fn f()->u8{2u8}"###)), src("b", "b.hirsel", false, lit(r###"fn use(x:a.R){}"###))])])]),
        seq("generic_cross_clone", vec![
        check(vec![
            core(vec![src("held", "core_held.hirsel", true, lit(r###"pub type P{x:u8}
pub fn make(x:u8)->P{P(x:x)}
pub fn value(p:P)->u8{p.x}"###)), src("hook", "core_hook.hirsel", true, lit(r###"clone(p:held.P){held.make(held.value(p))}"###))]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn use[T](x:T)->u8{7u8}"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:core.held.P)->core.held.P{let _=a.use(x);clone(x)}"###))])]),
        check(vec![
            core(vec![src("held", "core_held.hirsel", true, lit(r###"pub type P{x:u8}
pub fn make(x:u8)->P{P(x:x)}
pub fn value(p:P)->u8{let x=p.x; x}"###)), src("hook", "core_hook.hirsel", true, lit(r###"clone(p:held.P){held.make(held.value(p))}"###))]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn use[T](x:T)->u8{7u8}"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:core.held.P)->core.held.P{let _=a.use(x);clone(x)}"###))])])]),
        seq("declaration_pass_order", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:Inline[u8]){}"###)), src("b", "b.hirsel", false, lit(r###"type R{x:Inline[u8]}"###)), src("c", "c.hirsel", false, lit(r###"pub fn g()->u8{1u8}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:Inline[u8]){}"###)), src("b", "b.hirsel", false, lit(r###"type R{x:Inline[u8]}"###)), src("c", "c.hirsel", false, lit(r###"pub fn g()->u8{2u8}"###))])])]),
        seq("global_summary_refusal", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type R:plain{x:u8}
fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"fn g(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"type R:plain{x:u8}
fn f()->u8{2u8}"###)), src("b", "b.hirsel", false, lit(r###"fn g(){}"###))])])]),
        seq("test_signature_reference_order", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(){b.law()}"###)), src("b", "b.hirsel", false, lit(r###"@test pub fn law(){}"###)), src("c", "c.hirsel", false, lit(r###"fn g(){}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(){b.law()}"###)), src("b", "b.hirsel", false, lit(r###"@test pub fn law(){}"###)), src("c", "c.hirsel", false, lit(r###"fn g(){let _=0u8}"###))])])]),
        seq("same_shape_refused_constant", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub const X:u8=false
fn f(){}"###)), src("b", "b.hirsel", false, lit(r###"pub const Y:u8=a.X"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub const X:u8=false
fn f(){let _=0u8}"###)), src("b", "b.hirsel", false, lit(r###"pub const Y:u8=a.X"###))])])]),
        seq("cycle_two_current", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f(n:u8)->u8{if n==0u8{7u8}else{b.g(n-1u8)}}"###)), src("b", "b.hirsel", false, lit(r###"pub fn g(n:u8)->u8{a.f(n)}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f(2u8)))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f(n:u8)->u8{if n==0u8{9u8}else{b.g(n-1u8)}}"###)), src("b", "b.hirsel", false, lit(r###"pub fn g(n:u8)->u8{let x=n; a.f(x)}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f(2u8)))}"###))])])]),
        seq("generic_instance_new_type", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn id[T:plain](x:T)->T{x}"###)), src("b", "b.hirsel", false, lit(r###"pub fn f()->u8{a.id(7u8)}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:b.f()))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn id[T:plain](x:T)->T{x}"###)), src("b", "b.hirsel", false, lit(r###"pub fn f()->u8{let _=a.id(3u64); a.id(7u8)}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:b.f()))}"###))])])]),
        seq("generic_refused_then_demanded", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn bad[T](x:T)->u8{x}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:7u8))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn bad[T](x:T)->u8{x}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.bad(true)))}"###))])])]),
        seq("constant_with_current_owner", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub const X:u8=7u8
pub fn f()->u8{X}"###)), src("b", "b.hirsel", false, lit(r###"pub const Y:u8=a.X"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:b.Y))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub const X:u8=7u8
pub fn f()->u8{let x=X; x}"###)), src("b", "b.hirsel", false, lit(r###"pub const Y:u8=a.X"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:b.Y))}"###))])])]),
        seq("constant_parts_forward", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type R{x:u8}
pub const X:R=b.Y
pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"pub const Y:a.R=a.R(x:7u8)"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.X.x))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type R{x:u8}
pub const X:R=b.Y
pub fn f()->u8{2u8}"###)), src("b", "b.hirsel", false, lit(r###"pub const Y:a.R=a.R(x:7u8)"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.X.x))}"###))])])]),
        seq("referenced_type_current_owner", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A{x:u8}
pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"pub type B{x:a.A}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A{x:u8}
pub fn f()->u8{7u8}"###)), src("b", "b.hirsel", false, lit(r###"pub type B{x:a.A}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###))])])]),
        seq("decl_count_fallback", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub type R{x:u8}
pub fn f()->u8{7u8}"###))])])]),
        seq("code_identity_body", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"pub fn named[F](s:StaticFn[F],want:Bytes)->bool{code.identity(s)==want}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{let _=b.named(code.describe(a.f),code.identity(code.describe(a.f)));Error(Exit(status:a.f()))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###)), src("b", "b.hirsel", false, lit(r###"pub fn named[F](s:StaticFn[F],want:Bytes)->bool{code.identity(s)==want}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{let _=b.named(code.describe(a.f),code.identity(code.describe(a.f)));Error(Exit(status:a.f()))}"###))])])]),
        seq("reach_to_full", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###)), src("z", "z.hirsel", false, lit(r###"fn bad()->u8{false}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###)), src("z", "z.hirsel", false, lit(r###"fn bad()->u8{false}"###))])])]),
        seq("full_to_reach", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###)), src("z", "z.hirsel", false, lit(r###"fn bad()->u8{false}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###)), src("z", "z.hirsel", false, lit(r###"fn bad()->u8{false}"###))])])]),
        seq("entry_warning_interleave", vec![
        Req { entry: Some("app@1/a/main"), ..check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn main()->Result[(),Exit] waits{Ok(())}"###)), src("b", "b.hirsel", false, lit(r###"pub fn other()->Result[(),Exit]{Ok(())}"###))])]) },
        Req { entry: Some("app@1/b/other"), ..check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn main()->Result[(),Exit] waits{Ok(())}"###)), src("b", "b.hirsel", false, lit(r###"pub fn other()->Result[(),Exit]{Ok(())}"###))])]) },
        Req { entry: Some("app@1/a/main"), ..check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn main()->Result[(),Exit] waits{Ok(())}"###)), src("b", "b.hirsel", false, lit(r###"pub fn other()->Result[(),Exit]{Ok(())}"###))])]) }]),
        seq("publish_body_third", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{9u8}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{9u8}"###))])])]),
        seq("publish_refused_second", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{false}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])])]),
        Seq { fault: Fault::TornTemp, ..seq("publish_uncommitted_second", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{9u8}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{9u8}"###))])])]) },
        seq("publish_check_build_check", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])])]),
        seq("backend_llvm", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        Req { native: Some(llvm), ..build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]) }]),
        seq("optimization", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        Req { native: Some(optimize), ..build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]) }]),
        seq("debug_lines", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        Req { native: Some(debug_lines), ..build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]) }]),
        seq("root_limit", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]),
        Req { native: Some(root_limit), ..build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###)), src("a", "a.hirsel", false, lit(r###"pub fn f()->u8{7u8}"###))])]) }]),
    ]
}

/// Sequences over the request tuple, faults, entry policy and a module ring
/// (l-body-review-1036 tuple_state.py and entry_policy.py).
fn tuple_sequences() -> Vec<Seq> {
    const MAIN0: &str = "pub fn main()->Result[(),Exit]{Ok(())}";
    const MAIN1: &str = "pub fn main()->Result[(),Exit]{Error(Exit(status:other.V))}";
    let relocated = |dir| Req {
        dir,
        ..check(app(&[
            ("a.hirsel", "a", "pub fn f()->u8{1u8}"),
            ("b.hirsel", "b", "fn g()->u8{a.f()}"),
        ]))
    };
    let edited = |v: &'static str| {
        check(app(&[
            ("a.hirsel", "a", v),
            ("b.hirsel", "b", "fn g()->u8{a.f()}"),
        ]))
    };
    let ring = |edit: bool| {
        let sources = (0..25)
            .map(|m| {
                let next = (m + 1) % 25;
                let body = if m == 0 && edit {
                    "{let _=0u8; if"
                } else {
                    "{if"
                };
                let text: &'static str = format!(
                    "pub fn f(n:u64)->u64{body} n==0u64 {{0u64}} else {{m{next}.f(n-1u64)}}}}"
                )
                .leak();
                src(
                    format!("m{m}").leak(),
                    &format!("m{m}.hirsel"),
                    false,
                    lit(text),
                )
            })
            .collect();
        check(vec![core(vec![]), pkg("app", &["core"], sources)])
    };
    // Reversing two files changes the order of declaration rows in the image.
    let ordering = |label: &str, names: [usize; 2]| {
        let texts = ["pub type A{n:u64}", "pub type B{m:u32}"];
        let sources = texts
            .iter()
            .zip(names)
            .map(|(text, name)| {
                src(
                    "ordering",
                    &format!("ordering_{label}{name}.hirsel"),
                    false,
                    lit(*text),
                )
            })
            .collect();
        Req {
            product: Product::Core,
            ..check(vec![
                core(sources),
                pkg(
                    "app",
                    &["core"],
                    vec![src("main", "main.hirsel", false, lit(MAIN0))],
                ),
            ])
        }
    };
    vec![
        // Package reuse must publish the same root as a fresh check in either order.
        seq(
            "product_order",
            vec![
                ordering("a", [1, 0]),
                ordering("b", [0, 1]),
            ],
        ),
        seq(
            "checkout_relocated",
            vec![relocated("src"), relocated("moved")],
        ),
        seq(
            "symlink_directory",
            vec![
                relocated("src"),
                Req {
                    link: Some("src"),
                    ..relocated("alias")
                },
            ],
        ),
        seq(
            "manifest_permuted",
            vec![
                check(app(&[
                    ("a.hirsel", "a", "pub fn f()->u8{1u8}"),
                    ("b.hirsel", "b", "fn g()->u8{a.f()}"),
                ])),
                check(app(&[
                    ("b.hirsel", "b", "fn g()->u8{a.f()}"),
                    ("a.hirsel", "a", "pub fn f()->u8{1u8}"),
                ])),
            ],
        ),
        seq(
            "source_moves_module",
            vec![
                check(app(&[
                    ("x.hirsel", "a", "pub fn f()->u8{1u8}"),
                    ("b.hirsel", "b", "fn g()->u8{a.f()}"),
                ])),
                check(app(&[
                    ("y.hirsel", "a", "pub fn f()->u8{1u8}"),
                    ("b.hirsel", "b", "fn g()->u8{a.f()}"),
                ])),
            ],
        ),
        seq(
            "parse_mode_changed",
            vec![
                check(vec![
                    core(vec![]),
                    pkg(
                        "app",
                        &["core"],
                        vec![src("a", "a.hirsel", false, lit("pub fn f()->u8"))],
                    ),
                ]),
                check(vec![
                    core(vec![]),
                    pkg(
                        "app",
                        &["core"],
                        vec![src("a", "a.hirsel", true, lit("pub fn f()->u8"))],
                    ),
                ]),
            ],
        ),
        seq(
            "check_to_build",
            vec![
                check(app(&[
                    ("main.hirsel", "main", MAIN1),
                    ("other.hirsel", "other", "pub const V:u8=1u8"),
                ])),
                build(app(&[
                    ("main.hirsel", "main", MAIN1),
                    ("other.hirsel", "other", "pub const V:u8=1u8"),
                ])),
            ],
        ),
        Seq {
            fault: Fault::MissingRoot,
            ..seq(
                "root_missing",
                vec![edited("pub fn f()->u8{1u8}"), edited("pub fn f()->u8{2u8}")],
            )
        },
        Seq {
            fault: Fault::TornTemp,
            ..seq(
                "unpublished_temp",
                vec![edited("pub fn f()->u8{1u8}"), edited("pub fn f()->u8{2u8}")],
            )
        },
        seq(
            "header_alternates",
            vec![
                edited("pub fn f()->u8{1u8}"),
                edited("pub fn f()->u16{1u16}"),
                edited("pub fn f()->u8{1u8}"),
            ],
        ),
        seq("ring", vec![ring(false), ring(true)]),
        // The copies of a duplicate definition share one name and one key.
        seq(
            "duplicate_copy_body",
            vec![
                check(app(&[
                    ("a.hirsel", "a", "pub fn f()->u8{1u8}"),
                    ("a#2.hirsel", "a", "pub fn f()->u8{2u8}"),
                ])),
                check(app(&[
                    ("a.hirsel", "a", "pub fn f()->u8{1u8}"),
                    ("a#2.hirsel", "a", "pub fn f()->u8{1u16}"),
                ])),
                check(app(&[
                    ("a.hirsel", "a", "pub fn f()->u8{1u16}"),
                    ("a#2.hirsel", "a", "pub fn f()->u8{1u8}"),
                ])),
            ],
        ),
    ]
}
fn sequences() -> Vec<Seq> {
    let mut seqs = review_sequences();
    seqs.extend(review_1039_sequences());
    seqs.extend(tuple_sequences());
    seqs.extend(layer_sequences());
    seqs
}

/// Steps whose check root differs from an empty store's while their answers
/// agree, with the cause.
const UNEQUAL_ROOTS: &[&str] = &[];

include!("request_sequence_gates.rs");
include!("request_sequence_comptime.rs");

include!("request_sequence_laws.rs");

enum Edit {
    /// Whitespace inside an item rechecks it: its record's sites and its IR
    /// instructions' anchors are byte offsets from the item's first token.
    Trivia(&'static str),
    Body(&'static str),
    HeaderSameType(&'static str),
    HeaderNewType {
        body: &'static str,
        readers: &'static [&'static str],
    },
    Add(&'static str),
    Delete,
    Rename(&'static str),
    Move,
}

impl Edit {
    fn name(&self) -> &'static str {
        match self {
            Self::Trivia(_) => "trivia",
            Self::Body(_) => "body",
            Self::HeaderSameType(_) => "header-same-type",
            Self::HeaderNewType { .. } => "header-new-type",
            Self::Add(_) => "add",
            Self::Delete => "delete",
            Self::Rename(_) => "rename",
            Self::Move => "move",
        }
    }

    fn violation(&self, checked: &CheckProduct) -> Option<String> {
        let actual = checked.rechecked_paths();
        let path = |name| DefPath::from_full(name).unwrap();
        let expected = match self {
            Self::Delete | Self::Move => BTreeSet::new(),
            Self::Trivia(body)
            | Self::Body(body)
            | Self::HeaderSameType(body)
            | Self::Add(body)
            | Self::Rename(body) => BTreeSet::from([path(body)]),
            Self::HeaderNewType { body, readers } => {
                readers.iter().copied().chain([*body]).map(path).collect()
            }
        };
        let matches = if matches!(self, Self::HeaderNewType { .. }) {
            expected.iter().all(|path| actual.contains(path))
        } else {
            expected.iter().eq(actual)
        };
        (!matches).then(|| format!("expected {expected:?}, rechecked {:?}", checked.rechecked))
    }
}

#[test]
fn request_sequences_recheck_edit_paths() {
    const VALUE: &str = "app@1/api/value";
    const READERS: &[&str] = &["app@1/read/first", "app@1/read/second"];
    const BASE: &str = "pub fn value(x: u8) -> u8 { 1u8 }\nfn old() {}";
    const TRIVIA: &str = "pub fn value(x: u8) -> u8 {    1u8 }\nfn old() {}";
    const BODY: &str = "pub fn value(x: u8) -> u8 {    2u8 }\nfn old() {}";
    const SAME_TYPE: &str = "pub fn value(y: u8) -> u8 {    2u8 }\nfn old() {}";
    const NEW_TYPE: &str = "pub fn value(y: u8) -> u64 {    2u64 }\nfn old() {}";
    const ADDED: &str = "pub fn value(y: u8) -> u64 {    2u64 }\nfn old() {}\nfn extra() {}";
    const RENAMED: &str = "pub fn value(y: u8) -> u64 {    2u64 }\nfn new() {}";
    let edits = [
        (Edit::Trivia(VALUE), "api.hirsel", TRIVIA),
        (Edit::Body(VALUE), "api.hirsel", BODY),
        (Edit::HeaderSameType(VALUE), "api.hirsel", SAME_TYPE),
        (
            Edit::HeaderNewType {
                body: VALUE,
                readers: READERS,
            },
            "api.hirsel",
            NEW_TYPE,
        ),
        (Edit::Add("app@1/api/extra"), "api.hirsel", ADDED),
        (Edit::Delete, "api.hirsel", NEW_TYPE),
        (Edit::Rename("app@1/api/new"), "api.hirsel", RENAMED),
        (Edit::Move, "moved.hirsel", RENAMED),
    ];
    let with = |spare: bool, file, text| {
        let mut files = vec![
            (file, "api", text),
            (
                "read.hirsel",
                "read",
                "fn first() { let _ = api.value(0u8) }\nfn second() { let _ = api.value(1u8) }",
            ),
            ("spare.hirsel", "spare", "fn value() -> u8 { 9u8 }"),
        ];
        files.truncate(if spare { 3 } else { 2 });
        check(app(&files))
    };
    let req = |file, text| with(true, file, text);
    // Before the body edit, a request without `spare` publishes into the same
    // store, as `hirsel run` leaves the test sources out: the edit still
    // rechecks only the edited body, not `spare` that request did not hold.
    let steps = edits.iter().flat_map(|(edit, file, text)| {
        let other = matches!(edit, Edit::Body(_)).then(|| (None, with(false, "api.hirsel", TRIVIA)));
        other.into_iter().chain([(Some(edit), req(file, text))])
    });
    let dir = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    let mut written = BTreeSet::new();
    let mut resident = Driver::default();
    for (edit, req) in std::iter::once((None, req("api.hirsel", BASE))).chain(steps) {
        let inputs = materialize(&req, false, dir.path(), &mut written);
        for (store, driver) in [
            ("resident", &mut resident),
            ("disk", &mut Driver::default()),
        ] {
            let checked = driver
                .check(
                    CheckRequest {
                        manifest: None,
                        inputs: inputs.clone(),
                        store: dir.path().join(store),
                        library: LibraryInput::Sources,
                        entry: None,
                    },
                    &mut Meter::new(false),
                )
                .unwrap();
            assert!(
                checked.accepted(),
                "{store}: {}",
                serde_json::json!({
                    "diagnostics": checked.machine_diagnostics().unwrap(),
                    "incomplete": checked.machine_incomplete().unwrap(),
                    "resources": checked.machine_resources().unwrap(),
                })
            );
            if let Some(edit) = edit
                && let Some(violation) = edit.violation(&checked)
            {
                failures.push(format!("{} ({store}): {violation}", edit.name()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "explicit native integration target"]
fn request_sequences_build_as_an_empty_store() {
    let natives = sequences()
        .into_iter()
        .filter(|s| s.steps.iter().any(|r| r.build))
        .collect();
    run_sequences(natives, Some(Backend::Cranelift), &[]);
}
include!("source_std_native.rs");

// The process leg (S3; STEP-C 2.1 replay law). A request is the `hirsel`
// executable's: the sequence cases run the product driver binary, so the
// memory-limit cases measure it rather than libtest startup. Its stdout,
// stderr, exit status and output file are compared with the same request
// against an empty store. Work counters are not answers and are left out
// (REPORT D2): the text line's checked-definitions count. Each step also
// says whether the request record must answer it, read from its `--metrics`
// stages, so a case cannot pass by never hitting.
pub(crate) mod process {
use super::*;

/// What a step's request is: its command, its app sources, the checkout it
/// runs in, and the request's own arguments and environment.
#[derive(Clone)]
struct Step {
    command: &'static str,
    /// (file, module, text) of the one app package over core.
    files: Vec<(&'static str, &'static str, String)>,
    /// The checkout directory under the case's root.
    dir: &'static str,
    args: Vec<String>,
    env: Vec<(&'static str, Option<&'static str>)>,
    /// Must the request record answer this step from the primed store?
    hit: bool,
    /// Done to the case's root before the primed request.
    before: Option<fn(&Path)>,
    /// Run the primed request through a copy of the driver.
    copied: bool,
    /// The output path under the run directory.
    output: &'static str,
    /// The exit status the empty store's request must have: 0 unless the
    /// case is about a failure, so no step passes by failing for an
    /// unrelated cause in both stores.
    exit: i32,
    /// App files the manifest names by a path relative to the request's
    /// working directory; the rest it names absolutely.
    relative: &'static [&'static str],
}
struct Case {
    name: &'static str,
    steps: Vec<Step>,
}
#[derive(Debug, PartialEq)]
struct Ran {
    exit: Option<i32>,
    stdout: String,
    stderr: String,
    /// The output file's bytes' hash, when the request names one and it exists.
    output: Option<ContentHash>,
}
pub(crate) const CHILD_MARK: &str = "\u{1}request-child\n";
/// The child: run the request in `REQUEST_CHILD_ARGS` as `hirsel` does.
#[test]
#[ignore = "explicit native integration target"]
fn request_child() {
    let Some(args) = std::env::var_os("REQUEST_CHILD_ARGS") else {
        return;
    };
    let args: Vec<String> = serde_json::from_str(args.to_str().unwrap()).unwrap();
    use std::io::Write;
    std::io::stdout().write_all(CHILD_MARK.as_bytes()).unwrap();
    std::io::stdout().flush().unwrap();
    std::io::stderr().write_all(CHILD_MARK.as_bytes()).unwrap();
    crate::request::main(args)
}
pub(super) fn tape_request_command(args: &[String], directory: &Path) -> std::process::Command {
    request_command_at(&std::env::current_exe().unwrap(), args, directory)
}

pub(crate) fn request_command_at(
    executable: &Path,
    args: &[String],
    directory: &Path,
) -> std::process::Command {
    let mut command = std::process::Command::new(executable);
    command
        .args(["--exact", "native::tests::process::request_child", "--ignored", "--nocapture", "--test-threads=1"])
        .env("REQUEST_CHILD_ARGS", serde_json::to_string(args).unwrap())
        .current_dir(directory);
    command
}

/// Exercise the real CLI request in the existing process child harness.
pub(super) fn tape_request(args: &[String], directory: &Path) -> (Option<i32>, String, String) {
    tape_request_output(tape_request_command(args, directory))
}
/// Run a prepared request child and split its output after the child mark.
pub(crate) fn tape_request_output(mut command: std::process::Command) -> (Option<i32>, String, String) {
    let output = command.output().unwrap();
    let after = |bytes: &[u8]| {
        let text=String::from_utf8_lossy(bytes);
        let at=text.find(CHILD_MARK).expect("request child started")+CHILD_MARK.len();
        text[at..].to_owned()
    };
    (output.status.code(),after(&output.stdout),after(&output.stderr))
}
fn step(command: &'static str, files: &[(&'static str, &'static str, &str)]) -> Step {
    Step {
        command,
        files: files.iter().map(|(f, m, t)| (*f, *m, (*t).to_owned())).collect(),
        dir: "src",
        args: vec![],
        env: vec![],
        hit: false,
        before: None,
        copied: false,
        output: "out/program",
        exit: 0,
        relative: &[],
    }
}
fn hit(step: Step) -> Step {
    Step { hit: true, ..step }
}
/// Write `step`'s checkout: core's sources copied in, the app's files, and
/// the manifest `inputs.json` beside them, so every file name the request
/// prints is inside the checkout.
fn checkout_for(root: &Path, step: &Step) -> PathBuf {
    let dir = root.join(step.dir);
    let core = dir.join("core");
    if !core.exists() {
        std::fs::create_dir_all(&core).unwrap();
        for entry in std::fs::read_dir(checkout().join("core/core")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "hirsel") {
                std::fs::copy(&path, core.join(path.file_name().unwrap())).unwrap();
            }
        }
    }
    let app = dir.join("app");
    if app.exists() {
        std::fs::remove_dir_all(&app).unwrap();
    }
    std::fs::create_dir_all(&app).unwrap();
    let mut sources = vec![];
    for (file, module, text) in &step.files {
        std::fs::write(app.join(file), text).unwrap();
        sources.push(hirsel_core_resolve::ModuleSource {
                    foreign: None,
                    captured: None,
            file: if step.relative.contains(file) {
                Path::new(step.dir).join("app").join(file)
            } else {
                app.join(file)
            },
            modules: module.split('/').map(str::to_owned).collect(),
            signatures: false,
        });
    }
    let manifest = CorpusManifest {
        core: package("core"),
        packages: vec![
            PackageProduct::bundled_manifest(&core),
            PackageManifest {
                    namespace: None,
                package: package_id("app"),
                dependencies: vec![package("core")],
                sources,
                toolchain: false,
            },
        ],
    };
    let inputs = dir.join("inputs.json");
    std::fs::write(&inputs, serde_json::to_vec(&manifest).unwrap()).unwrap();
    inputs
}
fn native_args(root: &Path, output: &str) -> Vec<String> {
    let repo = checkout();
    vec![
        "--entry".into(),
        "app@1/main/main".into(),
        "--source-core".into(),
        "--runtime".into(),
        repo.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native runtime archive"))
            .display()
            .to_string(),
        "--cc".into(),
        PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap())
            .join("bin/clang")
            .display()
            .to_string(),
        "-o".into(),
        root.join(output).display().to_string(),
    ]
}
/// The content store (executables, request records) of requests on `store`.
fn cache_for(store: &Path) -> PathBuf {
    store.with_extension("cache")
}
/// Run `step` against the store `store`, from a run directory `run` that
/// holds its output file and metrics; return the answer and its reuse stages.
fn run_step(root: &Path, step: &Step, store: &Path, run: &Path, exe: &Path) -> (Ran, String) {
    let inputs = checkout_for(root, step);
    std::fs::create_dir_all(run.join("out")).unwrap();
    let metrics = run.join("metrics.json");
    let _ = std::fs::remove_file(&metrics);
    let mut args = vec![
        step.command.to_owned(),
        "--inputs".into(),
        inputs.display().to_string(),
        "--store".into(),
        store.display().to_string(),
        "--metrics".into(),
        metrics.display().to_string(),
    ];
    if step.command != "check" {
        args.extend(native_args(run, step.output));
    }
    args.extend(
        step.args
            .iter()
            .map(|arg| arg.replace("<root>", &root.display().to_string())),
    );
    let mut child = Command::new(exe);
    child
        .args(&args)
        .env("XDG_CACHE_HOME", cache_for(store))
        .current_dir(root);
    for (name, value) in &step.env {
        match value {
            Some(value) => child.env(name, value),
            None => child.env_remove(name),
        };
    }
    let out = child.output().unwrap();
    let after = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes).into_owned();
        let text = text
            .replace(&cache_for(store).display().to_string(), "<cache>")
            .replace(&store.display().to_string(), "<store>")
            .replace(&run.display().to_string(), "<run>");
        // A memory refusal's allocation size and resident peak are this
        // process's measurements, not the request's answer.
        text.lines()
            .map(|line| match line.split_once(" reached (HIRSEL_MEMORY_LIMIT)") {
                Some((head, _)) => format!("{head} reached"),
                None => line.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let stdout = after(&out.stdout);
    let output = std::fs::read(run.join(step.output))
        .ok()
        .map(|bytes| ContentHash::of_bytes(&bytes));
    // Store stages, in the order the request ran them.
    let metrics = std::fs::read_to_string(&metrics).unwrap_or_default();
    let record = metrics
        .split('"')
        .filter(|name| name.starts_with("request record/") || name.starts_with("build store/"))
        .collect::<Vec<_>>()
        .join(", ");
    (
        Ran {
            exit: out.status.code(),
            stdout,
            stderr: after(&out.stderr),
            output,
        },
        record,
    )
}
/// Each case's steps against one primed store, each compared with the same
/// step against an empty store; returns the failures.
fn run_case(case: &Case) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let canonical = dir.path().canonicalize().unwrap();
    let root = canonical.as_path();
    let exe = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(std::env::var_os("HIRSEL_DEV_TEST_DRIVER").expect("driver artifact"))
        .canonicalize()
        .unwrap();
    let (primed_store, copy) = (root.join("primed-store"), root.join("copied-driver"));
    let mut failures = vec![];
    let mut before = String::new();
    for (n, step) in case.steps.iter().enumerate() {
        if let Some(before) = step.before {
            before(root);
        }
        if step.copied && !copy.exists() {
            std::fs::copy(&exe, &copy).unwrap();
        }
        let primed_run = root.join("primed-run");
        let _ = std::fs::remove_dir_all(primed_run.join("out"));
        let (primed, record) = run_step(
            root,
            step,
            &primed_store,
            &primed_run,
            if step.copied { &copy } else { &exe },
        );
        let fresh_run = root.join(format!("fresh-run{n}"));
        let (fresh, _) = run_step(root, step, &root.join(format!("fresh-store{n}")), &fresh_run, &exe);
        if primed != fresh {
            failures.push(format!(
                "{} step {n}: primed {primed:#?}\n  fresh {fresh:#?}",
                case.name
            ));
        }
        if fresh.exit != Some(step.exit) {
            failures.push(format!(
                "{} step {n}: the empty store's request exited {:?}, the case needs {:?}",
                case.name, fresh.exit, step.exit
            ));
        }
        let hit_stage = if step.command == "build" {
            "build store/hit"
        } else {
            "request record/hit"
        };
        let hit = record.split(", ").any(|stage| stage == hit_stage);
        if hit != step.hit {
            failures.push(format!(
                "{} step {n}: the store {} it ({record}; the step before: {before}), the case says it {}",
                case.name,
                if hit { "answered" } else { "did not answer" },
                if step.hit { "must" } else { "must not" }
            ));
        }
        before = record;
    }
    failures
}

const MAIN: &str = "pub fn main()->Result[(),Exit]{Error(Exit(status:7u8))}";
const MAIN_GRANTS: &str = "pub fn main(files: Dir, clock: Clock)->Result[(),Exit]{Error(Exit(status:7u8))}";
const MAIN_B: &str = "pub fn main()->Result[(),Exit]{Error(Exit(status:9u8))}";
const MAIN_BAD: &str = "pub fn main()->Result[(),Exit]{Error(Exit(status:nope))}";
/// A memory limit the harness starts under and checking core does not
/// (measured: 8M refuses the check, 12M passes it).
const LOW_LIMIT: &str = "8M";
fn with(mut step: Step, change: impl FnOnce(&mut Step)) -> Step {
    change(&mut step);
    step
}
/// Remove the blob holding the executable the last primed step wrote.
fn delete_stored_executable(root: &Path) {
    for path in stored_executable(root) {
        std::fs::remove_file(path).unwrap();
    }
}
fn damage_stored_executable(root: &Path) {
    for path in stored_executable(root) {
        let mut bytes = std::fs::read(&path).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 1;
        std::fs::write(path, bytes).unwrap();
    }
}
fn stored_executable(root: &Path) -> Vec<PathBuf> {
    // A build that wrote nothing leaves nothing to find; the case's
    // comparison reports that build.
    let Ok(program) = std::fs::read(root.join("primed-run/out/program")) else {
        return vec![];
    };
    let mut found = vec![];
    let mut dirs = vec![cache_for(&root.join("primed-store"))];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if std::fs::read(&path).is_ok_and(|bytes| bytes == program) {
                found.push(path);
            }
        }
    }
    assert!(!found.is_empty(), "the primed store holds no copy of the executable");
    found
}
fn opt(step: Step, args: &[&str]) -> Step {
    with(step, |s| s.args.extend(args.iter().map(|a| (*a).to_owned())))
}
/// A C driver that runs the pinned clang with a different runtime search path.
fn other_cc(root: &Path) {
    let clang = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang");
    let cc = root.join("other-cc");
    crate::executable::write(
        &cc,
        format!("#!/bin/sh\nexec '{}' -Wl,-rpath,request-test \"$@\"\n", clang.display()).as_bytes(),
        0o755,
    )
    .unwrap();
}
fn cases() -> Vec<Case> {
    let check = |text: &str| step("check", &[("main.hirsel", "main", text)]);
    let build = |text: &str| step("build", &[("main.hirsel", "main", text)]);
    let llvm = |step: Step| opt(step, &["--backend", "llvm"]);
    let low = |step: Step| with(step, |s| s.env = vec![("HIRSEL_MEMORY_LIMIT", Some(LOW_LIMIT))]);
    let refused = |step: Step| with(low(step), |s| s.exit = hirsel_guard::EXIT_STATUS);
    // A check that reports a diagnostic exits 1.
    let diagnosed = |step: Step| Step { exit: 1, ..step };
    // An output path that is a directory: the build exits 2 writing it.
    let unwritable = |step: Step| Step { output: "out", exit: 2, ..step };
    // Two files of one module, the second with a diagnostic; `relative`
    // names a1 by a working-directory path, which sorts after a2's absolute
    // one, though the manifest-relative names sort a1 first.
    let two = |relative: &'static [&'static str]| {
        with(
            step(
                "check",
                &[
                    ("a1.hirsel", "a", "pub fn f()->u8{7u8}"),
                    ("a2.hirsel", "a", "pub fn g()->u8{nope}"),
                    ("main.hirsel", "main", "pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"),
                ],
            ),
            |s| s.relative = relative,
        )
    };
    vec![
        Case { name: "check edit, revert, no change", steps: vec![check(MAIN), hit(check(MAIN)), check(MAIN_B), hit(check(MAIN))] },
        Case { name: "check diagnostics, moved checkout", steps: vec![diagnosed(check(MAIN_BAD)), hit(diagnosed(check(MAIN_BAD))), hit(with(diagnosed(check(MAIN_BAD)), |s| s.dir = "moved"))] },
        Case { name: "build edit, revert, no change, moved", steps: vec![build(MAIN), hit(build(MAIN)), build(MAIN_B), hit(build(MAIN)), hit(with(build(MAIN), |s| s.dir = "moved"))] },
        Case { name: "commands and variants answer apart", steps: vec![check(MAIN), build(MAIN), hit(check(MAIN)), Step { exit: 7, ..step("run", &[("main.hirsel", "main", MAIN)]) }, hit(build(MAIN))] },
        // A record made under a higher memory limit must not answer a lower one.
        Case { name: "memory limit, success then refusal", steps: vec![check(MAIN), refused(check(MAIN)), hit(check(MAIN))] },
        Case { name: "memory limit, refusal then success", steps: vec![refused(check(MAIN)), check(MAIN), hit(check(MAIN))] },
        // D3: the output is a directory, so writing the executable fails after
        // the build stored it.
        Case { name: "output failure, then retry", steps: vec![unwritable(build(MAIN)), hit(build(MAIN)), hit(unwritable(build(MAIN)))] },
        Case { name: "stored executable deleted", steps: vec![build(MAIN), with(build(MAIN), |s| s.before = Some(delete_stored_executable)), hit(build(MAIN))] },
        Case { name: "stored executable damaged", steps: vec![build(MAIN), with(build(MAIN), |s| s.before = Some(damage_stored_executable)), hit(build(MAIN))] },
        Case { name: "LLVM facts toggle", steps: vec![llvm(build(MAIN)), with(llvm(build(MAIN)), |s| s.env = vec![(hirsel_llvm_lazy::core::NO_FACTS_VARIABLE, Some("1"))]), hit(llvm(build(MAIN)))] },
        // Each native option that changes the executable, after the last.
        Case { name: "native options answer apart", steps: vec![
            build(MAIN_GRANTS),
            llvm(build(MAIN_GRANTS)),
            opt(build(MAIN_GRANTS), &["--release", "--limit", "67108864"]),
            opt(build(MAIN_GRANTS), &["--debug-lines", "on"]),
            opt(build(MAIN_GRANTS), &["--limit", "1048576"]),
            opt(build(MAIN_GRANTS), &["--dir", "."]),
            opt(build(MAIN_GRANTS), &["--grant", "Clock"]),
            hit(build(MAIN_GRANTS)),
        ] },
        // A C driver that links differently: another toolchain.
        Case { name: "toolchain answers apart", steps: vec![
            build(MAIN),
            with(opt(build(MAIN), &["--cc", "<root>/other-cc"]), |s| s.before = Some(other_cc)),
            hit(build(MAIN)),
            hit(opt(build(MAIN), &["--cc", "<root>/other-cc"])),
        ] },
        Case { name: "copied driver", steps: vec![build(MAIN), hit(with(build(MAIN), |s| s.copied = true))] },
        // R1: two files of one module, their paths spelled two ways.
        Case { name: "two-file module respelled", steps: vec![diagnosed(two(&["a1.hirsel"])), hit(diagnosed(two(&[]))), hit(diagnosed(two(&["a1.hirsel"])))] },
    ]
}
/// The process leg: every case, each step against its empty store.
#[test]
#[ignore = "explicit native integration target"]
fn request_records_answer_as_an_empty_store() {
    let failures: Vec<String> = cases().iter().flat_map(run_case).collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
}
