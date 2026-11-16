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
// and RUN receipts under /workspace/code/hirsel-briefs/<review>/).

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
    /// Core is checked through a package product cache kept beside the store,
    /// as `check --core-product` does.
    core_product: bool,
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
/// the generation before it is refused (v3.1 gate condition 4). Recorded for the
/// gate's own law; nothing here reads it yet.
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
        core_product: false,
    }
}
fn build(packages: Vec<Pkg>) -> Req {
    Req {
        entry: Some("app@1/main/main"),
        build: true,
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

fn checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
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
                package: package_id(spec),
                dependencies: vec![],
                sources: vec![],
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
}
fn answer(product: &CheckProduct, req: &Req, options: Option<&NativeOptions>) -> Answer {
    let check = serde_json::json!({
        "diagnostics": product.machine_diagnostics().unwrap(),
        "incomplete": product.machine_incomplete().unwrap(),
        "resources": product.machine_resources().unwrap(),
    });
    let native = options.filter(|_| req.build).map(|options| {
        let entry = DefPath::from_full(req.entry.unwrap()).unwrap();
        let reach = product.reachable_for(&entry, options).unwrap();
        let refused = serde_json::json!({
            "diagnostics": product.diagnostics_for(reach.diagnostics.iter().copied()).unwrap(),
            "incomplete": product.incomplete_for(reach.incomplete.iter().copied()).unwrap(),
            "resources": product.resources_for(reach.resources.iter()).unwrap(),
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
    let products = store.with_extension("products");
    let request = CheckRequest {
        inputs,
        store,
        library: LibraryInput::Sources,
        entry: req.entry.map(|e| DefPath::from_full(e).unwrap()),
    };
    let meter = &mut Meter::new(false);
    let product = if req.core_product {
        match driver.check_with_core_product(request, &products, meter) {
            Ok(product) => product,
            Err(refused) => {
                return Answer {
                    check: serde_json::json!({ "core_product": refused.to_string() }),
                    root: None,
                    native: None,
                };
            }
        }
    } else {
        driver.check(request, meter).unwrap()
    };
    answer(&product, req, options.as_ref())
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
/// Run one sequence; return each step's failures of the law and, separately, the
/// steps whose roots differ while everything else agrees.
fn run_sequence(seq: &Seq, backend: Option<Backend>) -> (Vec<String>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("checkout");
    let mut written = BTreeSet::new();
    let mut resident = Driver::default();
    let (mut failures, mut roots) = (vec![], vec![]);
    let options = |tag: &str, req: &Req| {
        backend.map(|backend| {
            let mut options = NativeOptions {
                store: dir.path().join(format!("{tag}-native")),
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
            options("resident", req),
        );
        let disk_answer = request(
            &mut Driver::default(),
            req,
            inputs.clone(),
            dir.path().join("disk"),
            options("disk", req),
        );
        if n == 0 {
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
/// exactly `unequal_roots` (name, step, store).
fn run_sequences(seqs: Vec<Seq>, backend: Option<Backend>, unequal_roots: &[&str]) {
    let (mut failures, mut roots) = (vec![], vec![]);
    for seq in &seqs {
        let (f, r) = run_sequence(seq, backend);
        failures.extend(f);
        roots.extend(r);
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
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(x:Deque[u8])->Result[Deque[u8],Capacity]{clone(x)}"###))])]),
        check(vec![
            core(vec![src("deque", "core_deque.hirsel", true, repo("core/core/deque.hirsel", r###"clone[T](d: Deque[T]) {
    let count = list.len(d.ring.raw)"###, r###"clone[T](d: Deque[T]) {
    let _=0u8
    let count = list.len(d.ring.raw)"###))]),
            pkg("app", &["core"], vec![src("a", "app_a.hirsel", false, lit(r###"fn f(x:Deque[u8])->Result[Deque[u8],Capacity]{clone(x)}"###))])])]),
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
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn /* shift */ f(){}"###)), src("a", "a#2.hirsel", false, lit(r###"fn f(){}"###))])])]),
        seq("foreign_private_span", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:b.Hidden){}"###)), src("b", "b.hirsel", false, lit(r###"type Hidden{}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"fn f(x:b.Hidden){}"###)), src("b", "b.hirsel", false, lit(r###"type /* shifted */ Hidden{}"###))])])]),
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
        seq("reverse_twin", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A { n:u64 }"###)), src("aa", "aa.hirsel", false, lit(r###"pub type C { n:u64 }"###)), src("b", "b.hirsel", false, lit(r###"@twin(a.A) pub type B { n:u64 }"###)), src("p", "p.hirsel", false, lit(r###"@twin(q.Q) pub type P { x:a.A }"###)), src("q", "q.hirsel", false, lit(r###"pub type Q { x:b.B }"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A { n:u64 }"###)), src("aa", "aa.hirsel", false, lit(r###"@twin(a.A) pub type C { n:u64 }"###)), src("b", "b.hirsel", false, lit(r###"@twin(a.A) pub type B { n:u64 }"###)), src("p", "p.hirsel", false, lit(r###"@twin(q.Q) pub type P { x:a.A }"###)), src("q", "q.hirsel", false, lit(r###"pub type Q { x:b.B }"###))])])]),
        seq("reverse_twin_order", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A { n:u64 }"###)), src("aa", "aa.hirsel", false, lit(r###"@twin(a.A) pub type C { n:u64 }"###)), src("b", "b.hirsel", false, lit(r###"@twin(a.A) pub type B { n:u64 }"###)), src("p", "p.hirsel", false, lit(r###"@twin(q.Q) pub type P { x:a.A }"###)), src("q", "q.hirsel", false, lit(r###"pub type Q { x:b.B }"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("q", "q.hirsel", false, lit(r###"pub type Q { x:b.B }"###)), src("p", "p.hirsel", false, lit(r###"@twin(q.Q) pub type P { x:a.A }"###)), src("b", "b.hirsel", false, lit(r###"@twin(a.A) pub type B { n:u64 }"###)), src("aa", "aa.hirsel", false, lit(r###"@twin(a.A) pub type C { n:u64 }"###)), src("a", "a.hirsel", false, lit(r###"pub type A { n:u64 }"###))])])]),
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
        seq("twin_body", vec![
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A { n:u64 }"###)), src("b", "b.hirsel", false, lit(r###""###)), src("p", "p.hirsel", false, lit(r###"fn f(x:a.A){let _=clone(x)}"###))])]),
        check(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A { n:u64 }"###)), src("b", "b.hirsel", false, lit(r###"@twin(a.A) pub type B { n:u64 }"###)), src("p", "p.hirsel", false, lit(r###"fn f(x:a.A){let _=clone(x)}"###))])])]),
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
pub fn value(p:P)->u8{p.x}"###)), src("hook", "core_hook.hirsel", true, lit(r###"clone(p:held.P){Ok(held.make(held.value(p)))}"###))]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn use[T](x:T)->u8{7u8}"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:core.held.P)->Result[core.held.P,Capacity]{let _=a.use(x);clone(x)}"###))])]),
        check(vec![
            core(vec![src("held", "core_held.hirsel", true, lit(r###"pub type P{x:u8}
pub fn make(x:u8)->P{P(x:x)}
pub fn value(p:P)->u8{let x=p.x; x}"###)), src("hook", "core_hook.hirsel", true, lit(r###"clone(p:held.P){Ok(held.make(held.value(p)))}"###))]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub fn use[T](x:T)->u8{7u8}"###)), src("b", "b.hirsel", false, lit(r###"fn f(x:core.held.P)->Result[core.held.P,Capacity]{let _=a.use(x);clone(x)}"###))])])]),
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
        seq("twin_current_owner", vec![
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A{x:u8}
pub fn f()->u8{1u8}"###)), src("b", "b.hirsel", false, lit(r###"@twin(a.A) pub type B{x:u8}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###))])]),
        build(vec![
            core(vec![]),
            pkg("app", &["core"], vec![src("a", "a.hirsel", false, lit(r###"pub type A{x:u8}
pub fn f()->u8{7u8}"###)), src("b", "b.hirsel", false, lit(r###"@twin(a.A) pub type B{x:u8}"###)), src("main", "main.hirsel", false, lit(r###"pub fn main()->Result[(),Exit]{Error(Exit(status:a.f()))}"###))])])]),
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
    // core/ordering enumerates A,C,B,P,Q both times; the file names alone set
    // the traversal, and the traversal decides which twin pairs with which.
    let ordering = |label: &str, names: [usize; 5]| {
        let texts = [
            "pub type A{n:u64}",
            "@twin(A) pub type C{n:u64}",
            "@twin(A) pub type B{n:u64}",
            "@twin(Q) pub type P{x:A}",
            "pub type Q{x:B}",
        ];
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
            core_product: true,
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
        // Q,P,B,C,A is accepted; the same bytes renamed to traverse A,C,B,P,Q
        // are refused fresh, so a package record keyed by modules alone serves
        // the first order's acceptance.
        seq(
            "product_order",
            vec![
                ordering("a", [4, 3, 2, 1, 0]),
                ordering("b", [0, 1, 2, 3, 4]),
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
/// Not request sequences here: image_recursion compares an image layer with
/// sources; resource needs a process allocation limit.
fn sequences() -> Vec<Seq> {
    let mut seqs = review_sequences();
    seqs.extend(review_1039_sequences());
    seqs.extend(tuple_sequences());
    seqs
}

/// Steps whose check root differs from an empty store's while their answers
/// agree, with the cause.
const UNEQUAL_ROOTS: &[&str] = &[];

/// Each sequence's predicted gate class (see [`Gate`]), sorted by name. Unmarked
/// rows are l-body-review-1039's settled column (programs/*-v3.md, NEW-SEQUENCES.md).
#[rustfmt::skip]
const GATES: &[(&str, Gate)] = &[
    ("alias_cycle_added", Gate::Fallback),
    ("alias_cycle_body", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("alias_target", Gate::Fallback),
    ("backend_llvm", Gate::Fast),
    ("body_parse_refusal", Gate::Fallback),
    ("body_test_reference", Gate::Fast),
    ("bound_transitive", Gate::Fallback),
    ("check_to_build", Gate::Fallback),
    ("checkout_relocated", Gate::Fast),
    ("clone_added", Gate::Fallback),
    ("clone_body", Gate::Fast),
    ("clone_winner_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("clone_winner_removed", Gate::Fallback),
    ("code_identity_body", Gate::Fast),
    ("codec_context_added", Gate::Fallback),
    ("codec_default", Gate::Fallback),
    ("codec_field_changed", Gate::Fallback),
    ("codec_owner_body", Gate::Fast),
    ("comptime_absent", Gate::Fallback),
    ("constant_call_absent", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("constant_cycle", Gate::Fallback),
    ("constant_dfs_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("constant_parts_forward", Gate::Fast),
    ("constant_projection", Gate::Fallback),
    ("constant_ready_queue", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("constant_value", Gate::Fallback),
    ("constant_with_current_owner", Gate::Fast),
    ("constructor_body", Gate::Fallback),
    ("constructor_body_fixed", Gate::Fallback), // not in 1039: b gains a type, shape changes
    ("constructor_const", Gate::Fallback),
    ("constructor_const_fixed", Gate::Fallback), // not in 1039: b gains a type, shape changes
    ("constructor_pattern", Gate::Fallback),
    ("core_source_edit", Gate::Fast),
    ("cycle_two_current", Gate::Fast),
    ("debug_lines", Gate::Fast),
    ("decl_count_fallback", Gate::Fallback),
    ("declaration_pass_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("default_argument_absent", Gate::Fallback),
    ("dependency_order", Gate::Fast),
    ("dependency_version", Gate::Fallback),
    ("doc_trivia", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("duplicate_copy_body", Gate::Fallback), // not in 1039: A refuses the duplicate
    ("entry_warning_interleave", Gate::Fallback),
    ("extension_candidate", Gate::Fallback),
    ("extension_new_ambiguous", Gate::Fallback),
    ("failure_body", Gate::Fast),
    ("finalizer_body", Gate::Fallback),
    ("finalizer_cross_module", Gate::Fast),
    ("foreign_duplicate_span", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("foreign_private_span", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("full_to_reach", Gate::Fallback),
    ("generic_body", Gate::Fast),
    ("generic_body_result", Gate::Fast),
    ("generic_cross_clone", Gate::Fast),
    ("generic_instance_new_type", Gate::Fast),
    ("generic_refused_then_demanded", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("generic_transitive", Gate::Fast),
    ("global_summary_refusal", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("header_alternates", Gate::Fast), // not in 1039: B's shape falls back and does not publish; A' published A's generation
    ("header_type_before_function", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("header_type_before_function_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("holds_transitive", Gate::Fallback),
    ("implicit_capacity", Gate::Fallback),
    ("index_builtin", Gate::Fallback),
    ("index_duplicate", Gate::Fallback),
    ("inline_cycle", Gate::Fallback),
    ("keyword_module", Gate::Fallback),
    ("lang12_direct", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("lang12_mutual_break", Gate::Fallback),
    ("lang12_mutual_growth", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("lang12_permutation", Gate::Fast),
    ("lang12_reset", Gate::Fast),
    ("lang12_unused_parameter", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("lang12_wrapper_module", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("lang12_wrapper_own", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("lang12_wrapper_package", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("layout_embedded", Gate::Fallback),
    ("layout_failure_removed", Gate::Fallback),
    ("layout_first_failure_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("layout_global_failure", Gate::Fallback),
    ("macro_absent", Gate::Fallback),
    ("manifest_permuted", Gate::Fast),
    ("module_split", Gate::Fallback),
    ("native_const", Gate::Fallback),
    ("native_foreign_body", Gate::Fast),
    ("native_reorder", Gate::Fast),
    ("negative_module_head", Gate::Fallback),
    ("new_body_refusal", Gate::Fast),
    ("opaque_private_field", Gate::Fallback),
    ("optimization", Gate::Fast),
    ("parse_mode_changed", Gate::Fallback),
    ("prelude_negative_type", Gate::Fallback),
    ("private_alias", Gate::Fallback),
    ("private_function_body", Gate::Fast),
    ("product_order", Gate::Fallback), // 1036: fallback, the source tuple reorders
    ("publish_body_third", Gate::Fast),
    ("publish_check_build_check", Gate::Fast), // 1039 cold history: fallback; here A' published the check generation and the build does not replace it
    ("publish_refused_second", Gate::Fast),
    ("publish_uncommitted_second", Gate::Fast),
    ("reach_to_full", Gate::Fallback),
    ("reexport_alias", Gate::Fallback),
    ("repair_one_body_refusal", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("reverse_twin", Gate::Fallback),
    ("reverse_twin_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("ring", Gate::Fast),
    ("root_limit", Gate::Fast),
    ("root_missing", Gate::Fallback),
    ("same_module_file_tie", Gate::Fallback),
    ("same_shape_refused_constant", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("source_moves_module", Gate::Fallback),
    ("std_dependency_added", Gate::Fallback),
    ("symbol_body_insertion", Gate::Fast),
    ("symlink_directory", Gate::Fast),
    ("test_added", Gate::Fallback),
    ("test_body_added", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("test_header_flip", Gate::Fallback),
    ("test_signature_before_reference_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("test_signature_reference_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("twin_body", Gate::Fallback),
    ("twin_current_owner", Gate::Fast),
    ("two_body_refusals_order", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("unchanged_body_refusal", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("unpublished_temp", Gate::Fast),
    ("unreachable_body_refusal", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("via_conversion_body", Gate::Fast),
    ("via_representation", Gate::Fallback),
    ("waits_body", Gate::Fallback), // 1039: fast; a module of A is refused (v3.1 condition 4)
    ("waits_header", Gate::Fallback),
];

#[test]
fn request_sequences_answer_as_an_empty_store() {
    let seqs = sequences();
    let names: BTreeSet<&str> = seqs.iter().map(|s| s.name).collect();
    assert_eq!(names.len(), seqs.len(), "sequence names are distinct");
    let stale: Vec<&str> = GATES
        .iter()
        .map(|g| g.0)
        .filter(|n| !names.contains(n))
        .collect();
    assert!(stale.is_empty(), "gate classes of no sequence: {stale:?}");
    run_sequences(seqs, None, UNEQUAL_ROOTS);
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
