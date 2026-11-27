//! The loader's laws. Expected values come from PACK-2's fixup table, PACK-4's publish
//! and recycle steps and PACK-6's counts, worked by hand.

use super::*;
use hirsel_core_layout::{Arch, Target};
use hirsel_mod::{Backend, Class, Entry, Fixup, Header, Kind, NO_DESCRIPTOR, Span};

type Leaf = unsafe extern "C" fn(u64) -> u64;
// SAFETY: the law modules' one entry takes and returns a word.
unsafe impl Signature for Leaf {
    const DIGEST: ContentHash = ContentHash([9; 32]);
}
type Other = unsafe extern "C" fn();
// SAFETY: no entry has this signature.
unsafe impl Signature for Other {
    const DIGEST: ContentHash = ContentHash([8; 32]);
}

const IDENTITY: ContentHash = ContentHash([0xaa; 32]);
const ABI: ContentHash = ContentHash([1; 32]);

extern "C" fn double(x: u64) -> u64 {
    2 * x
}

fn fixup(site: u32, kind: Kind, bias: u8, addend: i64) -> Fixup {
    Fixup {
        site,
        kind,
        bias,
        addend,
    }
}

/// `leaf(x) = row(x) + k`, where the row is import 0 and `k` is the word at `data[8]`.
#[cfg(target_arch = "x86_64")]
fn leaf() -> (Vec<u8>, Vec<Fixup>) {
    #[rustfmt::skip]
    let text = vec![
        0x48, 0x8b, 0x05, 0, 0, 0, 0, // 0: mov rax, [rip + slot 0]
        0x48, 0x83, 0xec, 0x08,       // 7: sub rsp, 8
        0xff, 0xd0,                   // 11: call rax
        0x48, 0x83, 0xc4, 0x08,       // 13: add rsp, 8
        0x48, 0x03, 0x05, 0, 0, 0, 0, // 17: add rax, [rip + k]
        0xc3,                         // 24: ret
    ];
    let fixups = vec![
        fixup(3, Kind::Rel32Data, 4, 0),
        fixup(20, Kind::Rel32Data, 4, 8),
    ];
    (text, fixups)
}

#[cfg(target_arch = "aarch64")]
fn leaf() -> (Vec<u8>, Vec<Fixup>) {
    let words: [u32; 9] = [
        0xa9bf_7bfd, // 0: stp x29, x30, [sp, #-16]!
        0x9000_0010, // 4: adrp x16, slot 0
        0xf940_0210, // 8: ldr x16, [x16, low 12]
        0xd63f_0200, // 12: blr x16
        0x9000_0010, // 16: adrp x16, k
        0xf940_0210, // 20: ldr x16, [x16, low 12]
        0x8b10_0000, // 24: add x0, x0, x16
        0xa8c1_7bfd, // 28: ldp x29, x30, [sp], #16
        0xd65f_03c0, // 32: ret
    ];
    let fixups = vec![
        fixup(4, Kind::PageData, 0, 0),
        fixup(8, Kind::Low12LoadData, 0, 0),
        fixup(16, Kind::PageData, 0, 8),
        fixup(20, Kind::Low12LoadData, 0, 8),
    ];
    (
        words.iter().flat_map(|word| word.to_le_bytes()).collect(),
        fixups,
    )
}

fn image(k: u64) -> hirsel_mod::Image {
    let (text, text_fixups) = leaf();
    // An image's data follows its import slots, which the writer lays down.
    let data = k.to_le_bytes().to_vec();
    hirsel_mod::Image {
        header: Header {
            target: TARGET,
            backend: Backend::Cranelift,
            policy: 0,
            abi: ABI,
            compiler: ContentHash([2; 32]),
            coreir: ContentHash([k as u8; 32]),
            text_align: 64,
            data_align: 64,
        },
        spans: vec![Span {
            start: 0,
            length: text.len() as u32,
        }],
        names: vec!["leaf".into()],
        text,
        data,
        imports: vec![Import::Row(hirsel_mod::row("hirsel_core_alloc").unwrap())],
        text_fixups,
        data_fixups: vec![],
        entries: vec![Entry {
            identity: IDENTITY,
            signature: Leaf::DIGEST,
            text: 0,
            descriptor: NO_DESCRIPTOR,
            waits: false,
        }],
        cfi: vec![],
        sites: vec![],
    }
}

fn config() -> Config {
    Config {
        abi: ABI,
        policy: 0,
        mask: Mask::PURE,
        band_bytes: 16 << 20,
        installed_bytes: 1 << 20,
        retired_bytes: Config::RETIRED_BYTES,
        batch: Config::BATCH,
        row: Box::new(|_| Some(double as *const () as usize)),
        instance: Box::new(|_| None),
        prelinked: &[],
    }
}

/// Admit and load `bytes`, publishing at once.
fn load(loader: &Loader, bytes: &[u8]) -> Result<Code<Leaf>, Refusal> {
    if let Ok(module) = Module::parse(bytes) {
        loader.admit(module.header.key(), hirsel_mod::digest(bytes));
    }
    loader.load(bytes, &IDENTITY, true)
}

fn call(code: &Code<Leaf>, x: u64) -> u64 {
    // SAFETY: the law module's entry is `leaf`.
    unsafe { (code.entry())(x) }
}

/// The bytes of one law image: its text and its two data words.
fn size() -> u64 {
    leaf().0.len() as u64 + 16
}

/// Each kind's field holds the value its row of the PACK-2 table gives, and each way a
/// fixup can be wrong is refused by its index with the text table's records first.
#[test]
fn a_fixup_patches_the_field_its_kind_names_or_is_refused() {
    let (t, d) = (0x1000_0000u64, 0x5000_0000u64);
    let load = [0x48, 0x8b, 0x05, 0, 0, 0, 0, 0x90];
    let patch = |text: &[u8], data: usize, in_text: &[Fixup], in_data: &[Fixup], arch, d| {
        let (mut text, mut data) = (text.to_vec(), vec![0; data]);
        let done = fixup::apply(
            arch,
            (&mut text, t),
            (&mut data, d),
            in_text.iter().copied(),
            in_data.iter().copied(),
        );
        done.map(|()| (text, data))
    };
    let x86 =
        |in_text: &[Fixup], in_data: &[Fixup]| patch(&load, 16, in_text, in_data, Arch::X86_64, d);

    let rel = fixup(3, Kind::Rel32Data, 4, 8);
    let (text, data) = x86(
        &[rel],
        &[
            fixup(0, Kind::Abs64Text, 0, 8),
            fixup(8, Kind::Abs64Data, 0, 16),
        ],
    )
    .unwrap();
    // D + 8 - (T + 3 + 4) = 0x4000_0001.
    assert_eq!(text[3..7], 0x4000_0001u32.to_le_bytes());
    // One past the end of each section is a target.
    assert_eq!(data[..8], (t + 8).to_le_bytes());
    assert_eq!(data[8..], (d + 16).to_le_bytes());

    for (wrong, why) in [
        (
            fixup(3, Kind::Rel32Data, 4, 17),
            "a target past the data's end",
        ),
        (fixup(3, Kind::Rel32Data, 4, -1), "a target before the data"),
        (fixup(3, Kind::Rel32Data, 3, 8), "a bias below 4"),
        (
            fixup(3, Kind::Rel32Data, 6, 8),
            "a bias past the text's end",
        ),
        (
            fixup(4, Kind::Rel32Data, 4, 8),
            "a field that follows no rip-relative ModRM",
        ),
        (
            fixup(0, Kind::Rel32Data, 4, 8),
            "a field at the text's start",
        ),
        (fixup(3, Kind::PageData, 0, 8), "an aarch64 kind"),
        (
            fixup(3, Kind::Abs64Data, 0, 8),
            "a data kind in the text table",
        ),
    ] {
        assert_eq!(x86(&[wrong], &[]), Err(0), "{why}");
    }
    assert_eq!(x86(&[rel, rel], &[fixup(0, Kind::Abs64Text, 0, 9)]), Err(2));
    assert_eq!(x86(&[], &[fixup(9, Kind::Abs64Data, 0, 0)]), Err(0));
    assert_eq!(x86(&[], &[fixup(0, Kind::Rel32Data, 4, 0)]), Err(0));
    // A text 2 GiB below its data cannot hold the distance.
    assert_eq!(
        patch(&load, 16, &[rel], &[], Arch::X86_64, t + (1 << 31)),
        Err(0)
    );

    let words: [u32; 5] = [
        0x9000_0010, // 0: adrp x16
        0xf940_0210, // 4: ldr x16, [x16]
        0x9100_0021, // 8: add x1, x1, #0
        0xb940_0042, // 12: ldr w2, [x2]
        0x3dc0_0020, // 16: ldr q0, [x1]
    ];
    let code: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let arm = |in_text: &[Fixup]| patch(&code, 0x1000, in_text, &[], Arch::Aarch64, 0x1234_5000);
    let (text, _) = arm(&[
        fixup(0, Kind::PageData, 0, 0x678),
        fixup(4, Kind::Low12LoadData, 0, 0x678),
        fixup(8, Kind::Low12AddData, 0, 0x678),
        fixup(12, Kind::Low12LoadData, 0, 0x67c),
        fixup(16, Kind::Low12LoadData, 0, 0x670),
    ])
    .unwrap();
    let word = |at: usize| u32::from_le_bytes(text[at..at + 4].try_into().unwrap());
    // Page 0x12345 less page 0x10000 is 0x2345: low two bits 1, the rest 0x8d1.
    assert_eq!(word(0), 0x9000_0010 | 1 << 29 | 0x8d1 << 5);
    // 0x678 in 8-byte units is 0xcf; 0x67c in 4-byte units is 0x19f; 0x670 in 16-byte
    // units is 0x67.
    assert_eq!(word(4), 0xf940_0210 | 0xcf << 10);
    assert_eq!(word(8), 0x9100_0021 | 0x678 << 10);
    assert_eq!(word(12), 0xb940_0042 | 0x19f << 10);
    assert_eq!(word(16), 0x3dc0_0020 | 0x67 << 10);
    for (wrong, why) in [
        (fixup(4, Kind::PageData, 0, 0), "a page fixup on a load"),
        (
            fixup(0, Kind::Low12LoadData, 0, 0),
            "a load fixup on an adrp",
        ),
        (fixup(4, Kind::Low12AddData, 0, 0), "an add fixup on a load"),
        (
            fixup(4, Kind::Low12LoadData, 0, 0x67c),
            "an 8-byte load of an address that is not a multiple of 8",
        ),
        (
            fixup(16, Kind::Low12LoadData, 0, 0x678),
            "a 16-byte load of an address that is not a multiple of 16",
        ),
        (
            fixup(2, Kind::PageData, 0, 0),
            "a site inside an instruction",
        ),
        (fixup(0, Kind::PageData, 4, 0), "a bias"),
        (fixup(0, Kind::Rel32Data, 4, 0), "an x86-64 kind"),
        (fixup(20, Kind::PageData, 0, 0), "a site past the text"),
        (
            fixup(0, Kind::PageData, 0, 0x1001),
            "a target past the data's end",
        ),
    ] {
        assert_eq!(arm(&[wrong]), Err(0), "{why}");
    }
}

/// A loaded entry runs with its import bound and its data fixed up; the published
/// pages are never writable; a refusal installs nothing.
#[test]
fn a_loaded_entry_runs_and_a_refused_module_installs_nothing() {
    if !host::slab() {
        return;
    }
    let loader = Loader::new(config()).unwrap();
    let bytes = image(1000).encode();
    assert_eq!(
        loader.load::<Leaf>(&bytes, &IDENTITY, true).err(),
        Some(Refusal::NotAdmitted)
    );
    let code = load(&loader, &bytes).unwrap();
    assert_eq!(call(&code, 21), 1042);
    assert_eq!(loader.installed(), size());
    assert_eq!(host::protection(code.text()), host::LIVE);
    assert_eq!(host::protection(loader.0.data(0)), "r--p");
    // The text run's bytes outside the image are the trap pattern.
    let page = loader.0.page;
    let pattern: [u8; 4] = match TARGET.arch() {
        Arch::X86_64 => [0xcc; 4],
        Arch::Aarch64 => 0xd420_0000u32.to_le_bytes(),
    };
    for offset in leaf().0.len()..page {
        // SAFETY: the page is mapped readable.
        let byte = unsafe { *((code.text() + offset) as *const u8) };
        assert_eq!(byte, pattern[offset % 4], "{offset}");
    }

    // The same digest is the same image.
    let again = load(&loader, &bytes).unwrap();
    assert_eq!((again.text(), loader.installed()), (code.text(), size()));
    assert_eq!(call(&again, 1), 1002);

    assert_eq!(
        loader.load::<Other>(&bytes, &IDENTITY, true).err(),
        Some(Refusal::NoEntry)
    );
    let absent = ContentHash([0xab; 32]);
    assert_eq!(
        loader.load::<Leaf>(&bytes, &absent, true).err(),
        Some(Refusal::NoEntry)
    );

    let refused = |change: &dyn Fn(&mut hirsel_mod::Image), loader: &Loader| {
        let mut image = image(2000);
        change(&mut image);
        let before = loader.installed();
        let refusal = load(loader, &image.encode()).err();
        assert_eq!(loader.installed(), before);
        refusal
    };
    assert_eq!(
        refused(&|image| image.header.abi = ContentHash([7; 32]), &loader),
        Some(Refusal::Mismatch("abi"))
    );
    assert_eq!(
        refused(&|image| image.header.policy = 1, &loader),
        Some(Refusal::Mismatch("policy"))
    );
    let elsewhere = |image: &mut hirsel_mod::Image| {
        image.header.target = Target::Aarch64Darwin;
        (image.text_fixups, image.imports) = (vec![], vec![]);
    };
    assert_eq!(
        refused(&elsewhere, &loader),
        Some(Refusal::Mismatch("target"))
    );
    // A field that follows no instruction of its kind's form.
    let misplaced = |image: &mut hirsel_mod::Image| match TARGET.arch() {
        Arch::X86_64 => image.text_fixups[0].site += 4,
        Arch::Aarch64 => image.text_fixups[0].site = 0,
    };
    assert_eq!(refused(&misplaced, &loader), Some(Refusal::Fixup(0)));
    assert!(matches!(
        load(&loader, &bytes[..bytes.len() - 1]),
        Err(Refusal::Malformed(_))
    ));
    let instance = ContentHash([5; 32]);
    assert_eq!(
        refused(
            &|image| image.imports[0] = Import::Instance(instance),
            &loader
        ),
        Some(Refusal::Unbound(instance))
    );

    let row = hirsel_mod::row("hirsel_core_alloc").unwrap();
    let pure = Loader::new(Config {
        mask: Mask::of(&[Class::Pure]),
        ..config()
    })
    .unwrap();
    assert_eq!(refused(&|_| {}, &pure), Some(Refusal::Import(row)));
    let full = Loader::new(Config {
        installed_bytes: size(),
        ..config()
    })
    .unwrap();
    let _held = load(&full, &bytes).unwrap();
    assert_eq!(refused(&|_| {}, &full), Some(Refusal::Capacity));

    // After the refusals the next module still gets a cohort of its own and runs.
    let next = load(&loader, &image(3000).encode()).unwrap();
    assert_eq!((call(&next, 1), next.text() % page), (3002, 0));
    loader.quarantine(next.digest());
    assert_eq!(
        load(&loader, &image(3000).encode()).err(),
        Some(Refusal::Quarantined)
    );
}

/// Set in a child process that a law starts to do the one thing that ends a process.
const CHILD: &str = "HIRSEL_CODE_LAW";

/// Run the law `name` of this test binary again as a child with `CHILD` set to `role`.
fn child(name: &str, role: &std::ffi::OsStr) -> std::process::ExitStatus {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, role)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
}

/// G11: a jump into a dead cohort faults, its pages are returned, and the range runs
/// new code after it is published again (PACK-4 step 5).
#[test]
fn a_dead_cohort_faults_and_its_range_is_published_again() {
    if !host::slab() {
        return;
    }
    let loader = Loader::new(config()).unwrap();
    let first = load(&loader, &image(100).encode()).unwrap();
    let (entry, text) = (first.entry(), first.text());
    assert_eq!(call(&first, 1), 102);
    drop(first);
    loader.reclaim();
    if std::env::var_os(CHILD).is_some() {
        // The child jumps into the dead cohort, which ends it.
        // SAFETY: none; the law is that this call faults.
        unsafe { entry(1) };
        std::process::exit(0);
    }
    assert_eq!(loader.installed(), 0);
    assert_eq!(host::protection(text), "---p");
    assert_eq!(host::protection(loader.0.data(0)), "---p");
    let name = "tests::a_dead_cohort_faults_and_its_range_is_published_again";
    let ended = child(name, "jump".as_ref());
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&ended),
        Some(host::FAULT)
    );

    // Each later cohort takes the next pages of the writable window; when the window
    // is used up the lowest free run, the dead cohort's, opens again.
    let again = (1..=1 + WINDOW as u64 / loader.0.page as u64)
        .map(|k| (k, load(&loader, &image(k).encode()).unwrap()))
        .find(|(_, code)| code.text() == text);
    let (k, code) = again.expect("the dead cohort's range opens again");
    assert_eq!(
        (call(&code, 1), host::protection(text)),
        (2 + k, host::LIVE.into())
    );
}

/// Installs of one batch share a cohort, none is reachable before the batch is
/// published, and the cohort lives until its last image dies (PACK-4 steps 3 and 5).
#[test]
fn a_batch_is_one_cohort_that_lives_until_its_last_image_dies() {
    if !host::slab() {
        return;
    }
    let loader = Loader::new(Config {
        batch: Duration::from_secs(60),
        ..config()
    })
    .unwrap();
    let (early, late) = (image(1).encode(), image(2).encode());
    for bytes in [&early, &late] {
        loader.admit(
            Module::parse(bytes).unwrap().header.key(),
            hirsel_mod::digest(bytes),
        );
    }
    let started = Instant::now();
    let (waiting, flushed) = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| loader.load::<Leaf>(&early, &IDENTITY, false).unwrap());
        while loader.installed() == 0 {
            std::thread::yield_now();
        }
        // The first install is in an open cohort: writable, not executable.
        assert_eq!(host::protection(loader.0.text(0)), host::OPEN);
        let flushed = loader.load::<Leaf>(&late, &IDENTITY, true).unwrap();
        (waiting.join().unwrap(), flushed)
    });
    // The flush published both; the first did not wait out its minute.
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(waiting.text(), loader.0.text(0));
    // The second image starts at the next 64-byte line after the first.
    assert_eq!(
        flushed.text(),
        waiting.text() + leaf().0.len().next_multiple_of(64)
    );
    assert_eq!((call(&waiting, 1), call(&flushed, 1)), (3, 4));

    drop(waiting);
    loader.reclaim();
    // A dead image in a live cohort: a live image shares its page, so the page stays
    // executable, and its bytes still count as installed.
    assert_eq!(host::protection(loader.0.text(0)), host::LIVE);
    assert_eq!((call(&flushed, 1), loader.installed()), (4, 2 * size()));
    drop(flushed);
    loader.reclaim();
    assert_eq!(
        (host::protection(loader.0.text(0)), loader.installed()),
        ("---p".into(), 0)
    );
}

/// PACK-4 step 5 and PACK-9: when an image dies in a cohort that lives on, its address
/// stops resolving in every chunk it spanned and each page that held only its text is
/// closed, while a batch neighbour on its last page still runs and resolves. Dropping
/// the last handle does not wait for the loader.
#[test]
fn a_dead_image_in_a_live_cohort_stops_resolving_and_its_own_pages_are_closed() {
    if !host::slab() {
        return;
    }
    let loader = Loader::new(Config {
        installed_bytes: 8 << 20,
        batch: Duration::from_secs(60),
        ..config()
    })
    .unwrap();
    // The first image's text runs past the first 2 MiB chunk and ends inside a page.
    const WIDE: usize = (2 << 20) + 4096 + 100;
    let mut wide = image(1);
    wide.text.resize(WIDE, 0);
    wide.spans[0].length = WIDE as u32;
    let (wide, small) = (wide.encode(), image(2).encode());
    for bytes in [&wide, &small] {
        loader.admit(
            Module::parse(bytes).unwrap().header.key(),
            hirsel_mod::digest(bytes),
        );
    }
    let (wide, small) = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| loader.load::<Leaf>(&wide, &IDENTITY, false).unwrap());
        while loader.installed() == 0 {
            std::thread::yield_now();
        }
        let flushed = loader.load::<Leaf>(&small, &IDENTITY, true).unwrap();
        (waiting.join().unwrap(), flushed)
    });
    let (text, page) = (wide.text(), loader.0.page);
    let shared = text + WIDE / page * page;
    assert_eq!(small.text(), text + WIDE.next_multiple_of(64));
    assert_eq!((call(&wide, 1), call(&small, 1)), (3, 4));
    let installed = loader.installed();

    // The last handle is dropped while another thread holds the loader's lock.
    let (sent, dropped) = std::sync::mpsc::channel();
    let held = loader.0.slab.lock().unwrap();
    let dropping = std::thread::spawn(move || {
        drop(wide);
        sent.send(()).unwrap();
    });
    dropped
        .recv_timeout(Duration::from_secs(10))
        .expect("the drop waited for the loader");
    drop(held);
    dropping.join().unwrap();
    loader.reclaim();

    for pc in [text, text + (2 << 20), text + WIDE - 1] {
        assert_eq!(loader.resolve(pc), None);
    }
    assert_eq!(
        loader.resolve(small.text()),
        Some(Hit {
            digest: small.digest().0[..16].try_into().unwrap(),
            offset: 0,
        })
    );
    assert_eq!(
        [text, shared - page, shared].map(host::protection),
        ["---p".to_string(), "---p".to_string(), host::LIVE.into()]
    );
    // The neighbour runs, and the dead image's bytes stay installed until the cohort goes.
    assert_eq!((call(&small, 1), loader.installed()), (4, installed));
    assert_eq!(loader.unrevoked(), 0);
    drop(small);
    assert_eq!(loader.installed(), 0);
}

/// `activate` is a compare-and-swap on the version, the retired budget refuses without
/// changing anything, and a worker keeps old code alive only while its row holds it
/// (PACK-6 steps 3 to 5).
#[test]
fn activate_swaps_at_the_expected_version_within_the_retired_budget() {
    if !host::slab() {
        return;
    }
    let loader = Loader::new(Config {
        retired_bytes: size(),
        ..config()
    })
    .unwrap();
    let code = |k| load(&loader, &image(k).encode()).unwrap();
    let slot = loader.slot::<Leaf>();
    let mut worker = loader.worker();
    // SAFETY: the law module's entry is `leaf`.
    let mut run = |slot: &Slot<Leaf>| worker.dispatch(slot, |leaf| unsafe { leaf(1) });
    assert_eq!(run(&slot), None);

    let first = code(10);
    let old = first.text();
    assert_eq!(slot.activate(0, first), Ok(1));
    assert_eq!(slot.activate(0, code(20)), Err(Stop::Stale(1)));
    assert_eq!((run(&slot), loader.retired()), (Some(12), 0));

    // The worker's row is now the only owner of the first image, which is retired.
    assert_eq!(slot.activate(1, code(20)), Ok(2));
    assert_eq!(
        (loader.retired(), host::protection(old)),
        (size(), host::LIVE.into())
    );
    // Retiring a second image would pass the budget of one.
    assert_eq!(slot.activate(2, code(30)), Err(Stop::Capacity));
    assert_eq!((slot.version(), loader.retired()), (2, size()));

    // The worker moves on, the first image dies and its cohort is recycled.
    assert_eq!(run(&slot), Some(22));
    assert_eq!(
        (loader.retired(), host::protection(old)),
        (0, "---p".into())
    );
    assert_eq!(slot.activate(2, code(30)), Ok(3));
    assert_eq!(loader.retired(), size());
    worker.park();
    assert_eq!(loader.retired(), 0);

    // Returning to code that is still installed takes it out of the retired bytes.
    let kept = code(30);
    assert_eq!(slot.activate(3, code(40)), Ok(4));
    assert_eq!(loader.retired(), size());
    assert_eq!(slot.activate(4, kept), Ok(5));
    assert_eq!((loader.retired(), loader.installed()), (0, size()));
    drop(slot);
    assert_eq!((loader.retired(), loader.installed()), (0, 0));
}

static FAULTING: std::sync::atomic::AtomicPtr<Loader> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static SAYS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// Write what the PC table gives for the faulting address, and end the process.
extern "C" fn report(_: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    // SAFETY: the law set the loader before it raised the fault.
    let loader = unsafe { &*FAULTING.load(Ordering::Relaxed) };
    let pc = host::pc(info, context);
    let mut said = [0xff; 20];
    if let Some(hit) = loader.resolve(pc) {
        said[..16].copy_from_slice(&hit.digest);
        said[16..].copy_from_slice(&hit.offset.to_le_bytes());
    }
    // SAFETY: write and _exit take integers and a live buffer.
    unsafe {
        libc::write(
            SAYS.load(Ordering::Relaxed),
            said.as_ptr().cast(),
            said.len(),
        );
        libc::_exit(0);
    }
}

/// PACK-9: a code address resolves to the image that owns it and its text offset,
/// across a chunk boundary and from the handler of a fault in loaded code (in a child
/// process, which loads the same modules); the fault
/// quarantines the digest; a recycled cohort resolves to nothing.
#[test]
fn a_code_address_resolves_to_its_image_and_a_fault_quarantines_it() {
    if !host::slab() {
        return;
    }
    let loader = Loader::new(Config {
        installed_bytes: 8 << 20,
        ..config()
    })
    .unwrap();
    // The first image's text runs past the end of the first 2 MiB chunk.
    const WIDE: usize = (2 << 20) + 4096;
    let mut wide = image(1);
    wide.text.resize(WIDE, 0);
    wide.spans[0].length = WIDE as u32;
    let wide = load(&loader, &wide.encode()).unwrap();
    // Two instructions, the second undefined.
    let (text, at): (Vec<u8>, u32) = match TARGET.arch() {
        Arch::X86_64 => (vec![0x90, 0x90, 0x0f, 0x0b], 2),
        Arch::Aarch64 => (
            [0xd503_201fu32, 0]
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect(),
            4,
        ),
    };
    let mut faulty = image(2);
    let length = text.len();
    faulty.spans[0].length = length as u32;
    (faulty.text, faulty.text_fixups, faulty.imports) = (text, vec![], vec![]);
    let bytes = faulty.encode();
    let code = load(&loader, &bytes).unwrap();

    let short = |code: &Code<Leaf>| -> [u8; 16] { code.digest().0[..16].try_into().unwrap() };
    let hit = |code: &Code<Leaf>, offset: u32| {
        Some(Hit {
            digest: short(code),
            offset,
        })
    };
    assert_eq!(loader.resolve(wide.text()), hit(&wide, 0));
    assert_eq!(
        loader.resolve(wide.text() + WIDE - 1),
        hit(&wide, WIDE as u32 - 1)
    );
    assert_eq!(loader.resolve(code.text() + at as usize), hit(&code, at));
    // The trap fill after an image belongs to no image, and neither does the stack.
    assert_eq!(loader.resolve(code.text() + length), None);
    assert_eq!(loader.resolve(wide.text() - 1), None);
    assert_eq!(loader.resolve(&loader as *const Loader as usize), None);

    if let Some(path) = std::env::var_os(CHILD) {
        // The child runs the undefined instruction; its handler writes what the table
        // gives for the faulting address into the file and ends the process.
        let file = std::fs::File::create(path).unwrap();
        FAULTING.store(&loader as *const Loader as *mut Loader, Ordering::Relaxed);
        SAYS.store(std::os::fd::IntoRawFd::into_raw_fd(file), Ordering::Relaxed);
        // SAFETY: the handler is async-signal-safe, and the entry is the law's.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = report as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO;
            libc::sigaction(libc::SIGILL, &action, std::ptr::null_mut());
            (code.entry())(1);
        }
        std::process::exit(1);
    }
    let path = std::env::temp_dir().join(format!("hirsel-code-law-{}", std::process::id()));
    let name = "tests::a_code_address_resolves_to_its_image_and_a_fault_quarantines_it";
    assert_eq!(child(name, path.as_os_str()).code(), Some(0));
    let said = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        (&said[..16], &said[16..]),
        (&short(&code)[..], &at.to_le_bytes()[..])
    );

    // The reporter quarantines by the digest the handler recorded. The image may be
    // dead and reclaimed by then, and its address another image's.
    let pc = code.text() + at as usize;
    let (digest, recorded) = (code.digest(), loader.resolve(pc).unwrap());
    drop(code);
    assert_eq!(loader.resolve(pc), Some(recorded));
    loader.reclaim();
    assert_eq!(loader.resolve(pc), None);
    assert_eq!(loader.fault(recorded), Some(digest));
    assert_eq!(load(&loader, &bytes).err(), Some(Refusal::Quarantined));
    let unknown = Hit {
        digest: [0x77; 16],
        offset: 0,
    };
    assert_eq!(loader.fault(unknown), None);
    assert_eq!(loader.resolve(wide.text() + (2 << 20)), hit(&wide, 2 << 20));
}

/// PACK-5: a loader with no slab gives the host image's prelinked code through the
/// same `Code`, slot and worker, and refuses every key the host image lacks.
#[test]
fn a_loader_without_a_slab_gives_prelinked_code_and_refuses_every_other_key() {
    let (held, other) = (image(5).encode(), image(6).encode());
    let key = |bytes: &[u8]| Module::parse(bytes).unwrap().header.key();
    let entries = vec![(IDENTITY, Leaf::DIGEST, double as *const () as usize)];
    let prelinked = vec![Prelinked {
        key: key(&held),
        digest: hirsel_mod::digest(&held),
        text: double as *const () as usize,
        entries: entries.leak(),
    }];
    let loader = Loader::new(Config {
        band_bytes: 0,
        installed_bytes: 0,
        prelinked: prelinked.leak(),
        ..config()
    })
    .unwrap();
    // The host image's entry is `double`, not the module's leaf.
    let code = loader.load::<Leaf>(&held, &IDENTITY, true).unwrap();
    assert_eq!((call(&code, 21), loader.installed()), (42, 0));
    assert_eq!(
        loader.load::<Other>(&held, &IDENTITY, true).err(),
        Some(Refusal::NoEntry)
    );
    assert_eq!(
        loader.load::<Leaf>(&other, &IDENTITY, true).err(),
        Some(Refusal::NotPrelinked)
    );
    assert_eq!(
        loader.prelinked(&key(&other)).err(),
        Some(Refusal::NotPrelinked)
    );

    let slot = loader.slot::<Leaf>();
    assert_eq!(slot.activate(0, code.clone()), Ok(1));
    let mut worker = loader.worker();
    // SAFETY: the entry is `double`.
    assert_eq!(worker.dispatch(&slot, |entry| unsafe { entry(4) }), Some(8));
    assert_eq!(loader.resolve(code.text()), None);

    loader.quarantine(code.digest());
    assert_eq!(
        loader.prelinked(&key(&held)).err(),
        Some(Refusal::Quarantined)
    );
}
