//! The join's laws. Each expected offset is the hand-assembled instruction's place in
//! its text; each digest is `hirsel_mod::digest` of the module's bytes.

use std::sync::OnceLock;

use hirsel_code::{Code, Loader, Refusal, Signature};
use hirsel_core_ids::ContentHash;
use hirsel_core_rt::{
    process,
    run_stop::{EndReason, Execution, run_contained_worker},
    tasks::SchedulerProvider,
};
use hirsel_mod::{Backend, Entry, Fixup, Header, Import, Kind, Mask, NO_DESCRIPTOR, Span};

use super::*;

/// A root entry as `run_contained_worker` calls it.
#[derive(Clone, Copy)]
#[repr(transparent)]
struct Root(process::Entry);
// SAFETY: every law module whose entry has this digest is a root entry.
unsafe impl Signature for Root {
    const DIGEST: ContentHash = ContentHash([7; 32]);
}

/// `memset`'s C type.
#[derive(Clone, Copy)]
#[repr(transparent)]
struct Fill(unsafe extern "C" fn(*mut u8, i32, usize) -> *mut u8);
// SAFETY: the one law module with this digest jumps to `memset`.
unsafe impl Signature for Fill {
    const DIGEST: ContentHash = ContentHash([6; 32]);
}

const IDENTITY: ContentHash = ContentHash([0xaa; 32]);

/// The laws share the process's one loader, and one of them calls a recycled address,
/// so they run one at a time.
fn alone() -> std::sync::MutexGuard<'static, ()> {
    static ALONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ALONE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn loader() -> &'static Loader {
    static LOADER: OnceLock<&'static Loader> = OnceLock::new();
    LOADER.get_or_init(|| start(config(Mask::PURE, 16 << 20, 1 << 20)).unwrap())
}

fn x86() -> bool {
    match hirsel_code::TARGET.arch() {
        hirsel_core_layout::Arch::X86_64 => true,
        hirsel_core_layout::Arch::Aarch64 => false,
    }
}

fn words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// A root entry that returns 0.
fn sound() -> Vec<u8> {
    if x86() {
        vec![0x31, 0xc0, 0xc3] // xor eax, eax; ret
    } else {
        words(&[0x5280_0000, 0xd65f_03c0]) // mov w0, #0; ret
    }
}

/// A root entry that loads from address zero, and the offset of that load.
fn wild() -> (Vec<u8>, u32) {
    if x86() {
        // 0: xor eax, eax; 2: mov eax, [rax]; 4: ret
        (vec![0x31, 0xc0, 0x8b, 0x00, 0xc3], 2)
    } else {
        // 0: mov x0, #0; 4: ldr x0, [x0]; 8: ret
        (words(&[0xd280_0000, 0xf940_0000, 0xd65f_03c0]), 4)
    }
}

/// A root entry that reaches a trap, and the trap's offset.
fn trap() -> (Vec<u8>, u32) {
    if x86() {
        (vec![0x90, 0xcc, 0xc3], 1) // 0: nop; 1: int3; 2: ret
    } else {
        // 0: nop; 4: brk #0; 8: ret
        (words(&[0xd503_201f, 0xd420_0000, 0xd65f_03c0]), 4)
    }
}

/// A jump to import 0.
fn jump() -> (Vec<u8>, Vec<Fixup>) {
    let fixup = |site, kind, bias| Fixup {
        site,
        kind,
        bias,
        addend: 0,
    };
    if x86() {
        // jmp [rip + slot 0]
        (
            vec![0xff, 0x25, 0, 0, 0, 0],
            vec![fixup(2, Kind::Rel32Data, 4)],
        )
    } else {
        // adrp x16, slot 0; ldr x16, [x16, low 12]; br x16
        (
            words(&[0x9000_0010, 0xf940_0210, 0xd61f_0200]),
            vec![
                fixup(0, Kind::PageData, 0),
                fixup(4, Kind::Low12LoadData, 0),
            ],
        )
    }
}

/// A module of one span named `name` with one entry at its start. `seed` makes its
/// digest its own.
fn module(
    seed: u8,
    name: &str,
    signature: ContentHash,
    text: Vec<u8>,
    imports: Vec<Import>,
    text_fixups: Vec<Fixup>,
) -> Vec<u8> {
    hirsel_mod::Image {
        header: Header {
            target: hirsel_code::TARGET,
            backend: Backend::Cranelift,
            policy: hirsel_mod::POLICY,
            abi: hirsel_mod::abi(),
            compiler: ContentHash([2; 32]),
            coreir: ContentHash([seed; 32]),
            text_align: 64,
            data_align: 64,
        },
        spans: vec![Span {
            start: 0,
            length: text.len() as u32,
        }],
        names: vec![name.into()],
        text,
        data: vec![],
        imports,
        text_fixups,
        data_fixups: vec![],
        cfi: vec![],
        sites: vec![],
        entries: vec![Entry {
            identity: IDENTITY,
            signature,
            text: 0,
            descriptor: NO_DESCRIPTOR,
            waits: false,
        }],
    }
    .encode()
}

fn root(seed: u8, name: &str, text: Vec<u8>) -> Vec<u8> {
    module(seed, name, Root::DIGEST, text, vec![], vec![])
}

fn load<F: Signature>(bytes: &[u8]) -> Result<Code<F>, Refusal> {
    let module = hirsel_mod::Module::parse(bytes).unwrap();
    loader().admit(module.header.key(), hirsel_mod::digest(bytes));
    loader().load(bytes, &IDENTITY, true)
}

unsafe extern "C" fn silent(_: *mut u8, _: *const u8, _: usize) {}

/// Run `entry` as the root of a contained execution on its own worker.
fn run(execution: &Execution, entry: process::Entry) -> Result<u8, EndReason> {
    let config = process::Config {
        launch: std::ptr::null(),
        describe_output: None,
        root_limit: 256 << 10,
        args: std::ptr::null(),
        args_len: 0,
        grants: std::ptr::null(),
        grants_len: 0,
        provider: std::ptr::null(),
        state: std::ptr::null_mut(),
        diagnostic: silent,
        describe: None,
        scheduler: SchedulerProvider::default(),
        stop_fd: 2,
        capabilities: std::ptr::null(),
        capabilities_len: 0,
        close_grants: None,
    };
    // SAFETY: `config` and the entry's image outlive the call.
    unsafe { run_contained_worker(execution, &config, entry) }
}

fn stopped(result: Result<u8, EndReason>) -> Vec<u8> {
    match result {
        Err(EndReason::RunStop(stop)) => stop.as_bytes().to_vec(),
        other => panic!("the execution did not stop: {other:?}"),
    }
}

/// PACK-9 item 6 for a hardware fault and for a trap: the pool stops with `Code`, the
/// report names the module, the text offset and the span, the digest is quarantined,
/// and the process goes on to run other loaded code.
#[test]
fn a_fault_in_loaded_code_stops_its_execution_names_its_image_and_quarantines_it() {
    let _alone = alone();
    for (seed, name, (text, at)) in [(1, "wild", wild()), (2, "trap", trap())] {
        let bytes = root(seed, name, text);
        let digest = hirsel_mod::digest(&bytes);
        let code = load::<Root>(&bytes).unwrap();
        let execution = Execution::contained();
        assert_eq!(stopped(run(&execution, code.entry().0)), b"Code");
        // The report may come after the image has died and been reclaimed.
        let pc = code.text() + at as usize;
        drop(code);
        loader().reclaim();
        let report = report(&execution).unwrap();
        assert_eq!(report.pc, pc);
        assert_eq!(report.hit, Some((digest.0[..16].try_into().unwrap(), at)));
        assert_eq!(report.quarantined, Some(digest));
        assert_eq!(
            report.span(&hirsel_mod::Module::parse(&bytes).unwrap()),
            Some(name)
        );
        assert!(matches!(load::<Root>(&bytes), Err(Refusal::Quarantined)));
    }
    let code = load::<Root>(&root(3, "sound", sound())).unwrap();
    let execution = Execution::contained();
    assert_eq!(run(&execution, code.entry().0), Ok(0));
    assert_eq!(report(&execution), None);
}

/// A call through an address kept past its image's death never runs code: it stops the
/// execution with `Code`, and the report names no image and quarantines none.
#[test]
fn a_stale_code_address_stops_its_execution_and_names_no_image() {
    let _alone = alone();
    let bytes = root(4, "stale", sound());
    let code = load::<Root>(&bytes).unwrap();
    let (entry, text) = (code.entry().0, code.text());
    drop(code);
    loader().reclaim();
    let execution = Execution::contained();
    assert_eq!(stopped(run(&execution, entry)), b"Code");
    // Images loaded before the report may take the dead one's address. None of them
    // faulted, so none is quarantined.
    let later: Vec<Vec<u8>> = (0..4).map(|n| root(8 + n, "later", sound())).collect();
    let held: Vec<Code<Root>> = later.iter().map(|bytes| load(bytes).unwrap()).collect();
    assert_eq!(
        report(&execution),
        Some(Report {
            pc: text,
            hit: None,
            quarantined: None
        })
    );
    drop(held);
    assert!(later.iter().all(|bytes| load::<Root>(bytes).is_ok()));
    // The module itself did nothing wrong, so it still loads.
    assert!(load::<Root>(&bytes).is_ok());
}

/// A row import binds to the runtime symbol its row names: loaded code that jumps to
/// the `memset` row fills the bytes it is given. A row this image lacks has no address.
#[test]
fn a_row_import_binds_to_the_named_symbol_of_this_image() {
    let _alone = alone();
    let (text, fixups) = jump();
    let row = hirsel_mod::row("memset").unwrap();
    let bytes = module(
        5,
        "fill",
        Fill::DIGEST,
        text,
        vec![Import::Row(row)],
        fixups,
    );
    let code = load::<Fill>(&bytes).unwrap();
    let mut filled = [0u8; 5];
    // SAFETY: the module's text is a jump to `memset`, and the range is `filled`.
    unsafe { (code.entry().0)(filled.as_mut_ptr(), 0x5a, 4) };
    assert_eq!(filled, [0x5a, 0x5a, 0x5a, 0x5a, 0]);
    let absent = hirsel_mod::row("hirsel_core_law_witness").unwrap();
    assert_eq!(rows::address(absent), None);
}

/// PACK-9 item 5: of a sampled stack, each address in a live image gives its index,
/// text offset and digest prefix, and an address outside any image gives nothing.
#[test]
fn a_sampled_stack_resolves_its_loaded_code_frames() {
    let _alone = alone();
    let bytes = root(6, "sampled", sound());
    let digest = hirsel_mod::digest(&bytes).0;
    let code = load::<Root>(&bytes).unwrap();
    let outside = a_sampled_stack_resolves_its_loaded_code_frames as *const () as u64;
    let pcs = [outside, code.text() as u64 + 2, 0, code.text() as u64];
    let mut out = Vec::new();
    assert_eq!(code::frames(&pcs, &mut out), 2);
    let low = u64::from_le_bytes(digest[..8].try_into().unwrap());
    let high = u64::from_le_bytes(digest[8..16].try_into().unwrap());
    assert_eq!(out, [1, 2, low, high, 3, 0, low, high]);
}
