//! Each test names the bug it catches; every expected number is derived by hand
//! from MEM-9..13 and MEM-26 in the comments, not read back from the code.

use crate::*;

fn field(name: &str, ty: FieldTy) -> Field {
    Field {
        name: name.into(),
        ty,
    }
}
fn record(name: &str, repr: Repr, fields: Vec<Field>) -> Shape {
    Shape {
        name: name.into(),
        repr,
        body: Body::Record(fields),
        finalizer: None,
        grant: 0,
    }
}
fn variant(name: &str, repr: Repr, cases: Vec<(&str, Vec<Field>)>) -> Shape {
    Shape {
        name: name.into(),
        repr,
        body: Body::Variant(
            cases
                .into_iter()
                .map(|(n, fields)| Case {
                    name: n.into(),
                    fields,
                })
                .collect(),
        ),
        finalizer: None,
        grant: 0,
    }
}
fn fixed(r: &Recipe) -> &Fixed {
    match &r.layout {
        Layout::Fixed(f) => f,
        Layout::Compact(_) => panic!("compact"),
    }
}
fn u(s: Scalar) -> FieldTy {
    FieldTy::Scalar(s)
}

/// Bug: padding leaks into the logical charge, or a field is not aligned to its width.
/// `{a: u8, b: u64, c: u16}`: a@0, b@8 (aligned), c@16; size 24 (align 8); logical
/// 1 + 8 + 2 = 11, a Box of it charges round8(11) = 16.
#[test]
fn own_record_aligns_natively_and_charges_logically() {
    let r = compute(&[record(
        "R",
        Repr::Own,
        vec![
            field("a", u(Scalar::U8)),
            field("b", u(Scalar::U64)),
            field("c", u(Scalar::U16)),
        ],
    )])
    .unwrap();
    let f = fixed(&r[0]);
    assert_eq!(
        (f.offsets[0].clone(), f.size, f.align),
        (vec![0, 8, 16], 24, 8)
    );
    assert_eq!(r[0].logical_size, Some(11));
    assert_eq!(charge::boxed(11), Some(16));
    assert!(r[0].plain && r[0].drop.is_none());
}

/// Bug: own variants not reserving the largest payload, or the tag overlapping it.
/// `None | Some(u64) | Pair(u8, u32)`: tag u8@0; payload starts at 8 (max align 8);
/// Some.v@8; Pair: u8@8, u32@12; size 16. Logical (MEM-26: 8 plus the largest
/// payload, whatever the tag width) = 8 + max(0, 8, 5) = 16.
#[test]
fn own_variant_reserves_the_largest_payload_after_its_tag() {
    let r = compute(&[variant(
        "O",
        Repr::Own,
        vec![
            ("None", vec![]),
            ("Some", vec![field("v", u(Scalar::U64))]),
            (
                "Pair",
                vec![field("a", u(Scalar::U8)), field("b", u(Scalar::U32))],
            ),
        ],
    )])
    .unwrap();
    let f = fixed(&r[0]);
    assert_eq!(
        f.tag,
        Some(Tag {
            offset: 0,
            width: 1
        })
    );
    assert_eq!(f.offsets, vec![vec![], vec![8], vec![8, 12]]);
    assert_eq!((f.size, r[0].logical_size), (16, Some(16)));
}

/// Bug: a by-value self-reference accepted (infinite size), or a reference through a
/// List refused. `Chain = End | Link(u64, Chain)` is refused (MEM-13); `Tree {label:
/// String, kids: List[Tree]}` is accepted and its cleanup visits kids, then label.
#[test]
fn by_value_cycles_are_refused_and_collections_break_them() {
    let chain = variant(
        "Chain",
        Repr::Own,
        vec![
            ("End", vec![]),
            (
                "Link",
                vec![
                    field("n", u(Scalar::U64)),
                    field("next", FieldTy::Inline(TypeRef(0))),
                ],
            ),
        ],
    );
    assert_eq!(
        compute(&[chain]),
        Err(LayoutError::Cycle { ty: "Chain".into() })
    );
    let tree = record(
        "Tree",
        Repr::Own,
        vec![
            field("label", FieldTy::Seq(Box::new(u(Scalar::U8)))),
            field("kids", FieldTy::Seq(Box::new(FieldTy::Inline(TypeRef(0))))),
        ],
    );
    let r = compute(&[tree]).unwrap();
    let d = r[0].drop.as_ref().unwrap();
    assert_eq!(
        d.fields[0]
            .iter()
            .map(|f| (f.field, f.offset))
            .collect::<Vec<_>>(),
        vec![(1, 24), (0, 0)]
    );
    assert_eq!(fixed(&r[0]).size, 48);
}

/// Bug: cleanup in declaration order instead of reverse (FAIL-8), a plain field
/// visited, or a resource without finalizer called plain.
/// `resource Pair {a: Task[u64], n: u64, b: List[u8]}`: a@0, n@8, b@16; drops b then a.
#[test]
fn cleanup_visits_owned_fields_in_reverse_declaration_order() {
    let mut s = record(
        "Pair",
        Repr::Resource,
        vec![
            field("a", FieldTy::Handle(Handle::Task)),
            field("n", u(Scalar::U64)),
            field("b", FieldTy::Seq(Box::new(u(Scalar::U8)))),
        ],
    );
    s.finalizer = Some(FuncRef(7));
    let r = compute(&[s]).unwrap();
    let d = r[0].drop.as_ref().unwrap();
    assert_eq!(d.finalizer, Some(FuncRef(7)));
    assert_eq!(
        d.fields[0]
            .iter()
            .map(|f| (f.field, f.offset))
            .collect::<Vec<_>>(),
        vec![(2, 16), (0, 0)]
    );
    assert!(!r[0].plain);
    let bare = record("Lock", Repr::Resource, vec![field("n", u(Scalar::U64))]);
    assert!(
        !compute(&[bare]).unwrap()[0].plain,
        "a resource is never copied"
    );
}

fn le(bytes: &[u8], at: u64, n: u64) -> u64 {
    bytes[at as usize..(at + n) as usize]
        .iter()
        .rev()
        .fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
}

/// Bug: a field after a variable field placed at a constant offset, or a SeqSpan's
/// extent skipped wrongly. `@compact(u32) Entry {id: u64, name: Text, tags:
/// SeqSpan[Text]}` with id = 1, name "abc", tags ["ab", "c"]. `Text` is a
/// `SeqSpan[u8]` (MEM-12): count, extent, bytes.
/// id 8 bytes @0; name @8 = count 3 (4) + extent 3 (4) + "abc" = 11 bytes; tags @19
/// = count 2 (4), extent 19 (4), then "ab" (4+4+2) and "c" (4+4+1) = 19; total
/// 8 + 11 + 27 = 46.
#[test]
fn compact_record_walks_variable_fields_by_their_stored_lengths() {
    let entry = record(
        "Entry",
        Repr::Compact(Width::U32),
        vec![
            field("id", u(Scalar::U64)),
            field("name", FieldTy::Text),
            field("tags", FieldTy::SeqSpan(Box::new(FieldTy::Text))),
        ],
    );
    let r = compute(&[entry]).unwrap();
    let Layout::Compact(set) = &r[0].layout else {
        panic!()
    };
    let c = set.declared();
    assert_eq!(c.fields[0][0].pos, Pos::At(0));
    assert_eq!(c.fields[0][1].pos, Pos::At(8));
    assert_eq!(c.fields[0][2].pos, Pos::After { field: 1, plus: 0 });
    assert_eq!(c.fixed_extent, None);
    let mut b = vec![1, 0, 0, 0, 0, 0, 0, 0];
    b.extend([3, 0, 0, 0, 3, 0, 0, 0, b'a', b'b', b'c']);
    b.extend([2, 0, 0, 0, 19, 0, 0, 0]);
    b.extend([2, 0, 0, 0, 2, 0, 0, 0, b'a', b'b']);
    b.extend([1, 0, 0, 0, 1, 0, 0, 0, b'c']);
    let rw = |at| le(&b, at, 4);
    let rt = |at, n| le(&b, at, n);
    assert_eq!(compact_extent(&r, TypeRef(0), Width::U32, 0, &rw, &rt), 46);
    assert_eq!(
        compact_starts(&r, c, &c.fields[0], 0, &rw, &rt),
        vec![0, 8, 19]
    );
}

/// Bug: compact variants occupying their largest case (MEM-11 says active size), or a
/// link stored at the wrong width. `@compact(u32) E = Num(i64) | Neg(Link[E])`: tag u8;
/// Num = 1 + 8 = 9 bytes; Neg = 1 + 4 = 5 bytes.
#[test]
fn compact_variant_occupies_its_active_case_only() {
    let e = variant(
        "E",
        Repr::Compact(Width::U32),
        vec![
            ("Num", vec![field("n", u(Scalar::I64))]),
            ("Neg", vec![field("x", FieldTy::Link(TypeRef(0)))]),
        ],
    );
    let r = compute(&[e]).unwrap();
    for (bytes, extent) in [
        (&[1u8, 9, 0, 0, 0][..], 5),
        (
            &[0u8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..],
            9,
        ),
    ] {
        let rw = |at| le(bytes, at, 4);
        let rt = |at, n| le(bytes, at, n);
        assert_eq!(
            compact_extent(&r, TypeRef(0), Width::U32, 0, &rw, &rt),
            extent
        );
    }
}

/// Bug: an inline compact value encoded with its own declared width instead of its
/// holder's (MEM-9). `@compact(u16) Name { t: Text }` inline in `@compact(u32) Row
/// { n: Name, k: u8 }`: at u32, Name = count 4 + extent 4 + bytes; for "hi" that is
/// 10 bytes, so k sits at 10 and Row's extent is 11 (at u16 it would be 6 and 7).
#[test]
fn inline_compact_values_use_the_holders_width() {
    let name = record(
        "Name",
        Repr::Compact(Width::U16),
        vec![field("t", FieldTy::Text)],
    );
    let row = record(
        "Row",
        Repr::Compact(Width::U32),
        vec![
            field("n", FieldTy::Inline(TypeRef(0))),
            field("k", u(Scalar::U8)),
        ],
    );
    let r = compute(&[name, row]).unwrap();
    let b = [2u8, 0, 0, 0, 2, 0, 0, 0, b'h', b'i', 7];
    let rw = |at| le(&b, at, 4);
    let rt = |at, n| le(&b, at, n);
    assert_eq!(compact_extent(&r, TypeRef(1), Width::U32, 0, &rw, &rt), 11);
}

/// Bug: representation refusals missing (MEM-9: own field in compact; MEM-17: counted
/// field in stable; a compact value inline in own data).
#[test]
fn representation_refusals() {
    let bad_compact = record(
        "B",
        Repr::Compact(Width::U32),
        vec![field("o", FieldTy::Seq(Box::new(u(Scalar::U8))))],
    );
    assert!(matches!(
        compute(&[bad_compact]),
        Err(LayoutError::Field { .. })
    ));
    let bad_stable = record("S", Repr::Stable, vec![field("c", FieldTy::Rc(TypeRef(0)))]);
    assert!(matches!(
        compute(&[bad_stable]),
        Err(LayoutError::Field { .. })
    ));
    let blob = record(
        "C",
        Repr::Compact(Width::U16),
        vec![field("n", u(Scalar::U8))],
    );
    let holder = record(
        "H",
        Repr::Own,
        vec![field("c", FieldTy::Inline(TypeRef(0)))],
    );
    assert!(matches!(
        compute(&[blob, holder]),
        Err(LayoutError::Field { .. })
    ));
    let empty = record("E", Repr::Compact(Width::U16), vec![]);
    assert!(matches!(compute(&[empty]), Err(LayoutError::Empty { .. })));
    // MEM-2: a plain own record is a legal field of a stable type; a String is not.
    let pair = record("P", Repr::Own, vec![field("a", u(Scalar::U64))]);
    let ok = record(
        "S",
        Repr::Stable,
        vec![field("p", FieldTy::Inline(TypeRef(0)))],
    );
    assert!(compute(&[pair.clone(), ok]).is_ok());
    let s = record(
        "Q",
        Repr::Own,
        vec![field("s", FieldTy::Seq(Box::new(u(Scalar::U8))))],
    );
    let bad = record(
        "S",
        Repr::Stable,
        vec![field("q", FieldTy::Inline(TypeRef(0)))],
    );
    assert!(matches!(compute(&[s, bad]), Err(LayoutError::Field { .. })));
}

/// Bug: charge-v1 transcribed wrongly (MEM-26). Hand values: List of 3 three-byte
/// elements: 9 → 16; Map of 4 entries u64→u64: 4 × (8+8+16) = 128; Rc of a 12-byte
/// payload: 20 → 24; channel of 2 items of 24: 48 + 64 = 112; growth from 0 needing 1 is 4,
/// from 4 needing 5 is 8, from 8 needing 100 is 100; overflow is None, not a wrap.
/// A task with an 8-byte result and `bytes 4096`: 256 + 8 + 32 + 4096 = 4392; a
/// 16-byte resource with `grant bytes 256`: 16 + 32 + 256 = 304; a standalone
/// IndexedSpan of 2 elements and 5 payload bytes: 16 + 16 + 5 = 37 → 40.
#[test]
fn charge_v1_matches_the_spec_table() {
    assert_eq!(charge::seq(3, 3), Some(16));
    assert_eq!(charge::map(4, 8, 8), Some(128));
    assert_eq!(charge::counted(12), Some(24));
    assert_eq!(charge::task(8, 4096), Some(4392));
    assert_eq!(charge::resource(16, 256), Some(304));
    assert_eq!(charge::standalone_span(2, 5, true), Some(40));
    assert_eq!(charge::channel(2, 24), Some(112));
    assert_eq!(charge::grown_capacity(0, 1), Some(4));
    assert_eq!(charge::grown_capacity(4, 5), Some(8));
    assert_eq!(charge::grown_capacity(8, 100), Some(100));
    assert_eq!(charge::seq(u64::MAX, 2), None);
    assert_eq!(charge::round8(u64::MAX - 3), None);
}

/// Bug: a resource is laid out inline in its holder, so its cause cell and grant
/// (FAIL-10, MEM-29) have no block to live in, or a resource without a finalizer or
/// owned fields gets no cleanup and its block, cell and grant are never refunded.
/// `Temp { id: u64 }` held as `{a: u8, t: Temp}`: t is a block pointer @8, size 16.
#[test]
fn a_resource_is_a_cleaned_block_its_holder_points_to() {
    let r = compute(&[
        record("Temp", Repr::Resource, vec![field("id", u(Scalar::U64))]),
        record(
            "H",
            Repr::Own,
            vec![
                field("a", u(Scalar::U8)),
                field("t", FieldTy::Inline(TypeRef(0))),
            ],
        ),
    ])
    .unwrap();
    assert!(r[0].drop.is_some(), "a bare resource still needs cleanup");
    assert_eq!((fixed(&r[1]).offsets[0][1], fixed(&r[1]).size), (8, 16));
    let fields = &r[1]
        .drop
        .as_ref()
        .expect("the holder cleans its resource")
        .fields[0];
    assert_eq!(fields.iter().map(|f| f.field).collect::<Vec<_>>(), [1]);
}
