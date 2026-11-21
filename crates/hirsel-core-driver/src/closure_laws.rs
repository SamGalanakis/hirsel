//! Laws of an instance's identity: a type argument is its closure, so two rows are one
//! type exactly when their closures are equal, wherever and in whatever order they
//! were produced, and a closure imported into another module is the same type there.
use super::*;
use hirsel_core_ir::{Identity, InstanceId, Module, Scalar, SigId, TypeId};
use hirsel_core_layout::{Carried, FieldTy, Repr, TypeRef};
use std::collections::BTreeMap;

/// `source` as module `main` of an app over the bundled core and std.
fn check(source: &str, dir: &Path, driver: &mut Driver) -> Arc<CheckProduct> {
    let app = dir.join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("main.hirsel"), source).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut request = CheckRequest::program(&app, &root, &dir.join("store")).unwrap();
    request.entry = None;
    let product = driver.check(request, &mut Meter::new(false)).unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    product
}
/// Every instance `root` reaches, with its materialised module's IR hash.
fn snapshot(product: &CheckProduct, root: &str) -> BTreeMap<Identity, ContentHash> {
    let reached = product
        .reachable(&DefPath::from_full(root).unwrap(), 256)
        .unwrap();
    assert!(reached.accepted(), "{:?}", reached.instances);
    let mut scratch = vec![];
    reached
        .instances
        .as_ref()
        .unwrap()
        .iter()
        .map(|k| {
            let hash = reached.materialized.get(k).ir_hash(&mut scratch).unwrap();
            (k.clone(), hash)
        })
        .collect()
}
fn row(module: &Module, name: impl Fn(&str) -> bool) -> TypeId {
    let names: Vec<_> = module.types.iter().map(|s| &s.name).collect();
    TypeId(
        module
            .types
            .iter()
            .position(|s| name(&s.name))
            .unwrap_or_else(|| panic!("no such row in {names:?}")) as u32,
    )
}
/// `closure` imported into a module that already numbers other rows: its root's
/// closure there.
fn imported(identity: &Identity, closure: &hirsel_core_ir::Closure) -> hirsel_core_ir::Closure {
    let mut module = Module::default();
    for i in 0..5 {
        let mut pad = module_pad();
        pad.name = format!("pad.{i}");
        module.types.push(pad);
    }
    let instance = hirsel_core_front::import_instance(&mut module, identity, &[closure])
        .unwrap_or_else(|e| panic!("{e}"));
    let root = module.instances[instance.index()].arguments[0];
    module.closure(root)
}
fn module_pad() -> hirsel_core_ir::Shape {
    hirsel_core_ir::Shape {
        name: String::new(),
        repr: Repr::Own,
        carried: Carried::Nothing,
        body: hirsel_core_layout::Body::Record(vec![]),
        finalizer: None,
        clone: None,
        codec: None,
        grant: 0,
        send: false,
        held: None,
    }
}

const FIELDS: &str = "resource type Guard[T] { d: u64, value: T } drop(t) { let _ = divide(t.d); }\n\
fn divide(d: u64) -> u64 { 100u64 / d }\n\
type Probe { value: u64 }\n\
type Outer { g: Guard[u64], p: Probe, b: Box[Probe], c: Option[char], s: String }\n\
fn keep[T](take x: T) -> T { x }\n\
fn root(take x: Outer) -> Outer { keep(take x) }\n";

/// Field sensitivity at both boundaries, and renumbering. Law: two rows that differ
/// in one layout-read field (grant, repr, carried, held, a hook's type argument, a
/// hook's `dest`, a holder's encoding, a niche-bearing field) are two types: their
/// closures' hashes differ, and so do the identities of the instance they argue;
/// and each closure, imported into a module that numbers its rows otherwise, is the
/// same closure there. Oracle: the rule that a closure is the type (ir.rs,
/// `Closure`), applied to rows that differ only where stated.
#[test]
fn closure_rows_differ_in_every_layout_field_and_import_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let product = check(FIELDS, dir.path(), &mut Driver::default());
    let reached = product
        .reachable(&DefPath::from_full("app@1/main/root").unwrap(), 64)
        .unwrap();
    assert!(reached.accepted(), "{:?}", reached.instances);
    let keep = DefPath::from_full("app@1/main/keep").unwrap();
    let key = reached
        .instances
        .as_ref()
        .unwrap()
        .iter()
        .find(|k| k.definition == keep)
        .unwrap()
        .clone();
    let body = reached.materialized.get(&key);
    let base = body.ir().unwrap().clone();
    let instance = base.functions[body.function.index()].instance;
    let root = base.instances[instance.index()].arguments[0];
    let closure = base.closure(root);
    assert_eq!(base.identity(instance), key);
    assert_eq!(imported(&key, &closure), closure, "renumbered import");
    let probe = row(&base, |n| n.ends_with("Probe"));
    let guard = row(&base, |n| n.contains("Guard[u64]"));
    let option = row(&base, |n| n.contains("Option[char]"));
    let finalizer = base.types[guard.index()]
        .finalizer
        .expect("Guard's finalizer");
    type Edit = Box<dyn Fn(&mut Module)>;
    let edits: Vec<(&str, Edit)> = vec![
        (
            "grant (root)",
            Box::new(move |m| m.types[root.index()].grant += 64),
        ),
        (
            "grant (nested)",
            Box::new(move |m| m.types[probe.index()].grant += 64),
        ),
        (
            "repr",
            Box::new(move |m| {
                let r = &mut m.types[probe.index()].repr;
                *r = if *r == Repr::Own {
                    Repr::Region
                } else {
                    Repr::Own
                };
            }),
        ),
        (
            "carried",
            Box::new(move |m| {
                let c = &mut m.types[probe.index()].carried;
                *c = if *c == Carried::Address {
                    Carried::Nothing
                } else {
                    Carried::Address
                };
            }),
        ),
        (
            "held",
            Box::new(move |m| {
                let h = &mut m.types[probe.index()].held;
                *h = match h {
                    None => Some(FieldTy::Box(TypeRef(probe.0))),
                    Some(_) => None,
                };
            }),
        ),
        (
            "hook argument",
            Box::new(move |m| {
                let f = &m.functions[finalizer.0 as usize];
                m.instances[f.instance.index()].arguments[0] = probe;
            }),
        ),
        (
            "hook dest",
            Box::new(move |m| {
                let f = m.functions[finalizer.0 as usize].signature;
                let mut signature = m.signatures[f.index()].clone();
                signature.dest = match signature.dest {
                    None => Some(probe),
                    Some(_) => None,
                };
                m.signatures.push(signature);
                m.functions[finalizer.0 as usize].signature = SigId(m.signatures.len() as u32 - 1);
            }),
        ),
        (
            "holder",
            Box::new(move |m| {
                edit_field(m, root, |ty| match ty {
                    FieldTy::Box(t) => Some(FieldTy::RegionRef(*t)),
                    _ => None,
                })
            }),
        ),
        (
            "niche",
            Box::new(move |m| {
                edit_field(m, option, |ty| match ty {
                    FieldTy::Scalar(Scalar::Char) => Some(FieldTy::Scalar(Scalar::U32)),
                    _ => None,
                })
            }),
        ),
    ];
    for (field, edit) in edits {
        let mut module = base.clone();
        edit(&mut module);
        let edited = module.closure(root);
        assert_ne!(edited.hash, closure.hash, "{field}: closure hash");
        let identity = module.identity(instance);
        assert_ne!(identity, key, "{field}: identity");
        assert_eq!(imported(&identity, &edited), edited, "{field}: import");
    }
}
/// The first field of row `ty` that `edit` rewrites, rewritten.
fn edit_field(module: &mut Module, ty: TypeId, edit: impl Fn(&FieldTy) -> Option<FieldTy>) {
    let fields: Vec<&mut hirsel_core_layout::Field> = match &mut module.types[ty.index()].body {
        hirsel_core_layout::Body::Record(fields) => fields.iter_mut().collect(),
        hirsel_core_layout::Body::Variant(cases) => {
            cases.iter_mut().flat_map(|c| &mut c.fields).collect()
        }
    };
    for field in fields {
        if let Some(ty) = edit(&field.ty) {
            field.ty = ty;
            return;
        }
    }
    panic!("no field to edit");
}

/// Renumbering at the producer. Law: the instance a program argues with a type has
/// one identity however the program orders its declarations and whatever unrelated
/// rows lowering numbers first. Oracle: the two programs declare the same types.
#[test]
fn reordered_declarations_argue_the_same_identities() {
    let dir = tempfile::tempdir().unwrap();
    let first = check(FIELDS, dir.path(), &mut Driver::default());
    let mut lines: Vec<_> = FIELDS.lines().collect();
    lines.reverse();
    let reordered = format!(
        "type Unrelated {{ a: u8, b: Option[u16] }}\nfn spare(x: Unrelated) -> u8 {{ x.a }}\n{}\n",
        lines.join("\n")
    );
    let other = tempfile::tempdir().unwrap();
    let second = check(&reordered, other.path(), &mut Driver::default());
    let ids = |p: &CheckProduct| {
        snapshot(p, "app@1/main/root")
            .into_keys()
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&first), ids(&second));
}

/// `R[u64]`: a generic resource's finalizer is the instance of its own arguments.
/// Law: `Guard[u64]`'s finalizer is `Guard` at `u64`, in the module and in the
/// closure, and the build reaches that instance. Oracle: the source's `Guard[u64]`.
#[test]
fn a_generic_finalizer_binds_its_scalar_argument() {
    let dir = tempfile::tempdir().unwrap();
    let product = check(FIELDS, dir.path(), &mut Driver::default());
    let reached = product
        .reachable(&DefPath::from_full("app@1/main/root").unwrap(), 64)
        .unwrap();
    assert!(reached.accepted(), "{:?}", reached.instances);
    let keep = DefPath::from_full("app@1/main/keep").unwrap();
    let keys = reached.instances.as_ref().unwrap();
    let key = keys.iter().find(|k| k.definition == keep).unwrap();
    let body = reached.materialized.get(key);
    let module = body.ir().unwrap();
    let guard = row(module, |n| n.contains("Guard[u64]"));
    let u64_row = row(module, |n| n == "u64");
    let finalizer = module.types[guard.index()].finalizer.unwrap();
    let instance: InstanceId = module.functions[finalizer.0 as usize].instance;
    assert_eq!(module.instances[instance.index()].arguments, [u64_row]);
    let closure = module.closure(guard);
    let hook = closure.rows[0].hooks[0].as_ref().unwrap();
    let [p] = hook.arguments[..] else {
        panic!("{:?}", hook.arguments)
    };
    assert_eq!(closure.rows[p as usize].shape.name, "u64");
    let expect = Identity {
        definition: DefPath::from_full("app@1/main/Guard").unwrap(),
        dictionaries: vec![],
        closures: vec![module.closure(u64_row).hash],
    };
    assert!(keys.contains(&expect), "{keys:?}");
}

/// PROG-5 and PROG-10. The template `hold[T]` uses `Guard[T]` with a finalizer,
/// but `root` only calls `hold[u64]`. Reached instances retain concrete rows.
#[test]
fn a_hook_at_a_rigid_parameter_is_not_reached() {
    let dir = tempfile::tempdir().unwrap();
    let source = "resource type Guard[T] { d: u64, value: T } drop(t) { let _ = divide(t.d); }\n\
fn divide(d: u64) -> u64 { 100u64 / d }\n\
fn hold[T](take g: Guard[T]) -> u64 { 1u64 }\n\
fn root(take g: Guard[u64]) -> u64 { hold(take g) }\n";
    let product = check(source, dir.path(), &mut Driver::default());
    let reached = product
        .reachable(&DefPath::from_full("app@1/main/root").unwrap(), 64)
        .unwrap();
    assert!(reached.accepted(), "{:?}", reached.instances);
    let hold = DefPath::from_full("app@1/main/hold").unwrap();
    let template = product.definitions[&hold].ir().unwrap();
    let rigid = row(template, |name| {
        name.contains("Guard[") && name.contains('$')
    });
    assert!(template.types[rigid.index()].finalizer.is_some());

    let keys = reached.instances.as_ref().unwrap();
    let hold_instance = keys.iter().find(|key| key.definition == hold).unwrap();
    for key in keys {
        let body = reached.materialized.get(key);
        let module = body.ir().unwrap();
        let f = &module.functions[body.function.index()];
        let names = module.argument_names(&module.instances[f.instance.index()]);
        if key == hold_instance {
            assert_eq!(names, ["u64"]);
            let guard = row(module, |name| name.contains("Guard[u64]"));
            assert!(module.types[guard.index()].finalizer.is_some());
        }
        assert!(names.iter().all(|n| !n.contains('$')), "{key:?}: {names:?}");
        assert!(
            module.types.iter().all(|shape| !shape.name.contains('$')),
            "{key:?} retains a rigid type row"
        );
    }
}

const UNIVERSE: &str = "type Inner { a: u64 }\n\
type Outer { i: Inner, n: u64 }\n\
fn keep[T](take x: T) -> T { x }\n\
fn root(take x: Outer) -> u64 { let y = keep(take x); y.n }\n";

/// The retained universe (975). Law: in one resident driver, with product A
/// retained, a nested field edited at the same path builds B exactly as a fresh
/// driver does; then rebuilding A builds it exactly as fresh, and the retained A is
/// unchanged. Oracle: a fresh driver over a fresh store. Red if any closure, row or
/// instance memo outlived its request keyed by a name or a row number.
#[test]
fn a_resident_driver_builds_each_universe_like_a_fresh_one() {
    // Each fresh product is built on a thread of its own, so nothing the process
    // keeps per thread reaches it.
    let fresh = |source: &str| {
        let source = source.to_owned();
        std::thread::spawn(move || {
            let dir = tempfile::tempdir().unwrap();
            snapshot(
                &check(&source, dir.path(), &mut Driver::default()),
                "app@1/main/root",
            )
        })
        .join()
        .unwrap()
    };
    let edited = UNIVERSE.replace("{ a: u64 }", "{ a: u8 }");
    let (fresh_a, fresh_b) = (fresh(UNIVERSE), fresh(&edited));
    assert_ne!(
        fresh_a.keys().collect::<Vec<_>>(),
        fresh_b.keys().collect::<Vec<_>>()
    );
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::default();
    let a = check(UNIVERSE, dir.path(), &mut driver);
    assert_eq!(snapshot(&a, "app@1/main/root"), fresh_a, "A");
    let b = check(&edited, dir.path(), &mut driver);
    assert_eq!(snapshot(&b, "app@1/main/root"), fresh_b, "B after A");
    let again = check(UNIVERSE, dir.path(), &mut driver);
    assert_eq!(snapshot(&again, "app@1/main/root"), fresh_a, "A after B");
    assert_eq!(snapshot(&a, "app@1/main/root"), fresh_a, "retained A");
    assert_eq!(snapshot(&b, "app@1/main/root"), fresh_b, "retained B");
}
