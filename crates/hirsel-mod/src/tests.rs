use crate::{
    Backend, Entry, Fixup, Header, Image, Import, Kind, Malformed, Module, NO_DESCRIPTOR, ROWS,
    Span,
};
use hirsel_core_ids::ContentHash;
use hirsel_core_layout::Target;

fn put(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// One x86-64 Linux module laid out by hand from the PACK-2 tables: 16 bytes of text,
/// one import slot and one data word, one fixup each way, one entry named `f`.
fn sample() -> Vec<u8> {
    let mut b = vec![0u8; 432];
    b[..8].copy_from_slice(b"HIRMOD\0\x01");
    b[16] = 1;
    b[24..56].fill(0xa1);
    b[56..88].fill(0xa2);
    b[88..120].fill(0xa3);
    put(&mut b, 120, 64);
    put(&mut b, 124, 64);
    let sections = [
        (232, 16),
        (248, 16),
        (264, 40),
        (304, 16),
        (320, 16),
        (336, 80),
        (416, 8),
        (424, 0),
        (424, 0),
        (424, 5),
        (432, 0),
        (432, 0),
        (432, 0),
    ];
    for (i, (offset, length)) in sections.into_iter().enumerate() {
        put(&mut b, 128 + 8 * i, offset);
        put(&mut b, 132 + 8 * i, length);
    }
    b[232..248].fill(0x90);
    put(&mut b, 264 + 4, 7);
    put(&mut b, 304, 9);
    b[304 + 4] = 3;
    b[304 + 5] = 4;
    b[304 + 8] = 8;
    put(&mut b, 320, 8);
    b[320 + 4] = 1;
    b[336..368].fill(0x11);
    b[368..400].fill(0x22);
    put(&mut b, 336 + 68, NO_DESCRIPTOR);
    put(&mut b, 416 + 4, 16);
    put(&mut b, 424, 1);
    b[428] = b'f';
    b
}

fn image() -> Image {
    Image {
        header: Header {
            target: Target::X86_64Linux,
            backend: Backend::Cranelift,
            policy: 1,
            abi: ContentHash([0xa1; 32]),
            compiler: ContentHash([0xa2; 32]),
            coreir: ContentHash([0xa3; 32]),
            text_align: 64,
            data_align: 64,
        },
        text: vec![0x90; 16],
        data: vec![0; 8],
        imports: vec![Import::Row(7)],
        text_fixups: vec![Fixup {
            site: 9,
            kind: Kind::Rel32Data,
            bias: 4,
            addend: 8,
        }],
        data_fixups: vec![Fixup {
            site: 8,
            kind: Kind::Abs64Text,
            bias: 0,
            addend: 0,
        }],
        entries: vec![Entry {
            identity: ContentHash([0x11; 32]),
            signature: ContentHash([0x22; 32]),
            text: 0,
            descriptor: NO_DESCRIPTOR,
            waits: false,
        }],
        spans: vec![Span {
            start: 0,
            length: 16,
        }],
        cfi: Vec::new(),
        sites: Vec::new(),
        names: vec!["f".into()],
    }
}

#[test]
fn the_writer_and_reader_agree_with_the_specified_layout() {
    let bytes = sample();
    assert_eq!(image().encode(), bytes);
    let module = Module::parse(&bytes).unwrap();
    let image = image();
    assert_eq!(module.header, image.header);
    assert_eq!(module.text(), &image.text[..]);
    assert_eq!(module.data(), &[0; 16][..]);
    assert_eq!(module.imports().collect::<Vec<_>>(), image.imports);
    assert_eq!(module.text_fixups().collect::<Vec<_>>(), image.text_fixups);
    assert_eq!(module.data_fixups().collect::<Vec<_>>(), image.data_fixups);
    assert_eq!(
        module.entry(&ContentHash([0x11; 32])),
        Some(image.entries[0])
    );
    assert_eq!(module.entry(&ContentHash([0x12; 32])), None);
    assert_eq!(module.spans().collect::<Vec<_>>(), image.spans);
    assert_eq!(module.name(0), "f");
}

#[test]
fn an_entry_descriptor_is_three_whole_words_of_data_past_the_import_slots() {
    // One import slot and 40 more bytes: the data is 48 bytes, so a descriptor's three
    // words fit at 8, 16 and 24 and nowhere else.
    let described = |descriptor| {
        let mut image = image();
        image.data = vec![0; 40];
        image.entries[0].descriptor = descriptor;
        Module::parse(&image.encode()).map(|_| ())
    };
    for fits in [8, 16, 24] {
        assert_eq!(described(fits), Ok(()), "{fits}");
    }
    for refused in [12, 20, 32, 40, 48] {
        assert_eq!(described(refused), Err(Malformed::Entries), "{refused}");
    }
}

#[test]
fn a_module_that_breaks_one_rule_is_refused() {
    use Malformed::*;
    let rows = ROWS.len() as u32;
    let cases: &[(&str, usize, u32, Malformed)] = &[
        ("version", 4, u32::from_le_bytes(*b"D\0\0\x02"), Magic),
        ("target", 8, 5, Header),
        ("backend", 12, 2, Header),
        ("reserved header byte", 12, 0x100, Header),
        ("undefined policy bit", 16, 4, Header),
        ("text alignment", 120, 48, Header),
        ("data alignment", 124, 32, Header),
        ("gap before text", 128, 240, Sections),
        ("section past the file", 132 + 8 * 9, 9, Sections),
        ("reserved section", 132 + 8 * 10, 0, Sections),
        ("import kind", 264, 2, Imports),
        ("import row", 264 + 4, rows, Imports),
        ("row import with an instance", 264 + 8, 1, Imports),
        ("import slot content", 248, 1, Imports),
        ("aarch64 kind in x86-64 text", 304 + 4, 0x0004, TextFixups),
        ("data kind in text", 304 + 4, 0x0002, TextFixups),
        ("bias", 304 + 4, 0x0903, TextFixups),
        ("fixup record padding", 304 + 4, 0x1_0403, TextFixups),
        ("text site past text", 304, 13, TextFixups),
        ("instruction past text", 304 + 4, 0x0803, TextFixups),
        ("text addend past data", 304 + 8, 17, TextFixups),
        ("negative addend", 304 + 12, u32::MAX, TextFixups),
        ("data site in an import slot", 320, 4, DataFixups),
        ("data site past data", 320, 9, DataFixups),
        ("text kind in data", 320 + 4, 0x0403, DataFixups),
        ("data addend past text", 320 + 8, 17, DataFixups),
        ("entry off a span start", 336 + 64, 4, Entries),
        ("descriptor past data", 336 + 68, 16, Entries),
        ("descriptor in an import slot", 336 + 68, 0, Entries),
        ("waits", 336 + 72, 2, Entries),
        ("entry padding", 336 + 76, 1, Entries),
        ("span past text", 416 + 4, 17, Spans),
        ("empty span", 416 + 4, 0, Spans),
        ("name past the names", 424, 2, Names),
        ("unnamed bytes", 424, 0, Names),
    ];
    for (name, at, value, error) in cases {
        let mut bytes = sample();
        put(&mut bytes, *at, *value);
        if *name == "reserved section" {
            put(&mut bytes, 132 + 8 * 10, 8);
            bytes.extend_from_slice(&[0; 8]);
        }
        assert_eq!(Module::parse(&bytes).err(), Some(*error), "{name}");
    }

    let mut utf8 = sample();
    utf8[428] = 0xff;
    assert_eq!(Module::parse(&utf8).err(), Some(Names));
    let mut padding = sample();
    padding[430] = 1;
    assert_eq!(Module::parse(&padding).err(), Some(Sections));
    let mut trailing = sample();
    trailing.extend_from_slice(&[0; 8]);
    assert_eq!(Module::parse(&trailing).err(), Some(Sections));

    // Two fixups on one field, and two entries with one identity.
    let mut twice = image();
    twice.text_fixups.push(Fixup {
        site: 11,
        ..twice.text_fixups[0]
    });
    assert_eq!(Module::parse(&twice.encode()).err(), Some(TextFixups));
    let mut twice = image();
    twice.entries.push(twice.entries[0]);
    assert_eq!(Module::parse(&twice.encode()).err(), Some(Entries));

    // AArch64 text: an instruction field is a whole aligned instruction word.
    let mut arm = image();
    arm.header.target = Target::Aarch64Linux;
    arm.text_fixups[0] = Fixup {
        site: 4,
        kind: Kind::PageData,
        bias: 0,
        addend: 8,
    };
    assert!(Module::parse(&arm.clone().encode()).is_ok());
    arm.text_fixups[0].site = 6;
    assert_eq!(Module::parse(&arm.clone().encode()).err(), Some(TextFixups));
    arm.text_fixups[0] = image().text_fixups[0];
    assert_eq!(Module::parse(&arm.encode()).err(), Some(TextFixups));
}

/// A module comes off a socket: no prefix and no corrupted word may panic the reader.
#[test]
fn truncated_and_corrupted_modules_are_refused_without_a_panic() {
    let bytes = sample();
    for length in 0..bytes.len() {
        assert!(Module::parse(&bytes[..length]).is_err(), "prefix {length}");
    }
    for at in (0..bytes.len() - 3).filter(|at| !(24..120).contains(at)) {
        for value in [0, 1, 7, 8, 0x7fff_ffff, 0x8000_0000, u32::MAX] {
            let mut bytes = bytes.clone();
            put(&mut bytes, at, value);
            if let Ok(module) = Module::parse(&bytes) {
                for i in 0..module.spans().len() {
                    module.name(i);
                }
                let _ = (module.imports().count(), module.entries().count());
                let _ = (module.text_fixups().count(), module.data_fixups().count());
            }
        }
    }
}

#[test]
fn the_row_table_is_strictly_sorted_for_lookup() {
    assert!(ROWS.windows(2).all(|pair| pair[0].0 < pair[1].0));
}

#[test]
fn abi_revision_is_bumped_and_sites_section_is_validated() {
    use hirsel_core_layout::source_positions;

    assert_eq!(crate::ABI, 5);
    let owner_row = crate::row("hirsel_core_cause_owner").expect("cause owner row");
    assert_eq!(ROWS[owner_row as usize].1, crate::Class::Pure);
    for code_sym in [
        "hirsel_core_code_activate",
        "hirsel_core_code_bind",
        "hirsel_core_code_dispatch_begin",
        "hirsel_core_code_dispatch_end",
        "hirsel_core_code_drop",
        "hirsel_core_code_identity",
        "hirsel_core_code_load",
        "hirsel_core_code_revision",
        "hirsel_core_code_share",
        "hirsel_core_code_slot_drop",
        "hirsel_core_code_slot_new",
    ] {
        let idx = crate::row(code_sym).expect(code_sym);
        assert_eq!(ROWS[idx as usize].1, crate::Class::Metered);
    }

    // Valid section-8 sites table passes parsing.
    let encoded = source_positions::encode("line1\nline2\nfail");
    let sites = source_positions::encode_source_table(&[("app/main.hirsel", 1, &encoded)]);
    let mut good = image();
    good.sites = sites.clone();
    let raw = good.encode();
    let parsed = Module::parse(&raw).unwrap();
    assert_eq!(parsed.sites(), &sites[..]);

    // Malformed section-8 sites tables are refused with Malformed::Sites.
    for bad_sites in [
        vec![1, 2, 3], // truncated header
        {
            let mut v = 1u64.to_le_bytes().to_vec();
            v.extend_from_slice(&[0; 20]); // truncated row
            v
        },
        source_positions::encode_source_table(&[
            ("a.hirsel", 20, &encoded),
            ("b.hirsel", 10, &encoded), // unsorted starts
        ]),
        {
            let mut v = source_positions::encode_source_table(&[("a.hirsel", 1, &encoded)]);
            // Corrupt path bytes to invalid UTF-8.
            v[8 + 40] = 0xff;
            v
        },
    ] {
        let mut bad = image();
        bad.sites = bad_sites;
        assert_eq!(Module::parse(&bad.encode()).err(), Some(Malformed::Sites));
    }
}

// tools/decoder-fuzz builds this crate's test configuration with the libFuzzer
// callback for nightly discovery. No other build compiles it.
#[cfg(fuzzing)]
#[path = "../../../tools/decoder-fuzz/host_target.rs"]
mod host_target;
#[path = "../../hirsel-core-driver/src/property_corpus/reviewed.rs"]
mod reviewed;

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Facts {
    backend: String,
    target: u32,
    policy: u64,
    text_align: u32,
    data_align: u32,
    text: usize,
    data: usize,
    imports: usize,
    text_fixups: usize,
    data_fixups: usize,
    entries: usize,
    spans: usize,
    cfi: usize,
    sites: usize,
    names: Vec<String>,
}

#[derive(Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Observed {
    Parsed(Facts),
    Malformed(String),
}

/// The owner predicate of the module reader. Bytes the structural reader
/// accepts go on to every checked view, and the writer must give the same
/// bytes back for what the views hold.
fn observe(bytes: &[u8]) -> Observed {
    let module = match Module::parse(bytes) {
        Ok(module) => module,
        Err(malformed) => return Observed::Malformed(format!("{malformed:?}")),
    };
    let imports: Vec<Import> = module.imports().collect();
    let entries: Vec<Entry> = module.entries().collect();
    let spans: Vec<Span> = module.spans().collect();
    let names: Vec<String> = (0..spans.len())
        .map(|index| module.name(index).to_owned())
        .collect();
    for entry in &entries {
        assert_eq!(module.entry(&entry.identity), Some(*entry));
    }
    let literals = module.literals();
    assert_eq!(literals.start, 8 * imports.len());
    assert_eq!(literals.end, module.data().len());
    let image = Image {
        header: module.header,
        text: module.text().to_vec(),
        data: module.data()[literals.start..].to_vec(),
        imports,
        text_fixups: module.text_fixups().collect(),
        data_fixups: module.data_fixups().collect(),
        entries,
        spans,
        cfi: module.cfi().to_vec(),
        sites: module.sites().to_vec(),
        names,
    };
    let facts = Facts {
        backend: match image.header.backend {
            Backend::Cranelift => "cranelift".into(),
            Backend::Llvm => "llvm".into(),
        },
        target: image.header.target.index() as u32,
        policy: image.header.policy,
        text_align: image.header.text_align,
        data_align: image.header.data_align,
        text: image.text.len(),
        data: module.data().len(),
        imports: image.imports.len(),
        text_fixups: image.text_fixups.len(),
        data_fixups: image.data_fixups.len(),
        entries: image.entries.len(),
        spans: image.spans.len(),
        cfi: image.cfi.len(),
        sites: image.sites.len(),
        names: image.names.clone(),
    };
    assert!(
        image.encode() == bytes,
        "the writer does not give back an accepted module"
    );
    Observed::Parsed(facts)
}

/// Discovery's entry to the owner predicate. `expect` is a declared status
/// that the observation must also meet. Returns the observation.
#[cfg(fuzzing)]
fn discovered(input: &[u8], _schedule: usize, expect: Option<&str>) -> String {
    let observation = match observe(input) {
        Observed::Parsed(facts) => format!(
            "parsed {} text={} data={} imports={} entries={} spans={}",
            facts.backend, facts.text, facts.data, facts.imports, facts.entries, facts.spans
        ),
        Observed::Malformed(table) => format!("malformed {table}"),
    };
    if let Some(expect) = expect {
        let status = observation.split(' ').next().unwrap();
        assert_eq!(status, expect, "discovered: declared status");
    }
    observation
}

#[test]
fn reviewed_module() {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Limits {
        bytes: usize,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Setup {
        backend: String,
        product: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Schedule {
        kind: String,
    }
    type Case = reviewed::Case<Limits, Setup, Observed, Schedule>;
    let cases: Vec<(Case, Vec<u8>)> = reviewed::read(
        include_bytes!("../../../core/tests/decoder-corpus/module.json"),
        "module",
        "reviewed_module",
        "hirsel-mod",
        "pack2-layout",
    )
    .unwrap();
    let mut products = Vec::new();
    let mut refused = 0;
    for (case, bytes) in cases {
        let id = &case.id;
        assert!(bytes.len() <= case.limits.bytes, "{id}");
        assert!(
            matches!(case.schedules.as_slice(), [one] if one.kind == "contiguous"),
            "{id}"
        );
        // Two replays, each over its own copy of the bytes.
        let first = observe(&bytes.clone());
        assert_eq!(observe(&bytes.clone()), first, "{id}: replay");
        assert_eq!(first, case.expected, "{id}");
        match &first {
            Observed::Parsed(facts) => {
                assert_eq!(case.setup.product, "compiler", "{id}");
                assert_eq!(facts.backend, case.setup.backend, "{id}");
                products.push(facts.backend.clone());
                eprintln!(
                    "{id}: parsed {} bytes twice, {} entries, {} spans, writer agrees",
                    bytes.len(),
                    facts.entries,
                    facts.spans
                );
            }
            Observed::Malformed(table) => {
                assert_eq!(case.setup.product, "edited", "{id}");
                refused += 1;
                eprintln!("{id}: malformed {table} twice");
            }
        }
    }
    // One trusted product of each backend, and refusals of the structural reader.
    assert_eq!(products, ["cranelift", "llvm"]);
    assert!(refused > 0);
}
