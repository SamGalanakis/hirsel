use super::*;
use crate::{source::SourceMap, text, wf};

fn decode(bytes: &[u8]) -> DecodeResult<Module> {
    super::decode(bytes, &Debit::unlimited()).map(|body| body.module)
}

fn checked(source: &str) -> Module {
    let module = text::parse(source).unwrap();
    wf::module(&module).unwrap();
    module
}

// b2 is reached exclusively by Step. b3 is dead even though it has a call.
const ORIGINAL: &str = "core v1
        type t0 = own \"u64\" value u64 { 0: u64 }
        type t1 = own \"u32\" value u32 { 0: u32 }
        static d0 = align 1 hex 01
        static d1 = align 2 hex 02
        sig g0 = (ptr) -> (u64)
        sig g1 = () -> ()
        inst n0 = app@1/main/ident [t0] []
        inst n1 = app@1/main/clean [] []
        inst n2 = app@1/main/dead [] []
        fn f0 = n0 g0 {
            callable v0
        b0(v0: ptr):
            v1: ptr = static d0
            step fail b2
            jump b1
        b1:
            v2: u64 = const 7
            return v2
        b2:
            call f1 g1 ()
            propagate
        b3:
            call f2 g1 ()
            unreachable
        }
        fn f1 = n1 g1 extern
        fn f2 = n2 g1 extern
    ";

const PERMUTED: &str = "core v1
        type t0 = own \"u32\" value u32 { 0: u32 }
        type t1 = own \"u64\" value u64 { 0: u64 }
        static d0 = align 2 hex 02
        static d1 = align 1 hex 01
        sig g0 = () -> ()
        sig g1 = (ptr) -> (u64)
        inst n0 = app@1/main/dead [] []
        inst n1 = app@1/main/clean [] []
        inst n2 = app@1/main/ident [t1] []
        fn f0 = n0 g0 extern
        fn f1 = n1 g0 extern
        fn f2 = n2 g1 {
            callable v0
        b0(v0: ptr):
            v1: ptr = static d1
            step fail b1
            jump b3
        b1:
            call f1 g0 ()
            propagate
        b2:
            call f0 g0 ()
            unreachable
        b3:
            v2: u64 = const 7
            return v2
        }
    ";

fn round_trip(encoded: &EncodedBody) -> Module {
    let decoded = decode(&encoded.bytes).unwrap();
    wf::module(&decoded).unwrap();
    assert_eq!(encode(&decoded, FuncId(0)).bytes, encoded.bytes);
    decoded
}

#[test]
fn row_numbering_ignores_producer_order_and_keeps_failure_targets() {
    let a = checked(ORIGINAL);
    let mut b = checked(PERMUTED);
    let body = b.functions[2].body.as_mut().unwrap();
    let old = std::mem::replace(&mut body.pools.values, vec![ValueId(1)]);
    body.pools.values.extend(old);
    for block in &mut body.blocks {
        block.params.start += 1;
    }
    for term in &mut body.terminators {
        if let Terminator::Return(values) = term {
            values.start += 1;
        }
    }
    for call in &mut body.calls {
        call.results.start += 1;
    }
    for edge in &mut body.edges {
        edge.args.start += 1;
    }
    wf::module(&b).unwrap();
    let a = encode(&a, FuncId(0));
    let b = encode(&b, FuncId(2));
    assert_eq!(a.bytes, b.bytes, "A5 canonical body bytes");
    assert_eq!(
        a.slots,
        vec![Slot::External(FuncId(1))],
        "A5 only reached callees bind"
    );
    let decoded = round_trip(&a);
    let body = decoded.functions[0].body.as_ref().unwrap();
    assert_eq!(body.blocks.len(), 3, "A5 keeps the non-call failure block");
    assert!(matches!(
        body.insts[1],
        Inst::Step {
            fail: Some(BlockId(1))
        }
    ));
    let dead_edit = checked(&ORIGINAL.replace("app@1/main/dead", "app@1/main/other_dead"));
    let dead_edit = encode(&dead_edit, FuncId(0));
    assert_eq!(
        a.hash(),
        dead_edit.hash(),
        "D8 the dead-only call contributes no body bytes"
    );
    assert_eq!(
        a.slots, dead_edit.slots,
        "D8 the dead-only call contributes no slot"
    );
    local_row_permutation();
}

#[test]
fn hints_only_change_the_annex() {
    let a = checked(ORIGINAL);
    let sources = SourceMap::default();
    let sources = sources.positions();
    let (encoded, original) = encode_with_annex(&a, FuncId(0), &sources);
    for hint in 0..3 {
        let mut b = a.clone();
        let body = b.functions[0].body.as_mut().unwrap();
        match hint {
            0 => body.callable_parameters.clear(),
            1 => body.calls[0].cold = true,
            2 => body.edges[0].cold = true,
            _ => unreachable!(),
        }
        let (b, annex) = encode_with_annex(&b, FuncId(0), &sources);
        assert_eq!(
            encoded.hash(),
            b.hash(),
            "A6 hint {hint} is outside the body"
        );
        assert_ne!(
            original.hash(),
            annex.hash(),
            "A6 hint {hint} is in the annex"
        );
        let bytes = annex.encode();
        let decoded_body = round_trip(&b);
        let decoded = Annex::decode(&bytes, &decoded_body, &Debit::unlimited()).unwrap();
        assert_eq!(decoded.encode(), bytes);
        let facts = decoded.0[0].as_ref().unwrap();
        assert_eq!(facts.cold_calls, vec![hint == 1]);
        assert_eq!(facts.cold_edges, vec![hint == 2]);
        assert_eq!(
            facts.callable_parameters,
            if hint == 0 { vec![] } else { vec![0] }
        );
    }
}

#[test]
fn instance_arguments_are_closed_type_rows() {
    let a = checked(ORIGINAL);
    let b = checked(PERMUTED);
    let mut c = a.clone();
    c.instances[0].arguments[0] = TypeId(1);
    let a = encode(&a, FuncId(0));
    let b = encode(&b, FuncId(2));
    let mut dictionary = checked(ORIGINAL);
    dictionary.instances[0]
        .dictionaries
        .push("app@1/impl/selected".into());
    let different_dictionary = encode(&dictionary, FuncId(0));
    let c = encode(&c, FuncId(0));
    assert_eq!(a.hash(), b.hash(), "D6 local TypeId is immaterial");
    assert_ne!(a.hash(), c.hash(), "D6 u64 and u32 separate");
    assert_ne!(
        a.hash(),
        different_dictionary.hash(),
        "D6 dictionary choice separates"
    );
    let identity = |body| {
        let module = round_trip(body);
        module.identity(module.functions[0].instance)
    };
    assert_ne!(identity(&a), identity(&c));
}
#[test]
fn cleanup_only_edit_changes_body_and_failure() {
    let before = "core v1
            sig g0 = (u64) -> (u64)
            sig g1 = () -> ()
            inst n0 = app@1/main/f [] []
            inst n1 = app@1/main/observe [] []
            fn f0 = n0 g0 {
            b0(v0: u64):
                v1: u64 = const 1
                v2: u64 = add v0, v1 fail b1
                return v2
            b1:
                propagate
            }
            fn f1 = n1 g1 {
            b0:
                v0: u64 = const 37
                v1: bool, v2: u64 = limit_enter v0
                branch v1, b1, b2
            b1:
                limit_exit
                return
            b2:
                return
            }
        ";
    let a = encode(&checked(before), FuncId(0));
    let after = before.replace("propagate", "call f1 g1 ()\n propagate");
    let b = encode(&checked(&after), FuncId(0));
    assert_ne!(
        a.hash(),
        b.hash(),
        "D7 the cleanup-only edit changes the body"
    );
    let root = Identity::plain(DefPath::from_full("app@1/main/f").unwrap());
    let address = |body: &EncodedBody| {
        assert!(body.slots.is_empty());
        crate::code_addresses([root.clone()], |_| {
            Some(crate::CodeRow {
                body: body.hash(),
                slots: Vec::new(),
                values: Vec::new(),
            })
        })
        .unwrap()[&root]
    };
    assert_ne!(
        address(&a),
        address(&b),
        "D7 the overflow observer changes the address"
    );
    // The evaluator half (the observer runs only on overflow) is in hirsel-core-eval.
    round_trip(&a);
    round_trip(&b);
}

fn hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn body_group_member_revision_and_builtin_have_fixed_bytes() {
    let module = checked(
        "core v1
sig g0 = () -> (u64)
inst n0 = app@1/main/f [] []
fn f0 = n0 g0 {
             b0:
 v0: u64 = const 7
 return v0
}
",
    );
    let encoded = encode(&module, FuncId(0));
    // Literal v5 wire fields and hashes derived independently from PROG-10.
    // SHA-256 frames the domain and payload with u64le lengths.
    let body = hex(concat!(
        "484952424f4459000500",               // HIRBODY, NUL, u16le version 5
        "000100010009000000",                 // types, () -> u64 signature, statics
        "010c61707040312f6d61696e2f66000000", // instance app@1/main/f
        "01000000010100010000",               // one function and block, const v0
        "0700000000000000",                   // u64le 7
        "0301000001000900000000",             // return v0, compact roots, local rows
    ));
    assert_eq!(encoded.bytes, body);
    let hash = encoded.hash();
    assert_eq!(
        hash.0.as_slice(),
        hex("cc6faed86929dbee021b8be3ecb28a9f352dbdd3a061364216faa0d6f12266c9")
    );
    let identity = module.identity(module.functions[0].instance);
    let addresses = crate::address::code_groups(
        [identity.clone()],
        |_| {
            Some(crate::CodeRow {
                body: hash,
                slots: vec![],
                values: vec![],
            })
        },
        |group, _| {
            assert_eq!(
                group.0.as_slice(),
                hex("b1a0e05ac9a9fa0ff286073cf3bada40f90afd71888557555cc67ca331428f18"),
            )
        },
    )
    .unwrap();
    let address = addresses[&identity];
    assert_eq!(
        address.0.as_slice(),
        hex("821099d1df26d39f0668e1564f7ac3fcbf38fb2696fbc61ace88ba4bcc7df807")
    );
    assert_eq!(
        crate::code_revision(address).0.as_slice(),
        hex("86bda38cdc4437c5eb56f0095d7897dc989ffd0781fc6a6d0786e7282845f0d0")
    );
    let widen = Identity::plain(DefPath::core("float.widen"));
    assert_eq!(
        crate::builtin_address(&widen).unwrap().0.as_slice(),
        hex("bb63c4ef24b5d261f66965edd632ade0cf2ff8d99540fd2be8bef1111448a41e")
    );
    round_trip(&encoded);
}

#[test]
fn explicit_closure_tuples_preserve_published_identity() {
    let module = text::parse(include_str!("../../samples/failure.core")).unwrap();
    let closure = module.closure(TypeId(0));
    // PROG-10's fixed-width v1 tuple for Temp, with a finalizer and no codecs.
    let expected = hex(concat!(
        "0100000000000000",                     // one row
        "040000000000000054656d70",             // Temp
        "010000000000000000000000",             // resource, address, record
        "0100000000000000",                     // one field
        "020000000000000069640000000009000000", // id: scalar u64
        "00000000000000000000000000",           // no shape hooks, grant, send, held
        "01",                                   // finalizer hook present
        "070000000000000073616d706c6573",       // samples
        "010000000000000030",                   // package version 0
        "0100000000000000",                     // one module component
        "07000000000000006661696c757265",       // failure
        "090000000000000054656d702e64726f70",   // Temp.drop
        "00000000000000000000000000000000",     // no dictionaries or type arguments
        "01000000000000000000000001000000",     // one value parameter, ptr
        "00000000000000000000",                 // no results, dest, or never
        "0000",                                 // no clone or codec hooks
    ));
    let empty = Module::default();
    let mut order = ModuleOrder::new(&empty);
    let mut out = Write::module(&mut order);
    wire::encode_closure(&closure.rows, &mut out);
    assert_eq!(out.bytes, expected);

    // SHA-256 over the length-framed domain and tuple above.
    assert_eq!(
        closure.hash.0.as_slice(),
        hex("9debaf6b4ea25b29f5812ee89b1043a97472e3787299e459bf95c5ee563c5254")
    );
}

#[test]
fn retained_layouts_bind_finalizers_but_codec_metadata_does_not() {
    let original = r#"core v1
            type t0 = resource "Alpha" drop f1 { x: u64 }
            type t1 = resource "Beta" drop f2 { x: u64 }
            type t2 = own "Pair" codec f3 { a: t0, b: t1 }
            type t3 = own "Codec[Pair]" { x: u64 }
            sig g0 = (&take t2) -> ()
            sig g1 = (ptr) -> ()
            sig g2 = () -> () dest t3
            inst n0 = app@1/main/dispose [] []
            inst n1 = app@1/main/Alpha [] []
            inst n2 = app@1/main/Beta [] []
            inst n3 = app@1/main/maker [] []
            fn f0 = n0 g0 {
                addr a0 = v0 : t2
            b0(v0: ptr):
                address a0
                drop a0 t2
                return
            }
            fn f1 = n1 g1 extern
            fn f2 = n2 g1 extern
            fn f3 = n3 g2 {
            b0(v0: ptr):
                v1: u64 = const 7
                return
            }
        "#;
    let a = checked(original);
    let mut codec_edit = checked(&original.replace("const 7", "const 8"));
    codec_edit.functions[3].versioned = true;
    assert_eq!(
        encode(&a, FuncId(0)).hash(),
        encode(&codec_edit, FuncId(0)).hash(),
        "D8 a metadata-only codec implementation is outside code identity"
    );
    let mut b = a.clone();
    b.types.swap(0, 1);
    b.functions.swap(1, 2);
    for shape in &mut b.types {
        if let Some(finalizer) = &mut std::sync::Arc::make_mut(shape).finalizer {
            finalizer.0 = 3 - finalizer.0;
        }
    }
    let hirsel_core_layout::Body::Record(fields) =
        &mut std::sync::Arc::make_mut(&mut b.types[2]).body
    else {
        unreachable!()
    };
    fields[0].ty = FieldTy::Inline(TypeRef(1));
    fields[1].ty = FieldTy::Inline(TypeRef(0));
    wf::module(&b).unwrap();
    let measure = |module: &Module| {
        let encoded = encode(module, FuncId(0));
        let slots = encoded
            .slots
            .iter()
            .map(|slot| slot.identity(module))
            .collect::<Vec<_>>();
        (encoded, slots)
    };
    let (a, slots_a) = measure(&a);
    let (b, slots_b) = measure(&b);
    assert_eq!(a.bytes, b.bytes, "A4 finalizer roots follow closure order");
    assert_eq!(slots_a, slots_b);
    let names = slots_a
        .iter()
        .map(|slot| slot.definition.0.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        BTreeSet::from(["Alpha", "Beta"]),
        "D8 drop-only slots, no codec slot"
    );
    let decoded = round_trip(&a);
    assert!(
        decoded.functions.last().unwrap().body.is_none(),
        "D8 codec maker is a declaration"
    );
    let dispose = decoded.identity(decoded.functions[0].instance);
    let caller_address = |alpha| {
        crate::code_addresses([dispose.clone()], |id| {
            let (body, slots) = if *id == dispose {
                (a.hash(), slots_a.clone())
            } else {
                let byte = if id.definition.0.name == "Alpha" {
                    alpha
                } else {
                    7
                };
                (ContentHash([byte; 32]), Vec::new())
            };
            Some(crate::CodeRow {
                body,
                slots,
                values: Vec::new(),
            })
        })
        .unwrap()[&dispose]
    };
    assert_ne!(
        caller_address(7),
        caller_address(8),
        "D8 edits to a nested drop-only finalizer propagate"
    );
}

#[test]
fn source_position_changes_only_the_matching_annex() {
    let mut module = checked(ORIGINAL);
    let root = DefPath::from_full("app@1/main/ident").unwrap();
    let unused = DefPath::from_full("app@1/main/unrelated").unwrap();
    module.items = vec![root.clone(), unused];
    let anchor = |item, start| {
        Some(Anchor {
            item: ItemId(item),
            start,
            end: start + 1,
        })
    };
    module.functions[0].body.as_mut().unwrap().inst_anchors[1] = anchor(0, 0);
    let sources = SourceMap {
        points: Default::default(),
        files: vec![crate::source::SourceFile {
            path: "main.hirsel".into(),
            text: "a\nb".into(),
        }],
        items: [(
            root,
            crate::source::ItemSource {
                file: 0,
                file_offset: 0,
            },
        )]
        .into(),
    };
    let sources = sources.positions();
    let (a, sites_a) = encode_with_annex(&module, FuncId(0), &sources);
    module.items.swap(0, 1);
    module.functions[0].body.as_mut().unwrap().inst_anchors[1] = anchor(1, 0);
    let (permuted, same_sites) = encode_with_annex(&module, FuncId(0), &sources);
    assert_eq!(
        a.bytes, permuted.bytes,
        "A1 the item-table permutation does not enter the body"
    );
    assert_eq!(sites_a, same_sites);
    module.functions[0].body.as_mut().unwrap().inst_anchors[1] = anchor(1, 2);
    let (b, sites_b) = encode_with_annex(&module, FuncId(0), &sources);
    assert_eq!(a.bytes, b.bytes, "A2 source offsets do not enter the body");
    let line = |annex: &Annex| {
        annex.0[0].as_ref().unwrap().instructions[1]
            .as_ref()
            .unwrap()
            .line
    };
    assert_eq!((line(&sites_a), line(&sites_b)), (1, 2));
    let decoded = round_trip(&b);
    assert_eq!(
        Annex::decode(&sites_b.encode(), &decoded, &Debit::unlimited()).unwrap(),
        sites_b
    );
    let mut wrong = sites_b;
    wrong.0[0].as_mut().unwrap().cold_calls.clear();
    assert!(
        Annex::decode(&wrong.encode(), &decoded, &Debit::unlimited()).is_err(),
        "annex must cover the body's calls"
    );
}

/// A memoized unit's annex is read back by `Annex::decode`, so every annex
/// the writer emits must decode against its own body. A decision whose
/// anchor resolves to no site (its item is not in the build's source map, as
/// for a std unit of a copied install) is no coverage site: the writer drops
/// it rather than storing a decision the reader refuses.
#[test]
fn every_written_annex_decodes_with_or_without_its_sources() {
    let mut module = checked(
        "core v1
            sig g0 = (bool) -> (u64)
            inst n0 = app@1/main/pick [] []
            fn f0 = n0 g0 {
            b0(v0: bool):
                branch v0, b1, b2
            b1:
                v1: u64 = const 1
                return v1
            b2:
                v2: u64 = const 2
                return v2
            }
        ",
    );
    let root = DefPath::from_full("app@1/main/pick").unwrap();
    module.items = vec![root.clone()];
    let block = &mut module.functions[0].body.as_mut().unwrap().blocks[0];
    block.anchor = Some(Anchor {
        item: ItemId(0),
        start: 0,
        end: 1,
    });
    block.decision = true;
    wf::module(&module).unwrap();
    let bound = SourceMap {
        points: Default::default(),
        files: vec![crate::source::SourceFile {
            path: "main.hirsel".into(),
            text: "if a".into(),
        }],
        items: [(
            root,
            crate::source::ItemSource {
                file: 0,
                file_offset: 0,
            },
        )]
        .into(),
    };
    for (sources, decided) in [(bound, true), (SourceMap::default(), false)] {
        let sources = sources.positions();
        let (body, written) = encode_with_annex(&module, FuncId(0), &sources);
        let (_, retained) = retain(&module, FuncId(0));
        let decoded = round_trip(&body);
        for annex in [
            written,
            retained_annex(&retained, &sources),
            RetainedSites::take(retained).annex(&sources),
        ] {
            let read = Annex::decode(&annex.encode(), &decoded, &Debit::unlimited())
                .expect("the writer's annex decodes against its body");
            let function = read.0[0].as_ref().unwrap();
            assert_eq!(function.decisions[0], decided, "a decision needs a site");
        }
    }
}

#[test]
fn logical_admission_is_in_the_body() {
    let before = "core v1
            sig g0 = () -> (u64)
            inst n0 = app@1/main/admit [] []
            fn f0 = n0 g0 {
            b0:
                v0: u64 = const 5
                v1: u64 = const 7
                v2: bool, v3: u64 = limit_enter v0
                branch v2, b1, b2
            b1:
                limit_exit
                v4: u64 = const 1
                return v4
            b2:
                return v3
            }
        ";
    let a = encode(&checked(before), FuncId(0));
    let b = encode(
        &checked(&before.replace("limit_enter v0", "limit_enter v1")),
        FuncId(0),
    );
    assert_ne!(
        a.hash(),
        b.hash(),
        "D11 logical admission enters checked code identity"
    );
    // The evaluator half (seven requested, six available) is in hirsel-core-eval.
    round_trip(&a);
    round_trip(&b);
}

#[test]
fn decoder_rejects_missing_or_nonminimal_fields() {
    let a = encode(&checked(ORIGINAL), FuncId(0));
    let mut extra = a.bytes.clone();
    extra.push(0);
    assert!(decode(&extra).is_err());
    let mut redundant = a.bytes.clone();
    redundant[BODY_MAGIC.len()] |= 128;
    redundant.insert(BODY_MAGIC.len() + 1, 0);
    assert!(decode(&redundant).is_err());
    assert!(decode(&a.bytes[..a.bytes.len() - 1]).is_err());
}
/// Blocks, values, calls and edges whose rows are not their canonical order.
const ROWS: &str = r#"core v1
            type t0 = compact u16 "First" { x: u8 }
            type t1 = compact u32 "Second" { x: u64 }
            sig g0 = (u64) -> (u64)
            sig g1 = (u64) -> (u64, u64)
            inst n0 = app@1/main/run [] []
            inst n1 = app@1/main/double [] []
            fn f0 = n0 g0 {
                root t0 2
                root t1 4
                slot s0 = u64
                slot s1 = u64
                addr a0 = s0 : u64
                addr a1 = a0 : u64
                addr a2 = s1 : u64
            b0(v0: u64):
                live s0
                live s1
                address a0
                address a1
                address a2
                store a0, v0
                store a2, v0
                switch v0 [0 -> b1, 1 -> b2] else b3(v0)
            b1:
                v1: u64 = read a1
                v2: u64, v3: u64 = call f1 g1 (v1)
                jump b3(v2)
            b2:
                v4: u64 = read a1
                v5: u64, v6: u64 = call f1 g1 (v4)
                jump b3(v6)
            b3(v7: u64):
                dead s0
                dead s1
                return v7
            }
            fn f1 = n1 g1 extern
        "#;

#[test]
fn retained_assembly_is_the_original_assembly() {
    let mut module = checked(ROWS);
    let root = DefPath::from_full("app@1/main/run").unwrap();
    module.items = vec![root.clone()];
    let body = module.functions[0].body.as_mut().unwrap();
    let anchor = |line: usize| {
        Some(Anchor {
            item: ItemId(0),
            start: 2 * line as u32,
            end: 2 * line as u32 + 1,
        })
    };
    for (line, site) in body.inst_anchors.iter_mut().enumerate() {
        *site = anchor(line);
    }
    let first = body.inst_anchors.len();
    for (line, block) in body.blocks.iter_mut().enumerate() {
        block.anchor = anchor(first + line);
    }
    body.calls[1].cold = true;
    let last = body.edges.len() - 1;
    body.edges[last].cold = true;
    body.callable_parameters = vec![ValueId(6), ValueId(1)];
    let sources = SourceMap {
        points: Default::default(),
        files: vec![crate::source::SourceFile {
            path: "main.hirsel".into(),
            text: "x\n".repeat(64),
        }],
        items: [(
            root,
            crate::source::ItemSource {
                file: 0,
                file_offset: 0,
            },
        )]
        .into(),
    };
    let sources = sources.positions();
    let (code, retained) = retain(&module, FuncId(0));
    let (_, annex, decoded) = prepare_assembly(&module, FuncId(0), &sources);
    assert_eq!(
        prepare_retained_assembly(&retained, &sources),
        (annex, decoded),
        "a retained module's annex is read off in canonical order"
    );
    let named = crate::source::anchored_items(&retained);
    let sites = RetainedSites::take(retained.clone());
    assert_eq!(sites.anchored_items(), named);
    assert_eq!(sites.annex(&sources), retained_annex(&retained, &sources));
    assert_eq!(sites.restore(&code.bytes), retained);
}

fn local_row_permutation() {
    let module = checked(ROWS);
    let mut other = module.clone();
    other.types.swap(0, 1);
    let b = other.functions[0].body.as_mut().unwrap();
    b.compact_roots.reverse();
    for root in &mut b.compact_roots {
        root.ty.0 = 1 - root.ty.0;
    }
    b.values.reverse();
    b.slots.swap(0, 1);
    b.addresses.rotate_right(1);
    b.calls.reverse();
    b.edges.reverse();
    let value = |v: &mut ValueId| v.0 = 7 - v.0;
    let address = |a: &mut AddressId| a.0 = (a.0 + 1) % 3;
    let edge_count = b.edges.len() as u32;
    let edge = |e: &mut EdgeId| e.0 = edge_count - 1 - e.0;
    for a in &mut b.addresses {
        match &mut a.base {
            AddressBase::Slot(s) => s.0 = 1 - s.0,
            AddressBase::Address(a) => address(a),
            AddressBase::Value(_) => unreachable!(),
        }
    }
    for inst in &mut b.insts {
        match inst {
            Inst::StorageLive { slot } | Inst::StorageDead { slot } => slot.0 = 1 - slot.0,
            Inst::Address { address: a, .. } => address(a),
            Inst::Store {
                address: a,
                value: v,
            }
            | Inst::Read {
                address: a,
                result: v,
                ..
            } => {
                address(a);
                value(v);
            }
            Inst::Call { call } => call.0 = 1 - call.0,
            _ => unreachable!(),
        }
    }
    for arg in &mut b.pools.args {
        let Arg::Value(v) = arg else { unreachable!() };
        value(v);
    }
    for v in &mut b.pools.values {
        value(v);
    }
    b.pools.arms.insert(
        0,
        SwitchArm {
            value: 99,
            edge: EdgeId(0),
        },
    );
    for arm in &mut b.pools.arms {
        edge(&mut arm.edge);
    }
    for term in &mut b.terminators {
        match term {
            Terminator::Jump(e) => edge(e),
            Terminator::Switch {
                value: v,
                arms,
                otherwise,
            } => {
                value(v);
                arms.start += 1;
                edge(otherwise);
            }
            Terminator::Return(_) => {}
            _ => unreachable!(),
        }
    }
    wf::module(&other).unwrap();
    let a = encode(&module, FuncId(0));
    let b = encode(&other, FuncId(0));
    assert_eq!(
        a.hash(),
        b.hash(),
        "A5 preserves local rows and compact-root sets"
    );
    round_trip(&b);
}

#[test]
fn versioned_flag_enters_the_body_and_release_copies_do_not() {
    let mut module = checked(
        r#"core v1
            sig g0 = (u8) -> (u64)
            inst n0 = app@1/main/vectors [] []
            fn f0 = n0 g0 versioned {
            b0(v0: u8):
                v1: v16xu8 = simd splat v0
                v2: u64 = const 7
                return v2
            }
        "#,
    );
    let first = encode(&module, FuncId(0)).hash();
    module
        .code_variants
        .insert(FuncId(0), ContentHash([0x31; 32]));
    assert_eq!(
        encode(&module, FuncId(0)).hash(),
        first,
        "A7 backend copies are outside checked code identity"
    );
    module.functions[0].versioned = false;
    assert_ne!(
        encode(&module, FuncId(0)).hash(),
        first,
        "D12 the checked versioned flag enters the body"
    );
}

/// A body whose `n` callees each take a wrapper of one `depth`-deep record chain, so
/// decoding hashes `n` closures that each walk the whole chain.
fn wrapped_chain(n: usize, depth: usize) -> Vec<u8> {
    let mut text = String::from("core v1\n");
    for i in 0..depth {
        let field = if i + 1 < depth {
            format!("t{}", i + 1)
        } else {
            "u64".into()
        };
        text += &format!("type t{i} = own \"Chain{i}\" {{ x: {field} }}\n");
    }
    for j in 0..n {
        text += &format!("type t{} = own \"Wrap{j}\" {{ x: t0 }}\n", depth + j);
    }
    text += "sig g0 = () -> ()\ninst n0 = app@1/main/root [] []\n";
    for j in 0..n {
        text += &format!("inst n{} = app@1/main/take [t{}] []\n", j + 1, depth + j);
    }
    text += "fn f0 = n0 g0 {\nb0:\n";
    for j in 0..n {
        text += &format!("call f{} g0 ()\n", j + 1);
    }
    text += "return\n}\n";
    for j in 0..n {
        text += &format!("fn f{} = n{} g0 extern\n", j + 1, j + 1);
    }
    encode(&checked(&text), FuncId(0)).bytes
}

/// The fewest decoded bytes `bytes` decodes within.
fn decode_bytes(bytes: &[u8]) -> usize {
    let within = |limit| {
        let budget = crate::decode::DecodeBudget::new(crate::decode::DecodeLimits {
            bytes: limit,
            members: usize::MAX,
            slots: usize::MAX,
            depth: usize::MAX,
            deadline: None,
        });
        super::decode(bytes, &budget.debit()).is_ok()
    };
    let (mut low, mut high) = (0, 1 << 30);
    assert!(within(high));
    while low < high {
        let mid = low + (high - low) / 2;
        if within(mid) {
            high = mid
        } else {
            low = mid + 1
        }
    }
    low
}

#[test]
fn decoded_bytes_bound_held_memory_not_hashing_work() {
    // launch.md "Admission" step 3 limits the memory decoded objects hold. A closure is
    // hashed and dropped, so another wrapper of the chain costs its own rows whatever the
    // chain's depth; charging each closure walk for good costs it the chain again.
    let extra =
        |depth| decode_bytes(&wrapped_chain(65, depth)) - decode_bytes(&wrapped_chain(1, depth));
    let (shallow, deep) = (extra(64), extra(256));
    assert!(
        deep < shallow + shallow / 4,
        "64 wrappers cost {shallow} at depth 64, {deep} at 256"
    );
}
