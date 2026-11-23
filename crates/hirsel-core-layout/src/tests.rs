//! Each test names the bug it catches; every expected number is derived by hand
//! from MEM-9..13 and MEM-24 in the comments, not read back from the code.

use crate::*;

fn field(name: &str, ty: FieldTy) -> Field {
    Field {
        name: name.into(),
        ty,
    }
}
fn record(name: &str, repr: Repr, fields: Vec<Field>) -> Row {
    Row::new(Shape {
        name: name.into(),
        repr,
        carried: Carried::Address,
        body: Body::Record(fields),
        finalizer: None,
        clone: None,
        codec: None,
        grant: 0,
        send: false,
        held: None,
    })
}
fn variant(name: &str, repr: Repr, cases: Vec<(&str, Vec<Field>)>) -> Row {
    Row::new(Shape {
        name: name.into(),
        repr,
        carried: Carried::Address,
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
        clone: None,
        codec: None,
        grant: 0,
        send: false,
        held: None,
    })
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

/// PROG-11 assigns bit j to byte j, including tags next to a matching lane.
/// Using an inexact zero-byte mask can include a neighboring nonmatch.
#[test]
fn map_group_masks_match_byte_loop() {
    use map_index::{
        DELETED, EMPTY, GROUP_WIDTH, match_mask, match_swar, vacant_mask, vacant_swar,
    };

    fn oracle(group: &[u8; GROUP_WIDTH], predicate: impl Fn(u8) -> bool) -> u16 {
        let mut mask = 0;
        for (lane, &byte) in group.iter().enumerate() {
            if predicate(byte) {
                mask |= 1 << lane;
            }
        }
        mask
    }

    fn check(group: &[u8; GROUP_WIDTH], tag: u8) {
        let matches = oracle(group, |byte| byte == tag);
        assert_eq!(match_swar(group, tag), matches);
        assert_eq!(match_mask(group, tag), matches);
        let vacancies = oracle(group, |byte| byte & 0x80 != 0);
        assert_eq!(vacant_swar(group), vacancies);
        assert_eq!(vacant_mask(group), vacancies);
    }

    for tag in 0..=u8::MAX {
        check(&[tag; GROUP_WIDTH], tag);
        check(&[EMPTY; GROUP_WIDTH], tag);
        check(&[DELETED; GROUP_WIDTH], tag);
        let bytes = [
            tag,
            tag.wrapping_add(1),
            tag.wrapping_sub(1),
            DELETED,
            EMPTY,
        ];
        let mut group = std::array::from_fn(|i| bytes[i % bytes.len()]);
        check(&group, tag);
        group.reverse();
        check(&group, tag);
        for lane in 0..GROUP_WIDTH {
            for byte in 0..=u8::MAX {
                group[lane] = byte;
                check(&group, tag);
            }
            group[lane] = bytes[(GROUP_WIDTH - 1 - lane) % bytes.len()];
        }
    }
}

/// Bug: padding leaks into the logical charge, or a field is not aligned to its width.
/// `{a: u8, b: u64, c: u16}`: b@0, c@8, a@10; size 16 (align 8); logical
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
        (vec![10, 0, 8], 16, 8)
    );
    assert_eq!(r[0].logical_size, Some(11));
    assert_eq!(charge::boxed(11), Some(16));
    assert!(r[0].plain && r[0].drop.is_none());
}

/// Bug: own variants not reserving the largest payload, or the tag overlapping it.
/// `None | Some(u64) | Pair(u8, u32)`: tag u8@0; payload starts at 8 (max align 8);
/// Some.v@8; Pair: u32@8, u8@12; size 16. Logical (MEM-24: 8 plus the largest
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
            width: 1,
            encoding: Encoding::Direct,
        })
    );
    assert_eq!(f.offsets, vec![vec![], vec![8], vec![12, 8]]);
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

/// Bug: cleanup in declaration order instead of reverse (FAIL-8) where a
/// finalizer, a handle or a refund to another task's limit could observe it
/// (SEM-8), a plain field visited, or a resource without finalizer called plain.
/// `resource Pair {a: Task[u64], n: u64, b: List[u8]}`: a@0, n@8, b@16; drops b then a.
/// `Outer {p: Box[Pair], s: List[u8]}` reaches the resource only through a box,
/// still observably: drops s@0 then p@24. `Sent {x: List[u8], y: Box[Plain2]}`:
/// a `List[u8]` is `send`, so its storage may refund to another task's limit,
/// an event (MEM-20): drops y@24 then x@0. `Plain {x: Box[Plain2], y:
/// Box[Plain2]}` with `Plain2` not `send` frees blocks to its own task's limits
/// only, so it cleans in construction order: x@0 then y@8.
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
    Row::make_mut(&mut s).finalizer = Some(FuncRef(7));
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
    let string = || FieldTy::Seq(Box::new(u(Scalar::U8)));
    let order = |shapes: &[Row], t: usize| -> Vec<(u32, u64)> {
        let r = compute(shapes).unwrap();
        r[t].drop.as_ref().unwrap().fields[0]
            .iter()
            .map(|f| (f.field, f.offset))
            .collect()
    };
    let pair = record(
        "Pair",
        Repr::Resource,
        vec![
            field("a", FieldTy::Handle(Handle::Task)),
            field("n", u(Scalar::U64)),
            field("b", string()),
        ],
    );
    let outer = record(
        "Outer",
        Repr::Own,
        vec![field("p", FieldTy::Box(TypeRef(0))), field("s", string())],
    );
    assert_eq!(order(&[pair, outer], 1), vec![(1, 0), (0, 24)]);
    let plain2 = || {
        record(
            "Plain2",
            Repr::Own,
            vec![field("z", FieldTy::Rc(TypeRef(1)))],
        )
    };
    let sent = record(
        "Sent",
        Repr::Own,
        vec![field("x", string()), field("y", FieldTy::Box(TypeRef(1)))],
    );
    assert_eq!(order(&[sent, plain2()], 0), vec![(1, 24), (0, 0)]);
    let plain = record(
        "Plain",
        Repr::Own,
        vec![
            field("x", FieldTy::Box(TypeRef(1))),
            field("y", FieldTy::Box(TypeRef(1))),
        ],
    );
    assert_eq!(order(&[plain, plain2()], 0), vec![(0, 0), (1, 8)]);
    // The map is not send. Its key's cleanup is order-free, but the
    // value's cleanup reaches a task handle and makes the outer order visible.
    let map = FieldTy::Map(
        Box::new(FieldTy::Box(TypeRef(1))),
        Box::new(FieldTy::Box(TypeRef(2))),
    );
    let key = record("Key", Repr::Own, vec![field("n", u(Scalar::U64))]);
    let value = record(
        "Value",
        Repr::Own,
        vec![field("task", FieldTy::Handle(Handle::Task))],
    );
    let table = record(
        "Table",
        Repr::Own,
        vec![
            field("map", map),
            field("scratch", FieldTy::Box(TypeRef(1))),
        ],
    );
    assert_eq!(order(&[table, key, value], 0), vec![(1, 24), (0, 0)]);

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

/// Bug: a field after a variable field placed at a constant offset, or a Text or
/// SeqSpan skipped wrongly. `@compact(u32) Entry {id: u64, name: Text, tags:
/// SeqSpan[Text]}` with id = 1, name "abc", tags ["ab", "c"]. `Text` is a `W`
/// byte count, then the bytes (MEM-12); SeqSpan is count, byte length, elements.
/// id 8 bytes @0; name @8 = count 3 (4) + "abc" = 7 bytes; tags @15 = count 2 (4),
/// extent 11 (4), then "ab" (4+2) and "c" (4+1) = 19; total 8 + 7 + 19 = 34.
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
    b.extend([3, 0, 0, 0, b'a', b'b', b'c']);
    b.extend([2, 0, 0, 0, 11, 0, 0, 0]);
    b.extend([2, 0, 0, 0, b'a', b'b']);
    b.extend([1, 0, 0, 0, b'c']);
    let rw = |at| le(&b, at, 4);
    let rt = |at, n| le(&b, at, n);
    assert_eq!(compact_extent(&r, TypeRef(0), Width::U32, 0, &rw, &rt), 34);
    assert_eq!(
        compact_starts(&r, c, &c.fields[0], 0, &rw, &rt),
        vec![0, 8, 15]
    );
}

/// Bug: a plain element encoded at its native, padded stride (MEM-12 packs it as its
/// fields back to back), or an element that is not packed accepted. `Pt {x: u16, y:
/// u8}` (native size 4) in `@compact(u32) S {xs: FixedSpan[Pt], ys: SeqSpan[Pt]}`:
/// xs is count 2 (4) + 2 × 3 = 10 bytes; ys @10 is count 1 (4), length 3 (4) and 3
/// bytes; extent 21. Refused: an empty record and a link as elements, a plain record
/// as a compact field (MEM-9), and a compact target referenced other than by locator.
#[test]
fn plain_elements_are_packed_and_only_packed_types_are_elements() {
    let pt = record(
        "Pt",
        Repr::Own,
        vec![field("x", u(Scalar::U16)), field("y", u(Scalar::U8))],
    );
    let pt_ = |c: fn(Box<FieldTy>) -> FieldTy| c(Box::new(FieldTy::Inline(TypeRef(0))));
    let s = record(
        "S",
        Repr::Compact(Width::U32),
        vec![
            field("xs", pt_(FieldTy::FixedSpan)),
            field("ys", pt_(FieldTy::SeqSpan)),
        ],
    );
    let r = compute(&[pt.clone(), s]).unwrap();
    let Layout::Compact(set) = &r[1].layout else {
        panic!()
    };
    let c = set.declared();
    let mut b = vec![2, 0, 0, 0, 1, 0, 9, 2, 0, 8];
    b.extend([1, 0, 0, 0, 3, 0, 0, 0, 7, 0, 6]);
    let rw = |at| le(&b, at, 4);
    let rt = |at, n| le(&b, at, n);
    assert_eq!(compact_extent(&r, TypeRef(1), Width::U32, 0, &rw, &rt), 21);
    assert_eq!(
        compact_starts(&r, c, &c.fields[0], 0, &rw, &rt),
        vec![0, 10]
    );
    let refused = |elem: FieldTy, extra: Row| {
        let s = record("S", Repr::Compact(Width::U32), vec![field("xs", elem)]);
        assert!(matches!(
            compute(&[extra, s]),
            Err(LayoutError::Field { .. })
        ));
    };
    refused(
        pt_(FieldTy::SeqSpan).clone(),
        record("E", Repr::Own, vec![]),
    );
    refused(
        FieldTy::SeqSpan(Box::new(FieldTy::Link(TypeRef(1)))),
        pt.clone(),
    );
    refused(FieldTy::Inline(TypeRef(0)), pt.clone());
    // A scalar's own row `{0: u64}` held inline is a plain record too: the front and a
    // generic instance both write a direct scalar field as `Scalar` (FORM #109), so a
    // relaxation of MEM-9 taking a one-scalar record as its scalar must decide
    // whether that row lays out as the scalar before it can change a layout.
    refused(
        FieldTy::Inline(TypeRef(0)),
        record("u64", Repr::Own, vec![field("0", u(Scalar::U64))]),
    );
    let blob = record(
        "C",
        Repr::Compact(Width::U16),
        vec![field("n", u(Scalar::U8))],
    );
    let by_ptr = record(
        "H",
        Repr::Region,
        vec![field("c", FieldTy::RegionRef(TypeRef(0)))],
    );
    assert!(matches!(
        compute(&[blob.clone(), by_ptr]),
        Err(LayoutError::Field { .. })
    ));
    let located = |t: FieldTy| {
        record(
            "H",
            Repr::Region,
            vec![field("c", FieldTy::Locator(Box::new(t)))],
        )
    };
    let r = compute(&[blob.clone(), located(FieldTy::Inline(TypeRef(0)))]).unwrap();
    assert_eq!((fixed(&r[1]).size, r[1].logical_size), (16, Some(16)));
    // A locator names compact data (#63): a scalar or a native own type is not that.
    let own = record("O", Repr::Own, vec![field("n", u(Scalar::U8))]);
    for target in [u(Scalar::U64), FieldTy::Inline(TypeRef(2))] {
        assert!(matches!(
            compute(&[blob.clone(), located(target), own.clone()]),
            Err(LayoutError::Field { .. })
        ));
    }
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
/// { n: Name, k: u8 }`: at u32, Name = count 4 + bytes; for "hi" that is 6 bytes,
/// so k sits at 6 and Row's extent is 7 (at u16 it would be 4 and 5).
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
    let b = [2u8, 0, 0, 0, b'h', b'i', 7];
    let rw = |at| le(&b, at, 4);
    let rt = |at, n| le(&b, at, n);
    assert_eq!(compact_extent(&r, TypeRef(1), Width::U32, 0, &rw, &rt), 7);
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
    let bad_stable = record("S", Repr::Region, vec![field("c", FieldTy::Rc(TypeRef(0)))]);
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
    // MEM-2: a plain own record is a legal field of a region block; a String is not.
    let pair = record("P", Repr::Own, vec![field("a", u(Scalar::U64))]);
    let ok = record(
        "S",
        Repr::Region,
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
        Repr::Region,
        vec![field("q", FieldTy::Inline(TypeRef(0)))],
    );
    assert!(matches!(compute(&[s, bad]), Err(LayoutError::Field { .. })));
}

/// Bug: charge-v1 transcribed wrongly (MEM-24). Hand values: List of 3 three-byte
/// elements: 9 → 16; Map of 4 entries u64→u64: 4 × (8+8+16) = 128; Rc of a 12-byte
/// payload: 20 → 24; channel of 2 items of 24: 48 + 64 = 112; growth from 0 needing 1 is 4,
/// from 4 needing 5 is 8, from 8 needing 100 is 100; overflow is None, not a wrap.
/// A task with an 8-byte result and `bytes 4096`: 256 + 8 + 32 + 4096 = 4392; a
/// 16-byte resource with `grant bytes 256`: 16 + 32 + 256 = 304; a standalone Text
/// of 5 bytes: 8 + 5 = 13 → 16; a span builder of capacity 3 for u32: 12 + 8 = 20 →
/// 24; `seq.index_in` of 3 elements: 24 + 3 × 8 = 48.
#[test]
fn charge_v1_matches_the_spec_table() {
    assert_eq!(charge::seq(3, 3), Some(16));
    assert_eq!(charge::map(4, 8, 8), Some(128));
    assert_eq!(charge::counted(12), Some(24));
    assert_eq!(charge::task(8, 4096), Some(4392));
    assert_eq!(charge::resource(16, 256), Some(304));
    assert_eq!(charge::standalone(5), Some(16));
    assert_eq!(charge::span_builder(3, 4), Some(24));
    assert_eq!(charge::channel(2, 24), Some(112));
    assert_eq!(charge::seq_index(3), Some(48));
    assert_eq!(charge::grown_capacity(0, 1), Some(4));
    assert_eq!(charge::grown_capacity(4, 5), Some(8));
    assert_eq!(charge::grown_capacity(8, 100), Some(100));
    assert_eq!(charge::seq(u64::MAX, 2), None);
    assert_eq!(charge::round8(u64::MAX - 3), None);
    // FORM #113, hand-derived floor((2^64 - 8) / s) for logical size s: a bound one too
    // high would let LLVM assume a length the admission (MEM-22, MEM-24) never grants.
    // s = 3: 3 × 6148914691236517202 = 2^64 - 10, rounds to 2^64 - 8; one more is past 2^64 - 1.
    // s = 9 is a (u64, u8) record (stride 16): the spec's bound is by the 9, not the 16.
    for (s, bound) in [
        (2, 9223372036854775804),
        (3, 6148914691236517202),
        (8, 2305843009213693951),
        (9, 2049638230412172400),
        (24, 768614336404564650),
    ] {
        assert_eq!(charge::seq_len_bound(s), Some(bound), "size {s}");
        assert!(charge::seq(bound, s).is_some(), "size {s} admits its bound");
        assert_eq!(charge::seq(bound + 1, s), None, "size {s} refuses one past");
    }
    assert_eq!(charge::seq_len_bound(1), None);
    assert_eq!(charge::seq_len_bound(0), None);
}

/// Bug: a resource is laid out inline in its holder, so its cause cell and grant
/// (FAIL-10, MEM-27) have no block to live in, or a resource without a finalizer or
/// owned fields gets no cleanup and its block, cell and grant are never refunded.
/// `Temp { id: u64 }` held as `{a: u8, t: Temp}`: t is a block pointer @0, size 16.
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
    assert_eq!((fixed(&r[1]).offsets[0][1], fixed(&r[1]).size), (0, 16));
    let fields = &r[1]
        .drop
        .as_ref()
        .expect("the holder cleans its resource")
        .fields[0];
    assert_eq!(fields.iter().map(|f| f.field).collect::<Vec<_>>(), [1]);
}

/// Bug: an inline array laid out or charged as one element, at the wrong stride, or
/// not aligned to its element (SEM-9). `{a: u8, xs: Inline[u32, 3], b: u16}`: xs @0
/// (align 4) for 12 bytes, b @12, a @14, size 15 rounded to 16; logical 1 + 12 + 2 = 15. An
/// array of a record needing cleanup needs cleanup; of resources or in a compact
/// type it is refused.
#[test]
fn inline_arrays_hold_n_elements_without_a_block() {
    let arr = |e, n| FieldTy::Array(Box::new(e), n);
    let r = compute(&[record(
        "A",
        Repr::Own,
        vec![
            field("a", u(Scalar::U8)),
            field("xs", arr(u(Scalar::U32), 3)),
            field("b", u(Scalar::U16)),
        ],
    )])
    .unwrap();
    assert_eq!(fixed(&r[0]).offsets[0], [14, 0, 12]);
    assert_eq!((fixed(&r[0]).size, r[0].logical_size), (16, Some(15)));
    assert!(r[0].plain);
    let owner = record(
        "O",
        Repr::Own,
        vec![field("s", FieldTy::Seq(Box::new(u(Scalar::U8))))],
    );
    let holds = record(
        "H",
        Repr::Own,
        vec![field("os", arr(FieldTy::Inline(TypeRef(0)), 2))],
    );
    let r = compute(&[owner.clone(), holds]).unwrap();
    assert_eq!(r[1].logical_size, Some(48));
    assert!(r[1].drop.is_some(), "an array of owners is cleaned");
    let temp = record("T", Repr::Resource, vec![field("id", u(Scalar::U64))]);
    let bad = record(
        "H",
        Repr::Own,
        vec![field("ts", arr(FieldTy::Inline(TypeRef(0)), 2))],
    );
    assert!(matches!(
        compute(&[temp, bad]),
        Err(LayoutError::Field { .. })
    ));
    let bad = record(
        "C",
        Repr::Compact(Width::U32),
        vec![field("xs", arr(u(Scalar::U8), 2))],
    );
    assert!(matches!(compute(&[bad]), Err(LayoutError::Field { .. })));
}

/// Map cleanup uses the same entry offsets and stride as the front's tuple.
/// For u8/u64, meta @0, value @8, key @16, stride 24.
/// For String/u16, the 24-byte key comes first, meta @24, value @32, stride 40.
/// For u16/u8, meta @0, key @8, value @10, stride 16.
#[test]
fn map_entries_share_the_own_tuple_layout() {
    let s = || FieldTy::Seq(Box::new(u(Scalar::U8)));
    let e = |k, v| map_entry(&[], &[], &k, &v);
    let want = |stride, meta, key, value| MapEntry {
        stride,
        align: 8,
        meta,
        key,
        value,
    };
    assert_eq!(e(u(Scalar::U8), u(Scalar::U64)), want(24, 0, 16, 8));
    assert_eq!(e(s(), u(Scalar::U16)), want(40, 24, 0, 32));
    assert_eq!(e(u(Scalar::U16), u(Scalar::U8)), want(16, 0, 8, 10));
}

/// MEM-12: sequence headers terminate inline containment. J = Null | Arr(Seq[J])
/// contains [Null, Arr([Null])]. With W bytes, the sequential encoding occupies
/// 4 + 4W bytes; indexing adds the inner one-entry and outer two-entry trailers,
/// giving 4 + 7W. Neither recipe may depend on J's already having been computed.
#[test]
fn recursive_sequences_end_layout_dependencies() {
    for indexed in [false, true] {
        let elements = Box::new(FieldTy::Inline(TypeRef(0)));
        let sequence = if indexed {
            FieldTy::IndexedSpan(elements)
        } else {
            FieldTy::SeqSpan(elements)
        };
        let recipes = compute(&[variant(
            "J",
            Repr::Compact(Width::U32),
            vec![("Null", vec![]), ("Arr", vec![field("items", sequence)])],
        )])
        .unwrap();
        for (width, word, sequential, indexed_size) in [
            (Width::U16, 2, 12, 18),
            (Width::U32, 4, 20, 32),
            (Width::U64, 8, 36, 60),
        ] {
            let bytes = if indexed { 3 + 3 * word } else { 3 + 2 * word };
            let read = |offset| match offset {
                1 => 2,
                n if n == 1 + word => bytes,
                _ => panic!("unexpected header offset {offset}"),
            };
            let extent = compact_extent(&recipes, TypeRef(0), width, 0, &read, &|_, _| 1);
            assert_eq!(extent, if indexed { indexed_size } else { sequential });
            assert_eq!(
                compact_extent(
                    &recipes,
                    TypeRef(0),
                    width,
                    0,
                    &|_| panic!("Null has no header"),
                    &|_, _| 0
                ),
                1
            );
        }
    }
}

/// Appending shapes must retain correct prefix references, preserve recursive compact
/// sequences, and leave the prior table usable when a later shape is refused.
#[test]
fn appended_layouts_keep_prefix_dependencies_and_refuse_atomically() {
    let mut shapes = vec![record("Word", Repr::Own, vec![field("n", u(Scalar::U64))])];
    let mut recipes = compute(&shapes).unwrap();
    shapes.push(record(
        "Pair",
        Repr::Own,
        vec![
            field("word", FieldTy::Inline(TypeRef(0))),
            field("byte", u(Scalar::U8)),
        ],
    ));
    extend(&shapes, &mut recipes).unwrap();
    // Native padding makes 16 bytes, while portable charge is exactly 8 + 1.
    assert_eq!(fixed(&recipes[1]).size, 16);
    assert_eq!(fixed(&recipes[1]).offsets, vec![vec![0, 8]]);
    assert_eq!(recipes[1].logical_size, Some(9));
    shapes.push(variant(
        "J",
        Repr::Compact(Width::U32),
        vec![
            ("Null", vec![]),
            (
                "Arr",
                vec![field(
                    "items",
                    FieldTy::SeqSpan(Box::new(FieldTy::Inline(TypeRef(2)))),
                )],
            ),
        ],
    ));
    extend(&shapes, &mut recipes).unwrap();
    assert_eq!(
        compact_extent(
            &recipes,
            TypeRef(2),
            Width::U32,
            0,
            &|_| panic!("Null has no sequence header"),
            &|_, _| 0
        ),
        1
    );
    let before = recipes.clone();
    shapes.push(record(
        "Cycle",
        Repr::Own,
        vec![field("self", FieldTy::Inline(TypeRef(3)))],
    ));
    assert_eq!(
        extend(&shapes, &mut recipes),
        Err(LayoutError::Cycle { ty: "Cycle".into() })
    );
    assert_eq!(recipes, before);
    shapes.pop();
    shapes.push(record("Short", Repr::Own, vec![field("n", u(Scalar::U16))]));
    extend(&shapes, &mut recipes).unwrap();
    assert_eq!(fixed(&recipes[3]).size, 2);
    assert_eq!(recipes, compute(&shapes).unwrap());
}

#[test]
fn appended_layout_visit_state_is_linear_and_retry_resets_the_suffix() {
    let mut scratch = RecipeScratch::default();
    let mut shapes = Vec::new();
    let mut recipes = Vec::new();
    for n in 0..1024 {
        shapes.push(record(
            &format!("T{n}"),
            Repr::Own,
            vec![field("n", u(Scalar::U64))],
        ));
        scratch.extend(&shapes, &mut recipes).unwrap();
    }
    assert_eq!(scratch.initialized_rows, 1024);
    assert_eq!(recipes, compute(&shapes).unwrap());
    let before = recipes.clone();
    shapes.push(record(
        "Forward",
        Repr::Own,
        vec![field("next", FieldTy::Inline(TypeRef(1025)))],
    ));
    shapes.push(record(
        "Cycle",
        Repr::Own,
        vec![field("self", FieldTy::Inline(TypeRef(1025)))],
    ));
    assert_eq!(
        scratch.extend(&shapes, &mut recipes),
        Err(LayoutError::Cycle { ty: "Cycle".into() })
    );
    assert_eq!(recipes, before);
    shapes[1025] = record(
        "Prefix",
        Repr::Own,
        vec![field("first", FieldTy::Inline(TypeRef(0)))],
    );
    scratch.extend(&shapes, &mut recipes).unwrap();
    assert_eq!(recipes, compute(&shapes).unwrap());
    assert_eq!(scratch.initialized_rows, 1028);

    let first = shapes.len();
    let key = TypeRef(0);
    let local_box = || FieldTy::Box(key);
    shapes.push(record(
        "Order",
        Repr::Own,
        vec![
            field("a", local_box()),
            field("b", FieldTy::Seq(Box::new(u(Scalar::U8)))),
        ],
    ));
    let mut invalid = record("Invalid", Repr::Own, vec![field("n", u(Scalar::U64))]);
    Row::make_mut(&mut invalid).carried = Carried::Nothing;
    shapes.push(invalid);
    assert_eq!(
        scratch.extend(&shapes, &mut recipes),
        Err(LayoutError::Carried {
            ty: "Invalid".into()
        })
    );
    assert_eq!(recipes.len(), first);
    shapes.pop();
    shapes[first] = record(
        "Order",
        Repr::Own,
        vec![field("a", local_box()), field("b", local_box())],
    );
    scratch.extend(&shapes, &mut recipes).unwrap();
    let steps = &recipes[first].drop.as_ref().unwrap().fields[0];
    assert_eq!([steps[0].field, steps[1].field], [0, 1]);
}

/// Bug: a `View[String]` field is laid out as a pointer (8) instead of the String's
/// own carrier, or is cleaned (a double free of the owner's block), or is allowed in
/// a type that outlives its owner. `scoped Parser {s: View[String], n: u8}`: s@0 is a
/// carrier {data, len, cap} of 24 bytes, n@24, size 32 (align 8), and nothing to
/// clean; the same field in an own record is refused (core.md: scoped values are
/// never stored in own fields).
#[test]
fn a_view_field_is_its_types_bytes_and_is_never_cleaned() {
    let view = || FieldTy::View(Box::new(FieldTy::Seq(Box::new(u(Scalar::U8)))));
    let p = record(
        "Parser",
        Repr::Scoped,
        vec![field("s", view()), field("n", u(Scalar::U8))],
    );
    let r = compute(&[p]).unwrap();
    let f = fixed(&r[0]);
    assert_eq!((f.size, f.align, &f.offsets[0][..]), (32, 8, &[0, 24][..]));
    assert!(r[0].drop.is_none(), "a view is cleaned by nobody");
    let own = record("Keep", Repr::Own, vec![field("s", view())]);
    assert!(compute(&[own]).is_err());
}

/// Bug: a template's `View[$i]` instantiated at a compact type is refused or laid out
/// as an inline value, though a compact value lives only in a blob and its view is its
/// locator `{blob, at}` (16 bytes, align 8). `scoped R {v: View[C], n: u8}` with a
/// compact `C`: v@0, n@16, size 24.
#[test]
fn a_view_of_a_compact_type_is_its_locator() {
    let c = record(
        "C",
        Repr::Compact(Width::U32),
        vec![field("t", FieldTy::Text)],
    );
    let r = record(
        "R",
        Repr::Scoped,
        vec![
            field("v", FieldTy::View(Box::new(FieldTy::Inline(TypeRef(0))))),
            field("n", u(Scalar::U8)),
        ],
    );
    let out = compute(&[c, r]).unwrap();
    let f = fixed(&out[1]);
    assert_eq!((f.size, f.align, &f.offsets[0][..]), (24, 8, &[0, 16][..]));
}

/// Bug: a `List[()]` is cleaned like a list with storage, freeing a block that was
/// never admitted (SEM-4: a list of a zero-sized element has no element storage and
/// capacity 2^64 - 1), or a list of a sized element loses its cleanup.
/// `{units: List[()], n: u64}` needs no cleanup; `{bytes: List[u8]}` frees its block.
#[test]
fn a_list_of_a_zero_sized_element_has_nothing_to_clean() {
    let unit = record("Unit", Repr::Own, vec![]);
    let seq = |e| FieldTy::Seq(Box::new(e));
    let units = record(
        "Units",
        Repr::Own,
        vec![
            field("units", seq(FieldTy::Inline(TypeRef(0)))),
            field("n", u(Scalar::U64)),
        ],
    );
    let bytes = record("Bytes", Repr::Own, vec![field("b", seq(u(Scalar::U8)))]);
    let r = compute(&[unit, units, bytes]).unwrap();
    assert_eq!(fixed(&r[0]).size, 0, "() has size 0");
    assert!(r[1].drop.is_none());
    assert!(r[2].drop.is_some());
    // The element declared after its holder is not computed yet when the holder is.
    let late = record(
        "Late",
        Repr::Own,
        vec![field("u", seq(FieldTy::Inline(TypeRef(1))))],
    );
    let r = compute(&[late, record("Unit", Repr::Own, vec![])]).unwrap();
    assert!(r[0].drop.is_none());
}

/// FORM #64. Bug: a row published as carried by value whose body is not that one value
/// (two words, a narrower field, a variant, a field for `nothing`) would make a
/// generic instance pass a place's first bytes as the value. Each such row is refused;
/// the one-field rows that are the value, and any address row, are laid out.
#[test]
fn a_row_carried_unlike_its_body_is_refused() {
    let carried = |c: Carried, mut s: Row| {
        Row::make_mut(&mut s).carried = c;
        compute(&[s])
    };
    let u64v = Carried::Value(Ty::Scalar(Scalar::U64));
    let word = |s: Scalar| Carried::Value(Ty::Scalar(s));
    let enum3 = || {
        variant(
            "D",
            Repr::Own,
            vec![("A", vec![]), ("B", vec![]), ("C", vec![])],
        )
    };
    let one = |t: FieldTy| record("R", Repr::Own, vec![field("x", t)]);
    for (c, s) in [
        (u64v, one(u(Scalar::U64))),
        (Carried::Value(Ty::Ptr), one(FieldTy::Fn)),
        (
            Carried::Value(Ty::Ptr),
            record("W", Repr::Scoped, vec![field("f", FieldTy::Borrow)]),
        ),
        (Carried::Nothing, record("()", Repr::Own, vec![])),
        (Carried::Address, one(u(Scalar::U64))),
        // FORM #78: a variant whose bytes are one word is that word.
        (word(Scalar::U8), enum3()),
        (word(Scalar::U8), option("Option[bool]", u(Scalar::Bool))),
        (word(Scalar::U32), option("Option[char]", u(Scalar::Char))),
    ] {
        assert!(carried(c, s).is_ok(), "{c:?}");
    }
    let refused = [
        (
            u64v,
            record(
                "P",
                Repr::Own,
                vec![field("a", u(Scalar::U64)), field("b", u(Scalar::U64))],
            ),
        ),
        (u64v, one(u(Scalar::U32))),
        (
            u64v,
            variant(
                "V",
                Repr::Own,
                vec![("A", vec![field("x", u(Scalar::U64))])],
            ),
        ),
        (Carried::Value(Ty::Ptr), one(u(Scalar::U64))),
        (Carried::Nothing, one(u(Scalar::U64))),
        // Not its word: a three-case tag is a u8; `Option[char]` needs all 32 bits; a
        // u32 payload or a `fn` leaves no spare value for `None`.
        (word(Scalar::U32), enum3()),
        (word(Scalar::U8), option("Option[char]", u(Scalar::Char))),
        (word(Scalar::U32), option("Option[u32]", u(Scalar::U32))),
        (Carried::Value(Ty::Ptr), option("Option[fn]", FieldTy::Fn)),
        (word(Scalar::U64), option("Option[fn]", FieldTy::Fn)),
        // Its null niche fills a u64 with the tag at 0, yet a Box is no word field.
        (
            word(Scalar::U64),
            option("Option[Box]", FieldTy::Box(TypeRef(0))),
        ),
    ];
    for (c, s) in refused {
        assert!(
            matches!(carried(c, s), Err(LayoutError::Carried { .. })),
            "{c:?}"
        );
    }
}

fn option(name: &str, payload: FieldTy) -> Row {
    variant(
        name,
        Repr::Own,
        vec![("None", vec![]), ("Some", vec![field("v", payload)])],
    )
}

/// Bug: a niche layout that is not the payload's own size (the tag still reserved),
/// a niche taken from a type with no spare values, or a charge derived from the
/// niche layout (#72: charges stay "8 plus the largest payload", MEM-24). By the
/// niche table: a Box's null makes `Option[Box]` 8 bytes, charged 8 + 8 = 16; a
/// String's capacity word makes `Option[String]` 24; u64 and u8 have none, so a u8
/// tag at 0 and the payload at the payload's alignment: 16 and 2. A node of three
/// u64s and two `Option[Arc[Node]]` links is 3 × 8 + 2 × 16 = 56 bytes tagged and
/// 3 × 8 + 2 × 8 = 40 with niches.
#[test]
fn a_one_payload_variant_takes_its_payloads_size_and_keeps_its_charge() {
    let shapes = [
        option("Option[Box]", FieldTy::Box(TypeRef(0))),
        option("Option[String]", FieldTy::Seq(Box::new(u(Scalar::U8)))),
        option("Option[u64]", u(Scalar::U64)),
        option("Option[u8]", u(Scalar::U8)),
        option("Option[Arc[Node]]", FieldTy::Arc(TypeRef(5))),
        record(
            "Node",
            Repr::Own,
            vec![
                field("key", u(Scalar::U64)),
                field("value", u(Scalar::U64)),
                field("left", FieldTy::Inline(TypeRef(4))),
                field("right", FieldTy::Inline(TypeRef(4))),
                field("size", u(Scalar::U64)),
            ],
        ),
    ];
    let r = compute(&shapes).unwrap();
    let sizes: Vec<u64> = r.iter().map(|r| fixed(r).size).collect();
    assert_eq!(sizes, vec![8, 24, 16, 2, 8, 40]);
    assert_eq!(r[0].logical_size, Some(16));
    assert_eq!(fixed(&r[5]).offsets[0], vec![0, 8, 16, 24, 32]);
}

/// Bug: a niche word written or read at the wrong value, or a nested variant whose
/// empty case collides with its payload's. `Option[Option[bool]]`, by the table:
/// bool spares 2..=255; the inner None is 2, the outer None the next spare, 3; all
/// one byte. Reading the outer case of an inner None (word 2) must give Some: 2 - 3
/// wraps within the byte to 255, past the one empty case. `Option[String]`'s None is
/// capacity 2^63 at 16; `Option[char]`'s is 0x110000.
#[test]
fn niche_words_round_trip_through_read_case_and_case_word() {
    let shapes = [
        option("Option[bool]", u(Scalar::Bool)),
        option("Option[Option[bool]]", FieldTy::Inline(TypeRef(0))),
        option("Option[String]", FieldTy::Seq(Box::new(u(Scalar::U8)))),
        option("Option[char]", u(Scalar::Char)),
    ];
    let r = compute(&shapes).unwrap();
    let tag = |i: usize| fixed(&r[i]).tag.unwrap();
    let (inner, outer) = (tag(0), tag(1));
    assert_eq!((fixed(&r[1]).size, outer.offset, outer.width), (1, 0, 1));
    assert_eq!(
        (case_word(&inner, 0), case_word(&inner, 1)),
        (Some(2), None)
    );
    assert_eq!(
        (case_word(&outer, 0), case_word(&outer, 1)),
        (Some(3), None)
    );
    // outer None, outer Some(inner None), outer Some(Some(false)), Some(Some(true)).
    assert_eq!([3, 2, 0, 1].map(|w| case_of(&outer, w)), [0, 1, 1, 1]);
    // Inside an outer Some, the same byte is the inner value.
    assert_eq!([2, 0, 1].map(|w| case_of(&inner, w)), [0, 1, 1]);
    let string = tag(2);
    assert_eq!((string.offset, string.width), (16, 8));
    assert_eq!(case_word(&string, 0), Some(1 << 63));
    // A zero-sized element's list holds capacity 2^64 - 1 (SEM-4): still Some.
    assert_eq!(
        [1 << 63, u64::MAX, 0, 7].map(|w| case_of(&string, w)),
        [0, 1, 1, 1]
    );
    let char_tag = tag(3);
    assert_eq!(case_word(&char_tag, 0), Some(0x11_0000));
    assert_eq!(
        [0x11_0000, 0x10_FFFF].map(|w| case_of(&char_tag, w)),
        [0, 1]
    );
}

/// Bug: a variant with several dataful cases stays tagged (Calculator's `Node` at 40
/// bytes where Rust's is 32), or another case's field overlaps the tag word, so
/// writing that case's payload rewrites its case. Hand-derived: `Assignment(String,
/// Box)` is the largest (32); with its String last its capacity at 24 is the niche,
/// and Number (8), Variable (24) and Binary (u8, Box, Box: 24) fit before it. The
/// charge stays 8 plus the largest payload, 40 (MEM-24). `Two(u64) | Three(u64, u64)`
/// has no niche and stays tagged at 24.
#[test]
fn several_dataful_cases_share_one_niche_without_touching_it() {
    let string = || FieldTy::Seq(Box::new(u(Scalar::U8)));
    let node = || FieldTy::Box(TypeRef(0));
    let shapes = [
        variant(
            "Node",
            Repr::Own,
            vec![
                ("Number", vec![field("n", u(Scalar::I64))]),
                ("Variable", vec![field("name", string())]),
                (
                    "Binary",
                    vec![
                        field("op", u(Scalar::U8)),
                        field("l", node()),
                        field("r", node()),
                    ],
                ),
                (
                    "Assignment",
                    vec![field("name", string()), field("v", node())],
                ),
            ],
        ),
        variant(
            "Plain",
            Repr::Own,
            vec![
                ("Two", vec![field("a", u(Scalar::U64))]),
                (
                    "Three",
                    vec![field("a", u(Scalar::U64)), field("b", u(Scalar::U64))],
                ),
            ],
        ),
    ];
    let r = compute(&shapes).unwrap();
    let f = fixed(&r[0]);
    let tag = f.tag.unwrap();
    assert_eq!((f.size, tag.offset, tag.width), (32, 24, 8));
    assert_eq!(r[0].logical_size, Some(40));
    assert_eq!(f.offsets[3], vec![8, 0]);
    let sizes = [vec![8], vec![24], vec![1, 8, 8]];
    for (case, widths) in sizes.iter().enumerate() {
        for (&at, &w) in f.offsets[case].iter().zip(widths) {
            assert!(
                at + w <= tag.offset || at >= tag.offset + tag.width,
                "case {case}"
            );
        }
        let word = case_word(&tag, case as u32).unwrap();
        assert_eq!(case_of(&tag, word), case as u32);
    }
    assert_eq!(case_word(&tag, 3), None);
    assert_eq!(case_of(&tag, 5), 3);
    let plain = fixed(&r[1]);
    assert_eq!(plain.size, 24);
    assert_eq!(plain.tag.unwrap().encoding, Encoding::Direct);
}

/// Bug: a suspended fixed block's frame (node-bytes-1076) lands on a word cleanup
/// still reads, or a shared array record's offsets move with the element, so the
/// drain recovers an interior base (ATTACK-rev2). Hand-derived from FAIL-8's
/// reverse field order:
/// - Node: Binary yields r@8 (its vacated word and op's word @16 free; l@0 and the
///   tag@24 live), then l@0. Assignment yields v@0 while name@8..32 (with the
///   cap niche) is still to clean; a terminal String is freed from its pointer
///   and capacity, so its length word @16 is free. No spares: 32 native bytes,
///   a 48-byte chunk with the 8-byte payer word. Only Binary's l is last: after
///   it nothing is left to clean (op is a scalar), while Assignment's v still
///   has name to free, so marking it last would leak the String.
/// - Holder{xs: Inline[Pair, 2]}, Pair{pad, l, r} of stride 24: every word is in
///   the unfinished array, so A, B and the index are three spares after 48.
#[test]
fn suspended_frames_avoid_live_words_and_unfinished_arrays() {
    use crate::{Point, frames};
    let string = || FieldTy::Seq(Box::new(u(Scalar::U8)));
    let node = || FieldTy::Box(TypeRef(0));
    let tree = || FieldTy::Box(TypeRef(1));
    let shapes = [
        variant(
            "Node",
            Repr::Own,
            vec![
                ("Number", vec![field("n", u(Scalar::I64))]),
                ("Variable", vec![field("name", string())]),
                (
                    "Binary",
                    vec![
                        field("op", u(Scalar::U8)),
                        field("l", node()),
                        field("r", node()),
                    ],
                ),
                (
                    "Assignment",
                    vec![field("name", string()), field("v", node())],
                ),
            ],
        ),
        variant(
            "Tree",
            Repr::Own,
            vec![("Leaf", vec![]), ("Fork", vec![field("kid", tree())])],
        ),
        record(
            "Pair",
            Repr::Own,
            vec![
                field("pad", u(Scalar::U64)),
                field("l", tree()),
                field("r", tree()),
            ],
        ),
        record(
            "Holder",
            Repr::Own,
            vec![field(
                "xs",
                FieldTy::Array(Box::new(FieldTy::Inline(TypeRef(2))), 2),
            )],
        ),
    ];
    let r = compute(&shapes).unwrap();
    let at = |cursor, a, b, index, last| Point {
        cursor,
        a,
        b,
        index,
        last,
    };
    let f = frames(&shapes, &r, TypeRef(0));
    // Assignment cleans v@0 while name@8 waits: a terminal String is freed from
    // its pointer and capacity, so its length word @16 takes the link.
    assert_eq!(
        f.points,
        [
            at(1, 8, 16, None, false),
            at(2, 0, 8, None, true),
            at(1, 0, 16, None, false)
        ]
    );
    assert_eq!((f.spares, f.native_bytes(32)), (0, 32));
    let f = frames(&shapes, &r, TypeRef(3));
    assert_eq!(fixed(&r[3]).size, 48);
    assert_eq!(
        f.points,
        [
            at(1, 48, 56, Some(64), false),
            at(2, 48, 56, Some(64), false)
        ]
    );
    assert_eq!(f.native_bytes(48), 72);
}

/// Bug: a niche taken from a record's field other than the first, or the niche's
/// offset left relative to the field. `Pair { n: u64, b: Box }` has its only niche
/// in its second field at 8, so `Option[Pair]` is 16 bytes with its tag word at 8.
#[test]
fn a_record_lends_its_niche_from_any_field_at_that_fields_offset() {
    let shapes = [
        record(
            "Pair",
            Repr::Own,
            vec![
                field("n", u(Scalar::U64)),
                field("b", FieldTy::Box(TypeRef(0))),
            ],
        ),
        option("Option[Pair]", FieldTy::Inline(TypeRef(0))),
    ];
    let r = compute(&shapes).unwrap();
    let f = fixed(&r[1]);
    let t = f.tag.unwrap();
    assert_eq!((f.size, t.offset, t.width), (16, 8, 8));
    assert_eq!(f.offsets, vec![vec![], vec![0]]);
}

/// Bug: a block whose cleanup drops something is called terminal, so an engine
/// frees it with no cleanup frame and skips (leaks) what it owns; a fixed root is
/// called dynamic or the reverse, so it gets the other frame shape; a resource
/// payload is classed as own storage. The expected classes are hand-derived. Rows: 0 `Leaf {x: u64}`, 1 `Text
/// {s: String}` (owns a buffer), 2 `Res` (a resource), 3 `Pair {l: Leaf, n: u64}`
/// (inline, nothing to drop), 4 `Node {next: Box[Leaf]}` (owns a box).
#[test]
fn cleanup_classes_follow_what_cleaning_the_block_drops() {
    let string = || FieldTy::Seq(Box::new(u(Scalar::U8)));
    let row = |i: u32| TypeRef(i);
    let shapes = [
        record("Leaf", Repr::Own, vec![field("x", u(Scalar::U64))]),
        record("Text", Repr::Own, vec![field("s", string())]),
        record("Res", Repr::Resource, vec![field("n", u(Scalar::U64))]),
        record(
            "Pair",
            Repr::Own,
            vec![
                field("l", FieldTy::Inline(row(0))),
                field("n", u(Scalar::U64)),
            ],
        ),
        record("Node", Repr::Own, vec![field("next", FieldTy::Box(row(0)))]),
    ];
    let r = compute(&shapes).unwrap();
    let seq = |e: FieldTy| FieldTy::Seq(Box::new(e));
    let map = |k: FieldTy, v: FieldTy| FieldTy::Map(Box::new(k), Box::new(v));
    use Cleanup::{Dynamic as D, Fixed as F, Resource as R, Terminal as T};
    for (edge, want) in [
        (FieldTy::Box(row(0)), Some(T)),
        (FieldTy::Box(row(3)), Some(T)),
        (FieldTy::Box(row(1)), Some(F)),
        (FieldTy::Box(row(4)), Some(F)),
        (FieldTy::Box(row(2)), Some(R)),
        (FieldTy::Rc(row(0)), Some(T)),
        (FieldTy::Arc(row(1)), Some(F)),
        (string(), Some(T)),
        (seq(FieldTy::Inline(row(3))), Some(T)),
        (seq(FieldTy::Inline(row(1))), Some(D)),
        (seq(string()), Some(D)),
        (seq(FieldTy::Box(row(0))), Some(D)),
        (map(u(Scalar::U64), FieldTy::Inline(row(0))), Some(T)),
        (map(u(Scalar::U64), string()), Some(D)),
        (map(string(), u(Scalar::U64)), Some(D)),
        (FieldTy::OwnFn, Some(F)),
        (u(Scalar::U64), None),
        (FieldTy::Inline(row(4)), None),
    ] {
        assert_eq!(cleanup_class(&shapes, &r, &edge), want, "{edge:?}");
    }
}

/// Both aligned words come before the two-byte scalar and the bool. The bool
/// therefore lends its 1-byte niche at offset 18. The equivalent compact row
/// puts the bool first and occupies exactly 19 bytes.
#[test]
fn field_order_moves_an_own_niche_and_keeps_compact_order() {
    let fields = vec![
        field("flag", u(Scalar::Bool)),
        field("first", u(Scalar::U64)),
        field("code", u(Scalar::U16)),
        field("second", u(Scalar::U64)),
    ];
    let shapes = [
        record("Own", Repr::Own, fields.clone()),
        option("Option[Own]", FieldTy::Inline(TypeRef(0))),
        record("Compact", Repr::Compact(Width::U16), fields),
    ];
    let r = compute(&shapes).unwrap();
    let own = fixed(&r[0]);
    assert_eq!(
        (own.size, own.align, own.offsets[0].as_slice()),
        (24, 8, &[18, 0, 16, 8][..])
    );
    let option = fixed(&r[1]);
    let tag = option.tag.unwrap();
    assert_eq!(
        (option.size, tag.offset, tag.width, case_word(&tag, 0)),
        (24, 18, 1, Some(2))
    );
    assert_eq!([0, 1, 2].map(|word| case_of(&tag, word)), [1, 1, 0]);
    let Layout::Compact(compact) = &r[2].layout else {
        panic!("compact record")
    };
    let compact = compact.declared();
    assert_eq!(compact.fixed_extent, Some(19));
    assert_eq!(
        compact.fields[0].iter().map(|f| f.pos).collect::<Vec<_>>(),
        [Pos::At(0), Pos::At(1), Pos::At(9), Pos::At(11)],
    );
}

/// A String's spare capacity values are at byte 16 of its 24-byte carrier.
/// Ordinary sorting leaves the capacity at 16 and Variable cannot fit. Moving
/// Assignment's String after its Box puts capacity at 24 and Variable fits.
#[test]
fn own_payload_moves_its_niche_carrier_to_keep_the_smaller_variant() {
    let string = || FieldTy::Seq(Box::new(u(Scalar::U8)));
    let shapes = [variant(
        "Node",
        Repr::Own,
        vec![
            (
                "Assignment",
                vec![
                    field("name", string()),
                    field("value", FieldTy::Box(TypeRef(0))),
                ],
            ),
            ("Variable", vec![field("name", string())]),
        ],
    )];
    let r = compute(&shapes).unwrap();
    let payload = fixed(&r[0]);
    assert_eq!((payload.size, payload.align), (32, 8));
    assert_eq!(payload.offsets, [vec![8, 0], vec![0]]);
    let tag = payload.tag.unwrap();
    assert_eq!((tag.offset, tag.width), (24, 8));
    assert_eq!(case_word(&tag, 0), None);
    assert_eq!(case_word(&tag, 1), Some(1 << 63));
    assert_eq!(case_of(&tag, 0), 0);
    assert_eq!(case_of(&tag, 1 << 63), 1);
    assert_eq!(r[0].logical_size, Some(40));
}

/// With two Strings, the original carrier is chosen at offset 16. The other
/// case needs 32 bytes and fits before the niche only when that carrier moves
/// last. Switching to the other String's niche would keep the tag at 16.
#[test]
fn moving_a_niche_carrier_keeps_its_niche_on_a_tie() {
    let string = || FieldTy::Seq(Box::new(u(Scalar::U8)));
    let shapes = [variant(
        "PairOrWords",
        Repr::Own,
        vec![
            ("Pair", vec![field("a", string()), field("b", string())]),
            (
                "Words",
                vec![field("xs", FieldTy::Array(Box::new(u(Scalar::U64)), 4))],
            ),
        ],
    )];
    let r = compute(&shapes).unwrap();
    let layout = fixed(&r[0]);
    let tag = layout.tag.unwrap();
    assert_eq!((layout.size, tag.offset, tag.width), (48, 40, 8));
    assert_eq!(layout.offsets, [vec![24, 0], vec![0]]);
}
