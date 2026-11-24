//! Admission of foreign archives: their inventory, pins and decoded contents.
use super::*;

#[test]
fn foreign_archive_objects_follow_grant_inventory_and_raw_pins() {
    use hirsel_core_ir::release_recipe::{
        ForeignFunction, ForeignLeaf, ForeignTarget, GrantBinding, GrantKind,
    };
    use sha2::{Digest, Sha256};

    let harness = Harness::new(LARGE);
    let local = tempfile::tempdir().unwrap();
    let store = CodeStore::new(local.path());
    let archive = store.put(ObjectKind::ForeignArchive, b"abc").unwrap();
    assert_eq!(
        crate::call::hex(&archive.hash.0),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let mut fixture = Fixture::new("entry");
    let body = fixture.body("entry", "(u64)", "", "return");
    fixture.recipe.address = fixture.group(&[(body, vec![])], 0);
    fixture.recipe.launch.grants.push(GrantBinding {
        ordinal: 0,
        kind: GrantKind::Foreign,
        values: vec!["ffi_laws@1/laws/Laws".into()],
        archive: Some(archive),
        policy: None,
    });
    let admit = |fixture: &Fixture, archive_bytes: &[u8]| {
        harness.run(limits(), &NativeSemantics, |peer| {
            for key in start(peer, fixture) {
                let bytes = if key.kind == ObjectKind::ForeignArchive {
                    archive_bytes
                } else {
                    &fixture.objects[&key]
                };
                send_object(peer, key, bytes);
            }
            peer.write_all(&[0]).unwrap();
        })
    };
    assert!(
        matches!(admit(&fixture, b"abc"), Err(AdmissionError::Missing(hash)) if hash == archive.hash)
    );

    fixture.recipe.foreign_archives.push(archive);
    assert!(
        matches!(admit(&fixture, b"abd"), Err(AdmissionError::Hash(hash)) if hash == archive.hash)
    );
    assert!(admit(&fixture, b"abc").is_ok());

    fixture.recipe.launch.grants.clear();
    assert!(
        matches!(admit(&fixture, b"abc"), Err(AdmissionError::Extra(hash)) if hash == archive.hash)
    );

    // The address binds the full table even when the selected archive is unchanged.
    let grant = GrantBinding {
        ordinal: 0,
        kind: GrantKind::Foreign,
        values: vec!["app@1/main/I".into()],
        archive: Some(archive),
        policy: None,
    };
    let header = "(core@1/core/Foreign[app@1/main/I])->()";
    let definition = DefPath::from_full("app@1/native/f").unwrap();
    let leaf = ForeignLeaf {
        module: DefPath::from_full("app@1/native").unwrap(),
        symbols: "hfl_".into(),
        targets: vec![ForeignTarget {
            target: TARGET.into(),
            archive,
        }],
        functions: vec![ForeignFunction {
            identity: Identity::plain(definition),
            header: header.into(),
        }],
    };
    let mut table = b"\x18x86_64-unknown-linux-gnu".to_vec();
    table.extend_from_slice(&archive.hash.0);
    table.push(3);
    let mut address_frame =
        b"\x0eapp@1/native/f\0\0\x27(core@1/core/Foreign[app@1/main/I])->()\x05hfl_f".to_vec();
    address_frame.extend_from_slice(&Sha256::digest(table));
    let address = ContentHash::of_parts("hirsel.code-foreign.v1", [address_frame.as_slice()]);

    fixture.recipe.launch.grants.push(grant.clone());
    fixture.recipe.foreign_modules.push(leaf.clone());
    assert!(matches!(admit(&fixture, b"abc"), Err(AdmissionError::Extra(hash)) if hash == address));

    let mut fixture = Fixture::new("entry");
    fixture.recipe.launch.grants.push(grant);
    fixture.recipe.foreign_modules.push(leaf.clone());
    fixture.recipe.foreign_archives.push(archive);
    let externs = "inst n1 = app@1/native/f [] []\nfn f1 = n1 g0 extern\n";
    let body = fixture.body("entry", "(u64)", externs, "call f1 g0 (v0)\nreturn");
    fixture.recipe.address = fixture.group(&[(body, vec![Binding::External(address)])], 0);
    let error = failure(admit(&fixture, b"abc")).to_string();
    assert!(error.ends_with("is not an indexed ar archive"), "{error}");

    // An exact FFI-9 address and matching identity cannot substitute for the
    // slot's machine signature. A recipe may otherwise make the shared bridge
    // return through a different register class than its caller expects.
    let mut wrong_abi = Fixture::new("entry");
    wrong_abi.recipe.launch.grants = fixture.recipe.launch.grants.clone();
    wrong_abi.recipe.foreign_modules.push(leaf);
    wrong_abi.recipe.foreign_archives.push(archive);
    let body = wrong_abi.body(
        "entry",
        "(u64)",
        "sig g1 = (u64) -> (f64)\ninst n1 = app@1/native/f [] []\nfn f1 = n1 g1 extern\n",
        "v1: f64 = call f1 g1 (v0)\nreturn",
    );
    wrong_abi.recipe.address = wrong_abi.group(&[(body, vec![Binding::External(address)])], 0);
    assert!(matches!(
        admit(&wrong_abi, b"abc"),
        Err(AdmissionError::Binding(
            "slot signature differs from its bound foreign header"
        ))
    ));

    fixture.recipe.foreign_modules[0].targets.insert(
        0,
        ForeignTarget {
            target: "aarch64-unknown-linux-gnu".into(),
            archive: ObjectRef {
                hash: ContentHash(Sha256::digest(b"").into()),
                len: 0,
            },
        },
    );
    assert!(
        matches!(admit(&fixture, b"abc"), Err(AdmissionError::Missing(hash)) if hash == address)
    );
}

// launch.md "Admission" step 3 bounds the memory decoded objects hold, and a
// sealed archive holds its index and the members' symbols and ABI text.
#[test]
fn foreign_archive_contents_count_against_decoded_bytes() {
    use hirsel_core_ir::address::foreign_address;
    use hirsel_core_ir::release_recipe::{
        ForeignFunction, ForeignLeaf, ForeignTarget, GrantBinding, GrantKind,
    };
    use object::write::{Object, StandardSection, Symbol, SymbolSection};
    use object::{
        Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
    };
    use sha2::{Digest, Sha256};

    // One member defining hfl_f, whose ABI text is its line and a long comment.
    const ABI_BYTES: usize = 1 << 18;
    let mut abi = b"hfl_f stack () -> ()\n".to_vec();
    abi.resize(ABI_BYTES - 1, b'#');
    abi.push(b'\n');
    let mut elf = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let text = elf.section_id(StandardSection::Text);
    let at = elf.append_section_data(text, &[0xc3], 1);
    elf.add_symbol(Symbol {
        name: b"hfl_f".to_vec(),
        value: at,
        size: 1,
        kind: SymbolKind::Text,
        scope: SymbolScope::Dynamic,
        weak: false,
        section: SymbolSection::Section(text),
        flags: SymbolFlags::None,
    });
    elf.add_section(vec![], b".note.GNU-stack".to_vec(), SectionKind::Note);
    let section = elf.add_section(vec![], b".hirsel_abi".to_vec(), SectionKind::ReadOnlyData);
    elf.append_section_data(section, &abi, 1);
    let mut member = elf.write().unwrap();
    // A GNU archive: its symbol index names the one member header.
    let header = |name: &str, size: usize| {
        format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{size:<10}`\n", 0, 0, 0, 0)
    };
    let index_len = 4 + 4 + b"hfl_f\0".len();
    let member_at = (8 + 60 + index_len) as u32;
    let mut archive = b"!<arch>\n".to_vec();
    archive.extend_from_slice(header("/", index_len).as_bytes());
    archive.extend_from_slice(&1_u32.to_be_bytes());
    archive.extend_from_slice(&member_at.to_be_bytes());
    archive.extend_from_slice(b"hfl_f\0");
    archive.extend_from_slice(header("laws.o/", member.len()).as_bytes());
    if member.len() % 2 == 1 {
        member.push(b'\n');
    }
    archive.append(&mut member);

    let mut fixture = Fixture::new("entry");
    let archive = fixture.object(ObjectKind::ForeignArchive, archive);
    let leaf = ForeignLeaf {
        module: DefPath::from_full("app@1/native").unwrap(),
        symbols: "hfl_".into(),
        targets: vec![ForeignTarget {
            target: TARGET.into(),
            archive,
        }],
        functions: vec![ForeignFunction {
            identity: Identity::plain(DefPath::from_full("app@1/native/f").unwrap()),
            header: "(core@1/core/Foreign[app@1/main/I])->()".into(),
        }],
    };
    let mut rows = Vec::new();
    leaf.encode_archive_rows(&mut rows);
    let address = foreign_address(
        &leaf.functions[0],
        &leaf.symbols,
        ContentHash(Sha256::digest(&rows).into()),
    );
    fixture.recipe.launch.grants.push(GrantBinding {
        ordinal: 0,
        kind: GrantKind::Foreign,
        values: vec!["app@1/main/I".into()],
        archive: Some(archive),
        policy: None,
    });
    fixture.recipe.foreign_modules.push(leaf);
    fixture.recipe.foreign_archives.push(archive);
    let externs = "inst n1 = app@1/native/f [] []\nfn f1 = n1 g0 extern\n";
    let body = fixture.body("entry", "(u64)", externs, "call f1 g0 (v0)\nreturn");
    fixture.recipe.address = fixture.group(&[(body, vec![Binding::External(address)])], 0);

    let harness = Harness::new(LARGE);
    let admit = |decoded_bytes| {
        let limits = AdmissionLimits {
            decoded_bytes,
            ..limits()
        };
        harness
            .run(limits, &NativeSemantics, |peer| fixture.send(peer))
            .map(drop)
    };
    admit(LARGE as usize).unwrap();
    // The ABI text alone is more than the budget holds.
    assert!(matches!(
        admit(ABI_BYTES - 1),
        Err(AdmissionError::Limit(Limit::DecodeBytes))
    ));
}
