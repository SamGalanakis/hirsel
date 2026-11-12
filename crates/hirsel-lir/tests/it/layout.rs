//! Layout derivation and its independent validator: every M1b
//! representation family, scan/drop order, dynamic extents, unavailable
//! attribute combinations, validator mutations, golden vectors and C probes.
use hirsel_layout::layout::{
    Container, EnvironmentShape, LayoutBuilder, OwnerRepresentation, Owners,
};
use hirsel_lir::layout::*;
use hirsel_lir::layout_types::*;
use hirsel_lir::layout_validate::{Obligation, validate_table};
use hirsel_lir::runtime_abi::HEADER_SIZE;
use hirsel_syntax::{ContentHash, DefId, DefPath, ItemPath, PackageId, PassingMode};
use hirsel_tir::{BodyId, RepresentationRecord};
use hirsel_types::*;

// ----- fixtures ------------------------------------------------------------

fn path(name: &str) -> DefPath {
    DefPath(ItemPath {
        package: PackageId {
            name: "fixture".into(),
            version: "0".into(),
        },
        modules: vec!["m".into()],
        name: name.into(),
    })
}

use hirsel_types::testing::Types as Ty;

use crate::support::ty;

#[derive(Default)]
struct OwnerSpec {
    generics: u32,
    promises: Layout,
    resource: bool,
    finalizer: bool,
    /// Distinguishes private representations of one name.
    revision: &'static str,
}

fn representation(
    name: &str,
    spec: OwnerSpec,
    shape: impl FnOnce(&mut Ty) -> TypeId,
) -> RepresentationRecord {
    let mut t = ty();
    let private_shape = shape(&mut t);
    RepresentationRecord {
        definition: path(name),
        representation_revision: ContentHash::of_bytes(
            format!("{name}:{}", spec.revision).as_bytes(),
        ),
        generics: (0..spec.generics)
            .map(|i| GenericParameter::Type {
                name: format!("T{i}"),
                bounds: Vec::new().into(),
            })
            .collect(),
        private_shape,
        layout_promises: spec.promises,
        resource: spec.resource,
        finalizer: spec
            .finalizer
            .then(|| (path(&format!("{name}.drop")), BodyId(0))),
        unsafe_ffi_package: false,
    }
}

fn owner(
    owners: &mut Owners,
    name: &str,
    spec: OwnerSpec,
    shape: impl FnOnce(&mut Ty) -> TypeId,
) -> DefPath {
    let record = representation(name, spec, shape);
    owners.nominals.insert(
        DefId::intern(&path(name)),
        OwnerRepresentation::Defined(record),
    );
    path(name)
}

fn x86() -> TargetSpec {
    TargetSpec::x86_64_unknown_linux_gnu()
}

/// Validate and return the table; every derived table must pass.
fn checked(b: LayoutBuilder<'_>) -> LayoutTable {
    let target = b.target().clone();
    let table = b.finish();
    if let Err(v) = validate_table(&table, &target) {
        panic!("derived table fails validation: {v:#?}");
    }
    table
}

fn rec(table: &LayoutTable, id: LayoutId) -> &LayoutRecord {
    table.layout(id).unwrap()
}

fn fixed(r: &LayoutRecord) -> u64 {
    match r.size {
        SizeSpec::Fixed(n) => n,
        _ => panic!("dynamic size"),
    }
}

fn offsets(r: &LayoutRecord) -> Vec<u64> {
    r.fields.iter().map(|f| f.offset).collect()
}

/// Run a drop program over concrete bytes, independently of the derivation:
/// the ordered (address, action) events a runtime would perform.
fn drop_events(
    table: &LayoutTable,
    id: LayoutId,
    bytes: &[u8],
    base: u64,
    out: &mut Vec<(u64, &'static str)>,
) {
    let r = rec(table, id);
    run_drop(table, &r.drop, bytes, base, out);
}

fn read_u64(bytes: &[u8], at: u64) -> u64 {
    u64::from_le_bytes(bytes[at as usize..at as usize + 8].try_into().unwrap())
}

fn run_drop(
    table: &LayoutTable,
    steps: &[DropStep],
    bytes: &[u8],
    base: u64,
    out: &mut Vec<(u64, &'static str)>,
) {
    for step in steps {
        match step {
            DropStep::Finalize => out.push((base, "finalize")),
            DropStep::Release { offset } => out.push((base + offset, "release")),
            DropStep::ReleaseRoot { offset } => out.push((base + offset, "release root")),
            DropStep::ReleaseSealed { offset } => out.push((base + offset, "release sealed")),
            DropStep::ReleaseReceipt { offset } => out.push((base + offset, "release receipt")),
            DropStep::Environment { offset } => out.push((base + offset, "environment")),
            DropStep::CloseHandle { offset } => out.push((base + offset, "close")),
            DropStep::Child { offset, layout } => {
                drop_events(table, *layout, bytes, base + offset, out)
            }
            DropStep::Thaw { offset, .. } => out.push((base + offset, "thaw")),
            DropStep::ActiveVariant {
                tag_offset,
                form,
                variants,
            } => {
                let at = (base + tag_offset) as usize;
                let width = form.width(8) as usize;
                let mut word = [0u8; 8];
                word[..width].copy_from_slice(&bytes[at..at + width]);
                let tag = form.active(u64::from_le_bytes(word));
                if let Some((_, steps)) = variants.iter().find(|(o, _)| *o == tag) {
                    run_drop(table, steps, bytes, base, out);
                }
            }
            DropStep::ElementsReverse {
                count_offset,
                offset,
                stride,
                element,
            } => {
                let n = read_u64(bytes, base + count_offset);
                for i in (0..n).rev() {
                    drop_events(table, *element, bytes, base + offset + i * stride, out);
                }
            }
            DropStep::RepeatReverse {
                offset,
                count,
                stride,
                element,
            } => {
                for i in (0..*count).rev() {
                    drop_events(table, *element, bytes, base + offset + i * stride, out);
                }
            }
            DropStep::ColumnsReverse(c) => {
                let n = read_u64(bytes, base + c.count_offset);
                let cap = read_u64(bytes, base + c.capacity_offset);
                for i in (0..n).rev() {
                    // A variant row drops only the arm its tag column names.
                    let active = c.tag.map(|tag| {
                        let width = match tag.scalar {
                            ScalarKind::U8 => 1,
                            ScalarKind::U16 => 2,
                            _ => 4,
                        };
                        let at = (base + c.data_offset + cap * tag.prefix + i * width) as usize;
                        let mut word = [0u8; 4];
                        word[..width as usize].copy_from_slice(&bytes[at..at + width as usize]);
                        u32::from_le_bytes(word)
                    });
                    for column in c.columns.iter().rev() {
                        if active.is_some() && column.arm != active {
                            continue;
                        }
                        let at = base + c.data_offset + cap * column.prefix + i * column.stride;
                        drop_events(table, column.layout, bytes, at, out);
                    }
                }
            }
        }
    }
}

// ----- scalars and aggregates -------------------------------------------

#[test]
fn nested_mixed_inline_pointers_scan_and_drop_at_their_offsets() {
    let owners = Owners::default();
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let (u8_, i64_, u16_, i32_, string) = (
        t.p(Primitive::U8),
        t.p(Primitive::I64),
        t.p(Primitive::U16),
        t.p(Primitive::I32),
        t.p(Primitive::String),
    );
    let boxed = t.boxed(i64_);
    let inner = t.record(&[u16_, string]);
    let outer = t.record(&[u8_, boxed, inner, i32_]);
    let id = b.layout(&t.store, outer).unwrap();
    let inner_id = b.layout(&t.store, inner).unwrap();
    let table = checked(b);
    let r = rec(&table, id);
    assert_eq!(offsets(r), [0, 8, 16, 48]);
    assert_eq!((fixed(r), r.align), (56, 8));
    assert_eq!(offsets(rec(&table, inner_id)), [0, 8]);
    assert_eq!(
        r.scan,
        [
            ScanStep::Counted { offset: 8 },
            ScanStep::SliceOwner { offset: 24 }
        ]
    );
    // Fields high to low: the inner String before the box.
    assert_eq!(
        r.drop,
        [
            DropStep::Release { offset: 24 },
            DropStep::Release { offset: 8 }
        ]
    );
    assert_eq!(r.copy, CopyRecipe::Fields);
    // The String wrapper (owner, start, length) is distinct from its bytes.
    let string_id = rec(&table, inner_id).fields[1].layout;
    let LayoutKind::Slice { element } = rec(&table, string_id).kind else {
        panic!("String is a slice wrapper")
    };
    assert_eq!(
        rec(&table, element).kind,
        LayoutKind::Scalar(ScalarKind::U8)
    );
    assert_eq!(fixed(rec(&table, string_id)), 24);
    let storage = table
        .layouts
        .iter()
        .find(|l| l.kind == LayoutKind::Sequence { element })
        .expect("String storage is a counted byte sequence");
    assert_eq!(storage.copy, CopyRecipe::NotCopyable);
}

#[test]
fn packed_fields_are_unaligned_and_align_raises_alignment() {
    let mut owners = Owners::default();
    let packed = owner(
        &mut owners,
        "Wire",
        OwnerSpec {
            promises: Layout {
                packed: true,
                ..Layout::default()
            },
            ..OwnerSpec::default()
        },
        |t| {
            let (a, b, c) = (t.p(Primitive::U8), t.p(Primitive::U64), t.p(Primitive::U16));
            let bx = t.boxed(b);
            t.record(&[a, b, c, bx])
        },
    );
    let aligned = owner(
        &mut owners,
        "Line",
        OwnerSpec {
            promises: Layout {
                align: Some(64),
                ..Layout::default()
            },
            ..OwnerSpec::default()
        },
        |t| {
            let a = t.p(Primitive::U8);
            t.record(&[a])
        },
    );
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let p = t.named(&packed, vec![]);
    let a = t.named(&aligned, vec![]);
    let pid = b.layout(&t.store, p).unwrap();
    let aid = b.layout(&t.store, a).unwrap();
    let table = checked(b);
    let r = rec(&table, pid);
    assert_eq!(offsets(r), [0, 1, 9, 11]);
    assert_eq!((fixed(r), r.align), (19, 1));
    assert_eq!(
        r.fields.iter().map(|f| f.unaligned).collect::<Vec<_>>(),
        [false, true, true, true]
    );
    assert!(r.abi.unaligned);
    // An unaligned counted reference is still scanned and released at its offset.
    assert_eq!(r.scan, [ScanStep::Counted { offset: 11 }]);
    assert_eq!(r.drop, [DropStep::Release { offset: 11 }]);
    assert!(r.key.owner_revision.is_some());
    let r = rec(&table, aid);
    assert_eq!((fixed(r), r.align), (64, 64));
}

#[test]
fn variants_scan_and_drop_only_the_active_payload() {
    let owners = Owners::default();
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let (u8_, i64_, string) = (
        t.p(Primitive::U8),
        t.p(Primitive::I64),
        t.p(Primitive::String),
    );
    let v = t.variant(&[&[u8_], &[i64_, string], &[]]);
    let id = b.layout(&t.store, v).unwrap();
    let table = checked(b);
    let r = rec(&table, id);
    assert_eq!((fixed(r), r.align), (40, 8));
    let tag = r.tag.as_ref().unwrap();
    assert_eq!(tag.offset, 0);
    assert_eq!(offsets_of(&tag.variants[0].fields), [8]);
    assert_eq!(offsets_of(&tag.variants[1].fields), [8, 16]);
    assert!(tag.variants[2].fields.is_empty());
    assert_eq!(
        r.scan,
        [ScanStep::ActiveVariant {
            tag_offset: 0,
            form: TagForm::Ordinal,
            variants: vec![(1, vec![ScanStep::SliceOwner { offset: 16 }])]
        }]
    );
    let mut bytes = vec![0u8; 40];
    for (tag, expected) in [(0u32, vec![]), (1, vec![(16, "release")]), (2, vec![])] {
        bytes[..4].copy_from_slice(&tag.to_le_bytes());
        let mut events = Vec::new();
        drop_events(&table, id, &bytes, 0, &mut events);
        assert_eq!(events, expected, "tag {tag}");
    }
}

/// `Leaf | Node(Box[Tree], Box[Tree])`, the binarytrees node, and `Node(i64, String,
/// Box[f64]) | Leaf`, whose niche word is the String's owner after the i64.
fn niche_table() -> (LayoutTable, LayoutId, LayoutId, LayoutId) {
    niche_table_for(&x86())
}

fn niche_table_for(target: &TargetSpec) -> (LayoutTable, LayoutId, LayoutId, LayoutId) {
    let mut owners = Owners::default();
    let tree = owner(&mut owners, "Tree", OwnerSpec::default(), |t| {
        let me = t.named(&path("Tree"), vec![]);
        let child = t.boxed(me);
        t.variant(&[&[], &[child, child]])
    });
    let mut b = LayoutBuilder::new(target, &owners);
    let mut t = ty();
    let (i64_, string, f64_) = (
        t.p(Primitive::I64),
        t.p(Primitive::String),
        t.p(Primitive::F64),
    );
    let bx = t.boxed(f64_);
    let shifted = t.variant(&[&[i64_, string, bx], &[]]);
    let named = t.named(&tree, vec![]);
    let boxed = t.boxed(named);
    let tid = b.layout(&t.store, named).unwrap();
    let bid = b.layout(&t.store, boxed).unwrap();
    let sid = b.layout(&t.store, shifted).unwrap();
    (checked(b), tid, bid, sid)
}

#[test]
fn a_fieldless_and_a_block_holding_variant_share_the_null_word_as_their_tag() {
    let (table, tid, bid, sid) = niche_table();
    let r = rec(&table, tid);
    assert_eq!((fixed(r), r.align), (16, 8));
    let tag = r.tag.as_ref().unwrap();
    assert_eq!(
        (tag.offset, tag.form),
        (
            0,
            TagForm::Niche {
                nullary: 0,
                dataful: 1
            }
        )
    );
    assert!(tag.variants[0].fields.is_empty());
    assert_eq!(offsets_of(&tag.variants[1].fields), [0, 8]);
    // No u32 tag leaf: the two words are the node's whole ABI.
    assert_eq!(
        r.abi.scalars,
        [(0, ScalarKind::Pointer), (8, ScalarKind::Pointer)]
    );
    assert_eq!(
        rec(&table, bid).kind,
        LayoutKind::Box { payload: tid },
        "a Box[Tree] block's payload is the 16-byte node"
    );
    let mut bytes = vec![0u8; 16];
    let mut events = Vec::new();
    drop_events(&table, tid, &bytes, 0, &mut events);
    assert_eq!(events, [], "a null word is a Leaf");
    bytes[..8].copy_from_slice(&0x7f00_0000_1000u64.to_le_bytes());
    drop_events(&table, tid, &bytes, 0, &mut events);
    assert_eq!(events, [(8, "release"), (0, "release")]);

    let s = rec(&table, sid);
    assert_eq!(fixed(s), 40);
    let tag = s.tag.as_ref().unwrap();
    assert_eq!(
        (tag.offset, tag.form),
        (
            8,
            TagForm::Niche {
                nullary: 1,
                dataful: 0
            }
        )
    );
}

#[test]
fn variants_without_one_fieldless_and_one_nonnull_word_holding_variant_keep_a_u32_tag() {
    let mut owners = Owners::default();
    let packed = owner(
        &mut owners,
        "PackedOpt",
        OwnerSpec {
            promises: Layout {
                packed: true,
                ..Layout::default()
            },
            ..OwnerSpec::default()
        },
        |t| {
            let f = t.p(Primitive::F64);
            let bx = t.boxed(f);
            t.variant(&[&[], &[bx]])
        },
    );
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let (u8_, u64_, string, f64_) = (
        t.p(Primitive::U8),
        t.p(Primitive::U64),
        t.p(Primitive::String),
        t.p(Primitive::F64),
    );
    let bx = t.boxed(f64_);
    let maybe_text = t.variant(&[&[], &[string]]);
    let after_nullable = t.tuple(&[u64_, maybe_text]);
    let words = t.tuple(&[u64_, f64_]);
    let cases = [
        ("three variants", t.variant(&[&[], &[], &[bx]])),
        (
            "a nested variant's word may be null",
            t.variant(&[&[], &[maybe_text]]),
        ),
        (
            "a word only inside a nested variant",
            t.variant(&[&[], &[after_nullable]]),
        ),
        ("no pointer word", t.variant(&[&[], &[words]])),
        ("no fieldless variant", t.variant(&[&[u8_], &[bx]])),
        ("one variant", t.variant(&[&[bx]])),
        ("@packed", t.named(&packed, vec![])),
    ];
    let ids: Vec<_> = cases
        .iter()
        .map(|(_, ty)| b.layout(&t.store, *ty).unwrap())
        .collect();
    let table = checked(b);
    for ((name, _), id) in cases.iter().zip(ids) {
        let tag = rec(&table, id).tag.as_ref().unwrap();
        assert_eq!(tag.form, TagForm::Ordinal, "{name}");
    }
}

/// `None | Some(T)` over payloads whose first non-null word is nested: a String in a
/// tuple (the map entry `(String, u64)`) and after a u64, a String in a record after a
/// u8, a String two tuples deep, and a Box after a nullable `Option[Box]`.
fn nested_niche_table() -> (LayoutTable, [LayoutId; 5]) {
    let owners = Owners::default();
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let (u8_, u32_, u64_, string, f64_) = (
        t.p(Primitive::U8),
        t.p(Primitive::U32),
        t.p(Primitive::U64),
        t.p(Primitive::String),
        t.p(Primitive::F64),
    );
    let bx = t.boxed(f64_);
    let maybe_box = t.variant(&[&[], &[bx]]);
    let entry = t.tuple(&[string, u64_]);
    let late = t.tuple(&[u64_, string]);
    let record = t.record(&[u8_, string]);
    let inner = t.tuple(&[u32_, string]);
    let deep = t.tuple(&[u64_, inner]);
    let guarded = t.tuple(&[maybe_box, bx]);
    let ids = [entry, late, record, deep, guarded].map(|payload| {
        let option = t.variant(&[&[], &[payload]]);
        b.layout(&t.store, option).unwrap()
    });
    (checked(b), ids)
}

#[test]
fn a_nonnull_word_nested_in_a_string_tuple_or_record_is_the_niche() {
    let (table, ids) = nested_niche_table();
    // (size, niche offset): no variant grows past its payload.
    let expected = [(32, 0), (32, 8), (32, 8), (40, 16), (16, 8)];
    for (id, (size, offset)) in ids.into_iter().zip(expected) {
        let r = rec(&table, id);
        assert_eq!((fixed(r), r.align), (size, 8), "{offset}");
        let tag = r.tag.as_ref().unwrap();
        assert_eq!(
            (tag.offset, tag.form),
            (
                offset,
                TagForm::Niche {
                    nullary: 0,
                    dataful: 1
                }
            )
        );
        assert_eq!(offsets_of(&tag.variants[1].fields), [0]);
        let mut bytes = vec![0u8; size as usize];
        let mut events = Vec::new();
        drop_events(&table, id, &bytes, 0, &mut events);
        assert_eq!(events, [], "a null word is None");
        let at = offset as usize;
        bytes[at..at + 8].copy_from_slice(&0x7f00_0000_1000u64.to_le_bytes());
        drop_events(&table, id, &bytes, 0, &mut events);
        assert!(events.contains(&(offset, "release")), "{events:?}");
    }
}

fn offsets_of(fields: &[FieldLayout]) -> Vec<u64> {
    fields.iter().map(|f| f.offset).collect()
}

#[test]
fn resources_finalize_before_their_fields_high_to_low() {
    let mut owners = Owners::default();
    let res = owner(
        &mut owners,
        "File",
        OwnerSpec {
            resource: true,
            finalizer: true,
            ..OwnerSpec::default()
        },
        |t| {
            let (s, i) = (t.p(Primitive::String), t.p(Primitive::I64));
            let bx = t.boxed(i);
            t.record(&[s, bx])
        },
    );
    let bare = owner(
        &mut owners,
        "Token",
        OwnerSpec {
            resource: true,
            ..OwnerSpec::default()
        },
        |t| t.p(Primitive::U64),
    );
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let r1 = t.named(&res, vec![]);
    let tok = t.named(&bare, vec![]);
    let pair = t.record(&[r1, tok, r1]);
    let rid = b.layout(&t.store, r1).unwrap();
    let pid = b.layout(&t.store, pair).unwrap();
    let tid = b.layout(&t.store, tok).unwrap();
    let table = checked(b);
    let r = rec(&table, rid);
    assert_eq!(r.kind, LayoutKind::Resource);
    assert_eq!(r.copy, CopyRecipe::NotCopyable);
    assert_eq!(r.finalizer.as_ref().unwrap().definition, path("File.drop"));
    assert_eq!(
        r.drop,
        [
            DropStep::Finalize,
            DropStep::Release { offset: 24 },
            DropStep::Release { offset: 0 }
        ]
    );
    assert!(!r.storage.contains(&StorageKind::Static));
    // A resource without a drop body is still a noncopyable resource.
    let tok_r = rec(&table, tid);
    assert_eq!(
        (tok_r.kind.clone(), tok_r.copy),
        (LayoutKind::Resource, CopyRecipe::NotCopyable)
    );
    assert!(tok_r.finalizer.is_none());
    let p = rec(&table, pid);
    assert_eq!(offsets(p), [0, 32, 40]);
    let mut events = Vec::new();
    drop_events(&table, pid, &[0u8; 72], 0, &mut events);
    assert_eq!(
        events,
        [
            (40, "finalize"),
            (64, "release"),
            (40, "release"),
            (0, "finalize"),
            (24, "release"),
            (0, "release")
        ]
    );
}

#[test]
fn soa_sequences_drop_elements_high_to_low_and_fields_in_reverse() {
    let mut owners = Owners::default();
    let particle = owner(
        &mut owners,
        "Particle",
        OwnerSpec {
            promises: Layout {
                soa: true,
                ..Layout::default()
            },
            ..OwnerSpec::default()
        },
        |t| {
            let (id, name, f) = (
                t.p(Primitive::U32),
                t.p(Primitive::String),
                t.p(Primitive::F64),
            );
            let bx = t.boxed(f);
            t.record(&[id, name, f, bx])
        },
    );
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let p = t.named(&particle, vec![]);
    let list = t.list(p);
    let standalone = b.layout(&t.store, p).unwrap();
    let lid = b.layout(&t.store, list).unwrap();
    let table = checked(b);
    // Standalone records stay inline in declaration order.
    assert_eq!(offsets(rec(&table, standalone)), [0, 8, 32, 40]);
    let LayoutKind::Block { payload } = rec(&table, lid).kind else {
        panic!("a list is a block")
    };
    let payload_r = rec(&table, payload);
    assert_eq!(
        payload_r.kind,
        LayoutKind::SoaColumns {
            element: standalone
        }
    );
    let DropStep::ColumnsReverse(columns) = &payload_r.drop[0] else {
        panic!("SoA drop")
    };
    assert_eq!(columns.data_offset, 16);
    assert_eq!(columns.capacity_quantum, 8);
    assert_eq!(
        columns
            .columns
            .iter()
            .map(|c| (c.field, c.prefix, c.stride))
            .collect::<Vec<_>>(),
        [(1, 4, 24), (3, 36, 8)]
    );
    // Two live elements in capacity 2: column bases 16 + 2 * prefix.
    let mut bytes = vec![0u8; 16 + 2 * 44];
    bytes[..8].copy_from_slice(&2u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&2u64.to_le_bytes());
    assert_eq!(extent(&table, payload, &bytes), Ok(16 + 2 * 44));
    let mut events = Vec::new();
    drop_events(&table, payload, &bytes, 0, &mut events);
    let name = |i: u64| 16 + 2 * 4 + i * 24;
    let boxed = |i: u64| 16 + 2 * 36 + i * 8;
    assert_eq!(
        events,
        [
            (boxed(1), "release"),
            (name(1), "release"),
            (boxed(0), "release"),
            (name(0), "release")
        ]
    );
}

#[test]
fn soa_variants_store_a_narrow_tag_column_and_drop_only_active_payloads() {
    let mut owners = Owners::default();
    let soa = || OwnerSpec {
        promises: Layout {
            soa: true,
            ..Layout::default()
        },
        ..OwnerSpec::default()
    };
    let named = owner(&mut owners, "Named", soa(), |t| {
        let string = t.p(Primitive::String);
        t.variant(&[&[], &[string]])
    });
    let sample = owner(&mut owners, "Sample", soa(), |t| {
        let (u8_, i64_, u128_, string) = (
            t.p(Primitive::U8),
            t.p(Primitive::I64),
            t.p(Primitive::U128),
            t.p(Primitive::String),
        );
        t.variant(&[&[u8_], &[string, i64_], &[u128_], &[]])
    });
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let n = t.named(&named, vec![]);
    let s = t.named(&sample, vec![]);
    let list = t.list(s);
    let named_row = b.layout(&t.store, n).unwrap();
    let row = b.layout(&t.store, s).unwrap();
    let lid = b.layout(&t.store, list).unwrap();
    let table = checked(b);
    // An @soa variant has no null-pointer niche: its rows keep an ordinal tag.
    assert_eq!(
        rec(&table, named_row)
            .tag
            .as_ref()
            .map(|t| (t.offset, t.form)),
        Some((0, TagForm::Ordinal))
    );
    // Row: u32 tag, payload from 16 (u128 alignment), 48 bytes in all.
    assert_eq!((fixed(rec(&table, row)), rec(&table, row).align), (48, 16));
    let LayoutKind::Block { payload } = rec(&table, lid).kind else {
        panic!("a list is a block")
    };
    let DropStep::ColumnsReverse(columns) = &rec(&table, payload).drop[0] else {
        panic!("SoA drop")
    };
    // Four arms fit a u8 tag column; the payload column follows it, 32 bytes a row.
    assert_eq!(
        columns.tag,
        Some(SoaTag {
            prefix: 0,
            scalar: ScalarKind::U8
        })
    );
    assert_eq!((columns.data_offset, columns.capacity_quantum), (16, 16));
    assert_eq!(
        columns
            .columns
            .iter()
            .map(|c| (c.field, c.prefix, c.stride, c.arm))
            .collect::<Vec<_>>(),
        [(1, 1, 32, Some(1))]
    );
    // Capacity 16, two rows: row 0 is the String arm, row 1 the u8 arm.
    let mut bytes = vec![0u8; 16 + 16 * 33];
    bytes[..8].copy_from_slice(&2u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&16u64.to_le_bytes());
    bytes[16] = 1;
    bytes[17] = 0;
    assert_eq!(extent(&table, payload, &bytes), Ok(16 + 16 * 33));
    let mut events = Vec::new();
    drop_events(&table, payload, &bytes, 0, &mut events);
    assert_eq!(events, [(16 + 16, "release")]);

    // V: an arm view must sit under the tag column, as its own arm.
    let target = x86();
    let mutate = |edit: &dyn Fn(&mut SoaColumns)| {
        let mut bad = table.clone();
        let record = &mut bad.layouts[payload.0 as usize];
        let DropStep::ColumnsReverse(c) = &mut record.drop[0] else {
            unreachable!()
        };
        edit(c);
        validate_table(&bad, &target).is_err()
    };
    assert!(mutate(&|c| c.columns[0].arm = None));
    assert!(mutate(&|c| c.columns[0].arm = Some(0)));
    assert!(mutate(&|c| c.tag = None));
    assert!(mutate(&|c| c.columns.clear()));
}

#[test]
fn empty_large_and_overflowing_inline_arrays() {
    let owners = Owners::default();
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let (u8_, i64_, string) = (
        t.p(Primitive::U8),
        t.p(Primitive::I64),
        t.p(Primitive::String),
    );
    let empty = t.inline(string, 0);
    let large = t.inline(i64_, 1 << 20);
    let strings = t.inline(string, 3);
    let bytes4 = t.inline(u8_, 4);
    let huge = t.inline(i64_, 1 << 62);
    let overflow = t.inline(huge, 8);
    let e = b.layout(&t.store, empty).unwrap();
    let l = b.layout(&t.store, large).unwrap();
    let s = b.layout(&t.store, strings).unwrap();
    let w = b.layout(&t.store, bytes4).unwrap();
    assert_eq!(b.layout(&t.store, overflow), Err(LayoutError::Overflow));
    let table = checked(b);
    assert_eq!((fixed(rec(&table, e)), rec(&table, e).align), (0, 8));
    assert!(rec(&table, e).drop.is_empty());
    assert_eq!(fixed(rec(&table, l)), 8 << 20);
    assert!(
        rec(&table, l).abi.scalars.is_empty(),
        "too large for register classification"
    );
    assert_eq!(rec(&table, w).abi.scalars.len(), 4);
    let mut events = Vec::new();
    drop_events(&table, s, &[0; 72], 0, &mut events);
    assert_eq!(events, [(48, "release"), (24, "release"), (0, "release")]);
}

// ----- dynamic extents ----------------------------------------------------

#[test]
fn packed_variants_take_only_the_active_variant_extent() {
    let owners = Owners::default();
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let (u8_, i64_) = (t.p(Primitive::U8), t.p(Primitive::I64));
    let v = t.variant(&[&[u8_], &[i64_, i64_], &[]]);
    let packed = t.builtin(BuiltinType::Packed, vec![GenericArgument::Type(v)]);
    let boxed = t.boxed(packed);
    let ordinary = b.layout(&t.store, v).unwrap();
    let id = b.layout(&t.store, packed).unwrap();
    let bid = b.layout(&t.store, boxed).unwrap();
    // Bounded Packed storage reserves its largest active extent inline.
    let holder = t.record(&[packed]);
    let hid = b.layout(&t.store, holder).unwrap();
    let table = checked(b);
    assert_eq!(fixed(rec(&table, hid)), 24);
    assert_eq!(fixed(rec(&table, ordinary)), 24);
    let r = rec(&table, id);
    assert_eq!(r.kind, LayoutKind::Packed);
    for (tag, size) in [(0u32, 16u64), (1, 24), (2, 8)] {
        let mut bytes = [0u8; 8];
        bytes[..4].copy_from_slice(&tag.to_le_bytes());
        assert_eq!(extent(&table, id, &bytes), Ok(size), "tag {tag}");
    }
    assert!(matches!(
        extent(&table, id, &[9, 0, 0, 0]),
        Err(LayoutError::Extent(_))
    ));
    // Packed behind a block: the payload keeps its dynamic extent.
    assert_eq!(rec(&table, bid).kind, LayoutKind::Box { payload: id });
}

/// A flat message: id u32, name String, a Packed status, then the growable
/// List[String] of tags. Sizes come from the bytes with checked arithmetic.
fn flat_message(owners: &mut Owners) -> DefPath {
    let status = owner(owners, "Status", OwnerSpec::default(), |t| {
        let (u16_, u64_) = (t.p(Primitive::U16), t.p(Primitive::U64));
        t.variant(&[&[u16_], &[u64_, u16_]])
    });
    owner(
        owners,
        "Message",
        OwnerSpec {
            promises: Layout {
                flat: true,
                ..Layout::default()
            },
            ..OwnerSpec::default()
        },
        |t| {
            let (id, name) = (t.p(Primitive::U32), t.p(Primitive::String));
            let st = t.named(&status, vec![]);
            let packed = t.builtin(BuiltinType::Packed, vec![GenericArgument::Type(st)]);
            let tags = t.list(name);
            t.growable_record(&[id, name, packed, tags])
        },
    )
}

/// FlatFormatV0 golden bytes for one Message (little-endian, no padding).
fn message_bytes() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(7u32.to_le_bytes()); // id
    v.extend(3u64.to_le_bytes()); // name length
    v.extend(b"abc");
    v.extend(14u64.to_le_bytes()); // Packed status length
    v.extend(1u32.to_le_bytes()); // ordinal 1: u64, u16
    v.extend(9u64.to_le_bytes());
    v.extend(2u16.to_le_bytes());
    v.extend(29u64.to_le_bytes()); // tags length
    v.extend(2u64.to_le_bytes()); // tag count
    v.extend(1u64.to_le_bytes());
    v.extend(b"x");
    v.extend(4u64.to_le_bytes());
    v.extend(b"yzzy");
    v
}

#[test]
fn flat_v0_extents_follow_the_sequential_cursor() {
    let mut owners = Owners::default();
    let message = flat_message(&mut owners);
    let mut b = LayoutBuilder::new(&x86(), &owners);
    let mut t = ty();
    let m = t.named(&message, vec![]);
    let body = b.flat(&t.store, m).unwrap();
    let value = b.layout(&t.store, m).unwrap();
    let table = checked(b);
    let r = rec(&table, body);
    assert_eq!(r.kind, LayoutKind::Flat);
    assert_eq!(r.align, 1);
    // Static offsets: each earlier variable field counts its 8-byte prefix.
    assert_eq!(offsets(r), [0, 4, 12, 20]);
    let bytes = message_bytes();
    assert_eq!(extent(&table, body, &bytes), Ok(bytes.len() as u64));
    // Truncated bytes are an extent error, never a wild read.
    assert!(matches!(
        extent(&table, body, &bytes[..10]),
        Err(LayoutError::Extent(_))
    ));
    // A variable flat value lives behind a counted block of its bytes.
    let v = rec(&table, value);
    assert_eq!(v.kind, LayoutKind::Block { payload: body });
    assert_eq!(v.copy, CopyRecipe::FlatBytes);
    // A length prefix that would overflow the extent is checked.
    let mut huge = bytes.clone();
    huge[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(extent(&table, body, &huge), Err(LayoutError::Overflow));
}

// ----- blocks, owners, closures, handles -----------------------------------

// ----- identity -----------------------------------------------------------

// ----- validator mutations -------------------------------------------------

// ----- golden vectors and C probes -----------------------------------------

// ----- request order ------------------------------------------------------

/// Independent C probes: clang computes `sizeof`/`alignof`/`offsetof` of the
/// hand-written C mirrors and must agree with the derivation on both targets.
#[test]
#[ignore = "native: run by tools/check-native.sh"]
fn c_probes_agree_on_both_targets() {
    for (target, triple) in TargetSpec::executed()
        .into_iter()
        .zip(["x86_64-linux-gnu", "aarch64-linux-gnu"])
    {
        let mut owners = Owners::default();
        let names = c_records(&mut owners);
        let mut b = LayoutBuilder::new(&target, &owners);
        let mut t = ty();
        let mut probe = String::from("#include \"c_records.h\"\n");
        let mut ids = Vec::new();
        for name in &names {
            let ty = t.named(&path(name), vec![]);
            ids.push(b.layout(&t.store, ty).unwrap());
        }
        let table = checked(b);
        // The niche shapes: the dataful variant's fields are the whole value,
        // and the niche word is a pointer word inside them.
        let (niches, tree, _, shifted) = niche_table_for(&target);
        for (name, id) in [("NicheTree", tree), ("NicheShifted", shifted)] {
            let r = rec(&niches, id);
            let tag = r.tag.as_ref().unwrap();
            let TagForm::Niche { dataful, .. } = tag.form else {
                panic!("{name} is a niche")
            };
            probe += &format!(
                "_Static_assert(sizeof(struct {name}) == {}, \"{name} size\");\n",
                fixed(r)
            );
            probe += &format!(
                "_Static_assert(_Alignof(struct {name}) == {}, \"{name} align\");\n",
                r.align
            );
            for f in &tag.variants[dataful as usize].fields {
                probe += &format!(
                    "_Static_assert(offsetof(struct {name}, f{0}) == {1}, \"{name}.f{0}\");\n",
                    f.ordinal, f.offset
                );
            }
            probe += &format!(
                "_Static_assert(offsetof(struct {name}, niche) == {}, \"{name} niche\");\n",
                tag.offset
            );
        }
        for (name, id) in names.iter().zip(ids) {
            let r = rec(&table, id);
            probe += &format!(
                "_Static_assert(sizeof(struct {name}) == {}, \"{name} size\");\n",
                fixed(r)
            );
            probe += &format!(
                "_Static_assert(_Alignof(struct {name}) == {}, \"{name} align\");\n",
                r.align
            );
            assert_eq!(r.initialized, InitializedBytes::ZeroPadding);
            for (i, f) in r.fields.iter().enumerate() {
                probe += &format!(
                    "_Static_assert(offsetof(struct {name}, f{i}) == {}, \"{name}.f{i}\");\n",
                    f.offset
                );
            }
        }
        let dir = std::env::temp_dir().join(format!("hirsel-layout-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("probe-{triple}.c"));
        std::fs::write(&file, &probe).unwrap();
        let out = std::process::Command::new(
            std::path::PathBuf::from(
                std::env::var_os("LLVM_SYS_231_PREFIX").expect("LLVM_SYS_231_PREFIX"),
            )
            .join("bin/clang"),
        )
        .args(["-std=c11", "-ffreestanding", "-fsyntax-only"])
        .arg(format!("--target={triple}"))
        .arg("-I")
        .arg(golden_dir())
        .arg(&file)
        .output()
        .expect("the pinned clang input is executable");
        assert!(
            out.status.success(),
            "{triple}: {}\n{probe}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn golden_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../native-tests/layout")
}

/// `@layout(c)` records mirrored by hand in `native-tests/layout/c_records.h`.
fn c_records(owners: &mut Owners) -> Vec<&'static str> {
    let c = || OwnerSpec {
        promises: Layout {
            c: true,
            ..Layout::default()
        },
        ..OwnerSpec::default()
    };
    let cp = || OwnerSpec {
        promises: Layout {
            c: true,
            packed: true,
            ..Layout::default()
        },
        ..OwnerSpec::default()
    };
    let ca = || OwnerSpec {
        promises: Layout {
            c: true,
            align: Some(32),
            ..Layout::default()
        },
        ..OwnerSpec::default()
    };
    owner(owners, "CHeader", c(), |t| {
        let (a, b) = (t.p(Primitive::U32), t.p(Primitive::U16));
        t.record(&[a, b, b])
    });
    owner(owners, "CMixed", c(), |t| {
        let (a, b, d, e, f) = (
            t.p(Primitive::U8),
            t.p(Primitive::F64),
            t.p(Primitive::Bool),
            t.p(Primitive::I16),
            t.p(Primitive::F32),
        );
        t.record(&[a, b, d, e, f])
    });
    owner(owners, "CWide", c(), |t| {
        let (a, b, d) = (
            t.p(Primitive::U8),
            t.p(Primitive::I128),
            t.p(Primitive::U32),
        );
        t.record(&[a, b, d])
    });
    owner(owners, "CNested", c(), |t| {
        let (a, h) = (t.p(Primitive::U8), t.named(&path("CMixed"), vec![]));
        let arr = t.inline(h, 3);
        let bytes = t.inline(a, 5);
        t.record(&[a, h, bytes, arr])
    });
    owner(owners, "CPacked", cp(), |t| {
        let (a, b, d) = (t.p(Primitive::U8), t.p(Primitive::U64), t.p(Primitive::U16));
        t.record(&[a, b, d])
    });
    owner(owners, "CAligned", ca(), |t| {
        let (a, b) = (t.p(Primitive::U8), t.p(Primitive::U64));
        t.record(&[a, b])
    });
    vec![
        "CHeader", "CMixed", "CWide", "CNested", "CPacked", "CAligned",
    ]
}
