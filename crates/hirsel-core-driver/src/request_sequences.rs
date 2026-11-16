// Request sequences (L0). Law: a request served from a store that earlier requests
// of the sequence primed answers as the same request against an empty store. It has
// the same diagnostics, incomplete and resource rows, the same check root and, for a
// build, the same executable bytes and exit. Both primed paths are held to it: one
// driver across the sequence (memory and disk) and a new driver per request (disk
// alone). Sources are edited in place, as an editor does. Oracle: the empty store,
// which no earlier request can reach. Cases: the programs of option-l-review-1034
// and the request sequences of l-body-review-1036 (their fixtures and RUN receipts
// are under /workspace/code/hirsel-briefs/{option-l-review-1034,l-body-review-1036}).

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
}
#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    /// The selected check root's record is removed after the first request.
    MissingRoot,
    /// An unpublished partial record sits in the store's temporary directory.
    TornTemp,
}
struct Seq {
    name: &'static str,
    steps: Vec<Req>,
    fault: Fault,
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
    Seq {
        name,
        steps,
        fault: Fault::None,
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
                .map(|(file, modules, text)| src(modules, file, false, lit(text)))
                .collect(),
        ),
    ]
}

fn checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
/// Write `req`'s sources under `root`, removing the previous request's files it
/// no longer has, and return its manifest.
fn materialize(req: &Req, root: &Path, written: &mut BTreeSet<PathBuf>) -> CorpusManifest {
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
            name => PackageManifest {
                package: package(name),
                dependencies: vec![],
                sources: vec![],
            },
        };
        manifest.dependencies = p.deps.iter().map(|d| package(d)).collect();
        let bundled = manifest.sources.len();
        for s in &p.sources {
            let text = match s.text {
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
            let file = dir.join(p.name).join(&s.file);
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
    root: ContentHash,
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
        root: product.root,
        native,
    }
}
fn request(req: &Req, inputs: CorpusManifest, store: PathBuf) -> CheckRequest {
    CheckRequest {
        inputs,
        store,
        library: LibraryInput::Sources,
        entry: req.entry.map(|e| DefPath::from_full(e).unwrap()),
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
/// Run one sequence; return each step's failures of the law and, separately, the
/// steps whose roots differ while everything else agrees.
fn run_sequence(seq: &Seq, backend: Option<Backend>) -> (Vec<String>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("checkout");
    let mut written = BTreeSet::new();
    let mut resident = Driver::default();
    let (mut failures, mut roots) = (vec![], vec![]);
    let options = |tag: &str| {
        backend.map(|backend| NativeOptions {
            store: dir.path().join(format!("{tag}-native")),
            output: dir.path().join(format!("{tag}-program")),
            ..native_options(backend, dir.path())
        })
    };
    for (n, req) in seq.steps.iter().enumerate() {
        let inputs = materialize(req, &root, &mut written);
        let resident_product = resident
            .check(
                request(req, inputs.clone(), dir.path().join("resident")),
                &mut Meter::new(false),
            )
            .unwrap();
        let resident_answer = answer(&resident_product, req, options("resident").as_ref());
        let disk_product = Driver::default()
            .check(
                request(req, inputs.clone(), dir.path().join("disk")),
                &mut Meter::new(false),
            )
            .unwrap();
        let disk_answer = answer(&disk_product, req, options("disk").as_ref());
        if n == 0 {
            fault(&dir.path().join("resident"), seq.fault);
            fault(&dir.path().join("disk"), seq.fault);
            continue;
        }
        let fresh_store = dir.path().join(format!("fresh{n}"));
        let fresh_product = Driver::default()
            .check(request(req, inputs, fresh_store), &mut Meter::new(false))
            .unwrap();
        let fresh = answer(&fresh_product, req, options(&format!("fresh{n}")).as_ref());
        for (path, primed) in [("resident", &resident_answer), ("disk", &disk_answer)] {
            if (&primed.check, &primed.native) != (&fresh.check, &fresh.native) {
                failures.push(format!(
                    "{} step {n} ({path} store): primed {:#} {:?}\n  fresh {:#} {:?}",
                    seq.name, primed.check, primed.native, fresh.check, fresh.native
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

/// Sequences over the request tuple, faults, entry policy and a module ring
/// (l-body-review-1036 tuple_state.py and entry_policy.py).
fn tuple_sequences() -> Vec<Seq> {
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
    vec![
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
        seq(
            "entry_waits_policy",
            vec![
                Req {
                    entry: Some("app@1/a/main"),
                    ..check(app(&[
                        (
                            "a.hirsel",
                            "a",
                            "pub fn main()->Result[(),Exit] waits{Ok(())}",
                        ),
                        ("b.hirsel", "b", "pub fn other()->Result[(),Exit]{Ok(())}"),
                    ]))
                },
                Req {
                    entry: Some("app@1/b/other"),
                    ..check(app(&[
                        (
                            "a.hirsel",
                            "a",
                            "pub fn main()->Result[(),Exit] waits{Ok(())}",
                        ),
                        ("b.hirsel", "b", "pub fn other()->Result[(),Exit]{Ok(())}"),
                    ]))
                },
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
/// Not request sequences here: product_order (package order of an equal module
/// set) is check-product-959's case; image_recursion compares an image layer with
/// sources; resource needs a process allocation limit.
fn sequences() -> Vec<Seq> {
    let mut seqs = review_sequences();
    seqs.extend(tuple_sequences());
    seqs
}

/// Steps whose check root differs from an empty store's while their answers
/// agree, with the cause.
const UNEQUAL_ROOTS: &[&str] = &[];

#[test]
fn request_sequences_answer_as_an_empty_store() {
    run_sequences(sequences(), None, UNEQUAL_ROOTS);
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
