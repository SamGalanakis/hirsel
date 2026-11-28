use super::*;
use hirsel_core_ids::published::{Descriptor, Provider, Table, bind};
use std::fmt::Write;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// A doubling alias chain is laid out once per alias: T14, the longest within the
/// checker's expansion limit (`type_limits`), has 14 rows, not 2^15 - 1.
#[test]
fn repeated_tuple_alias_layout() {
    let mut source = String::from("type T0 = u64\n");
    for n in 1..=14 {
        writeln!(source, "type T{n} = (T{prev}, T{prev})", prev = n - 1).unwrap();
    }
    source.push_str("const SIZE: u64 = bytes[T14]\npub fn size() -> u64 { bytes[T14] }\n");
    let (index, modules) = crate::tests::program_inputs(&source);
    let mut p = Program::declare(&index, &modules);
    assert!(
        p.refusals.is_empty() && p.gaps.is_empty(),
        "{:?} {:?}",
        p.refusals,
        p.gaps
    );

    // Each pair doubles the byte extent and keeps the alignment of u64.
    let expected_size = 8u64 << 14;
    let value = p
        .constants
        .iter()
        .find(|c| c.path.0.name == "SIZE")
        .unwrap()
        .value
        .unwrap();
    assert_eq!(value.data, crate::ConstData::Bits(expected_size));
    let f = p
        .fns
        .iter()
        .position(|f| f.path.0.name == "size" && f.path.package_is("app", "1"))
        .unwrap();
    let check = p.check_lower(f, &mut crate::Emitter::new());
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{check:?}"
    );

    let decl = p.decls.iter().find(|d| d.path.0.name == "T14").unwrap();
    let DeclBody::Alias(ty) = decl.body else {
        panic!("T14 is an alias");
    };
    let mut shapes = vec![];
    let row = temporary_shapes(
        &p.decls,
        &mut p.types,
        &mut shapes,
        &mut ShapeNames::default(),
    )
    .shape(ty)
    .unwrap();
    assert_eq!(shapes.len(), 14);
    let recipes = compute(&shapes).unwrap();
    let hirsel_core_layout::Layout::Fixed(fixed) = &recipes[row.index()].layout else {
        panic!("T14 has fixed layout");
    };
    assert_eq!((fixed.size, fixed.align), (expected_size, 8));
}

struct Prefix {
    row: crate::TypeDecl,
    reads: Arc<AtomicUsize>,
}
impl Provider<crate::TypeDecl> for Prefix {
    fn row(&self, _: u32) -> std::io::Result<crate::TypeDecl> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.row.clone())
    }
}
#[test]
fn recipes_skip_undemanded_published_prefix() {
    let (index, modules) = crate::tests::program_inputs("type Local { n: u64 }\n");
    let mut p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let mut local = p
        .decls
        .iter()
        .find(|d| d.path.0.name == "Local")
        .unwrap()
        .clone();
    local.summary.recipe = None;
    let reads = Arc::new(AtomicUsize::new(0));
    let descriptor = Descriptor {
        namespace: hirsel_core_ids::ContentHash::of_bytes(b"layout-prefix-test"),
        table: 0,
        len: 128,
    };
    let prefix = Table::published(bind(
        descriptor,
        Arc::new(Prefix {
            row: local.clone(),
            reads: Arc::clone(&reads),
        }),
    ));
    p.decls = crate::Rows::overlay(Arc::new(prefix));
    p.decls.push(local);
    recipes(&mut p);
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "unrelated library declarations were decoded"
    );
    let recipe = p.decls[128].summary.recipe.as_ref().unwrap();
    let hirsel_core_layout::Layout::Fixed(fixed) = &recipe.layout else {
        panic!("fixed u64 record")
    };
    assert_eq!((fixed.size, fixed.align), (8, 8));
}
