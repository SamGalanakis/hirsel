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

/// The laws share one loader, and one of them calls a recycled address,
/// so they run one at a time.
fn alone() -> std::sync::MutexGuard<'static, ()> {
    static ALONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ALONE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn loader() -> &'static Loader {
    static LOADER: OnceLock<&'static Loader> = OnceLock::new();
    LOADER.get_or_init(|| start(config(0, Mask::PURE, 16 << 20, 1 << 20)).unwrap())
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
            policy: 0,
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
    // A second pool is a pool like the first: the runtime resolves its frames too,
    // and each pool has one handle however often and through whichever clone of its
    // loader it is asked.
    let other = start(config(0, Mask::PURE, 16 << 20, 1 << 20)).unwrap();
    let module = hirsel_mod::Module::parse(&bytes).unwrap();
    other.admit(module.header.key(), hirsel_mod::digest(&bytes));
    let twin: Code<Root> = other.load(&bytes, &IDENTITY, true).unwrap();
    let mut out = Vec::new();
    assert_eq!(code::frames(&[twin.text() as u64 + 2], &mut out), 1);
    assert_eq!(out, [0, 2, low, high]);
    let handles = (loader_handle(loader()), loader_handle(other));
    assert_eq!(
        handles,
        (loader_handle(&loader().clone()), loader_handle(other))
    );
    assert!(handles.0.is_some() && handles.1.is_some() && handles.0 != handles.1);
}

/// Entry that records a cause using its own `hirsel_core_cause_owner` import slot
/// and a literal stored in its own data band (or a caller-supplied literal pointer):
/// `fn(ctx, site_ordinal, foreign_lit_ptr_or_null, foreign_lit_len)`.
#[derive(Clone, Copy)]
#[repr(transparent)]
struct RaiseEntry(unsafe extern "C" fn(*mut hirsel_core_rt::Context, u64, *const u8, u64));
// SAFETY: every L2 law module with this signature implements the C ABI above.
unsafe impl Signature for RaiseEntry {
    const DIGEST: ContentHash = ContentHash([0x2b; 32]);
}

/// Build an L2 law module whose entry:
/// 1. Loads `*hirsel_core_cause_owner` (import 0) and writes it to `[ctx + CONTEXT_SITE_OWNER]`.
/// 2. If `rdx/x2` (`foreign_lit_ptr`) is null, loads the module's own literal pointer
///    and length from `data` after the import slots; otherwise uses `(foreign_lit_ptr, foreign_lit_len)`.
/// 3. Tail-calls `hirsel_core_raise_string(ctx, site_ordinal, data_ptr, len, 0)` (import 1).
fn cause_module(seed: u64, literal: &[u8], sites: Vec<u8>) -> Vec<u8> {
    let owner_row = hirsel_mod::row("hirsel_core_cause_owner").unwrap();
    let raise_row = hirsel_mod::row("hirsel_core_raise_string").unwrap();
    let imports = vec![Import::Row(owner_row), Import::Row(raise_row)];
    // `Image::encode` prepends `8 * imports.len()` = 16 bytes of import slots.
    // Our `data` layout after the 16-byte import slots:
    //   data_offset 16..24: literal length (u64 LE)
    //   data_offset 24..24 + literal.len(): literal UTF-8 bytes
    let mut data = Vec::with_capacity(8 + literal.len());
    data.extend_from_slice(&(literal.len() as u64).to_le_bytes());
    data.extend_from_slice(literal);

    let site_owner_off = hirsel_core_rt::abi_layout::CONTEXT_SITE_OWNER as u32;
    let (text, text_fixups) = if x86() {
        // SysV x86-64: rdi=ctx, rsi=site, rdx=foreign_ptr, rcx=foreign_len
        // Call hirsel_core_raise_string(rdi=ctx, rsi=site, rdx=ptr, rcx=len, r8=0)
        let mut t = Vec::new();
        let mut fx = Vec::new();
        // mov rax, [rip + slot 0]  ; &cause_owner_slot
        t.extend_from_slice(&[0x48, 0x8b, 0x05, 0, 0, 0, 0]);
        fx.push(Fixup {
            site: 3,
            kind: Kind::Rel32Data,
            bias: 4,
            addend: 0,
        });
        // mov rax, [rax]           ; CauseData*
        t.extend_from_slice(&[0x48, 0x8b, 0x00]);
        // mov [rdi + CONTEXT_SITE_OWNER], rax  (disp32)
        t.extend_from_slice(&[0x48, 0x89, 0x87]);
        t.extend_from_slice(&site_owner_off.to_le_bytes());
        // test rdx, rdx ; jnz +14 (skip own-literal load: 7-byte lea + 7-byte mov)
        t.extend_from_slice(&[0x48, 0x85, 0xd2, 0x75, 14]);
        // lea rdx, [rip + data_offset 24]
        let lea_site = t.len() as u32 + 3;
        t.extend_from_slice(&[0x48, 0x8d, 0x15, 0, 0, 0, 0]);
        fx.push(Fixup {
            site: lea_site,
            kind: Kind::Rel32Data,
            bias: 4,
            addend: 24,
        });
        // mov rcx, [rip + data_offset 16]
        let len_site = t.len() as u32 + 3;
        t.extend_from_slice(&[0x48, 0x8b, 0x0d, 0, 0, 0, 0]);
        fx.push(Fixup {
            site: len_site,
            kind: Kind::Rel32Data,
            bias: 4,
            addend: 16,
        });
        // xor r8d, r8d             ; capacity = 0
        t.extend_from_slice(&[0x45, 0x31, 0xc0]);
        // jmp [rip + slot 1]       ; hirsel_core_raise_string
        let jmp_site = t.len() as u32 + 2;
        t.extend_from_slice(&[0xff, 0x25, 0, 0, 0, 0]);
        fx.push(Fixup {
            site: jmp_site,
            kind: Kind::Rel32Data,
            bias: 4,
            addend: 8,
        });
        (t, fx)
    } else {
        // AAPCS64: x0=ctx, x1=site, x2=foreign_ptr, x3=foreign_len
        // Call hirsel_core_raise_string(x0=ctx, x1=site, x2=ptr, x3=len, x4=0)
        assert_eq!(site_owner_off % 8, 0);
        let imm12 = (site_owner_off / 8) & 0xfff;
        let str_x9_ctx = 0xf900_0009 | (imm12 << 10); // str x9, [x0, #CONTEXT_SITE_OWNER]
        let insts: [u32; 12] = [
            0x9000_0009, //  0: adrp x9, slot 0
            0xf940_0129, //  4: ldr  x9, [x9, lo12(slot 0)]
            0xf940_0129, //  8: ldr  x9, [x9]
            str_x9_ctx,  // 12: str  x9, [x0, #CONTEXT_SITE_OWNER]
            0xb500_00a2, // 16: cbnz x2, +20 (skip 4 insts -> offset 36)
            0x9000_0002, // 20: adrp x2, data+24
            0x9100_0042, // 24: add  x2, x2, lo12(data+24)
            0x9000_0003, // 28: adrp x3, data+16
            0xf940_0063, // 32: ldr  x3, [x3, lo12(data+16)]
            0xd280_0004, // 36: mov  x4, #0
            0x9000_0010, // 40: adrp x16, slot 1
            0xf940_0210, // 44: ldr  x16, [x16, lo12(slot 1)]
        ];
        let mut all = insts.to_vec();
        all.push(0xd61f_0200); // 48: br x16
        let fx = vec![
            Fixup {
                site: 0,
                kind: Kind::PageData,
                bias: 0,
                addend: 0,
            },
            Fixup {
                site: 4,
                kind: Kind::Low12LoadData,
                bias: 0,
                addend: 0,
            },
            Fixup {
                site: 20,
                kind: Kind::PageData,
                bias: 0,
                addend: 24,
            },
            Fixup {
                site: 24,
                kind: Kind::Low12AddData,
                bias: 0,
                addend: 24,
            },
            Fixup {
                site: 28,
                kind: Kind::PageData,
                bias: 0,
                addend: 16,
            },
            Fixup {
                site: 32,
                kind: Kind::Low12LoadData,
                bias: 0,
                addend: 16,
            },
            Fixup {
                site: 40,
                kind: Kind::PageData,
                bias: 0,
                addend: 8,
            },
            Fixup {
                site: 44,
                kind: Kind::Low12LoadData,
                bias: 0,
                addend: 8,
            },
        ];
        (words(&all), fx)
    };

    let mut coreir = [0u8; 32];
    coreir[..8].copy_from_slice(&seed.to_le_bytes());
    hirsel_mod::Image {
        header: Header {
            target: hirsel_code::TARGET,
            backend: Backend::Cranelift,
            policy: 0,
            abi: hirsel_mod::abi(),
            compiler: ContentHash([2; 32]),
            coreir: ContentHash(coreir),
            text_align: 64,
            data_align: 64,
        },
        spans: vec![Span {
            start: 0,
            length: text.len() as u32,
        }],
        names: vec!["raise_entry".into()],
        text,
        data,
        imports,
        text_fixups,
        data_fixups: vec![],
        cfi: vec![],
        sites,
        entries: vec![Entry {
            identity: IDENTITY,
            signature: RaiseEntry::DIGEST,
            text: 0,
            descriptor: NO_DESCRIPTOR,
            waits: false,
        }],
    }
    .encode()
}

/// Shared host `core`-style helper that inherits its caller's `(site, site_owner)`.
/// Even if an unrelated active slot changes before it raises, it must restore the
/// captured `caller_site_owner` and never infer from the active slot or ordinal.
unsafe fn shared_core_inheriting_helper(
    ctx: *mut hirsel_core_rt::Context,
    caller_site: u64,
    between: impl FnOnce(),
) {
    // SAFETY: mirrors Cranelift/LLVM inheriting function entry capture + restore.
    unsafe {
        let saved_owner = (*ctx).site_owner;
        between();
        (*ctx).site_owner = saved_owner;
        hirsel_core_rt::abi::hirsel_core_raise(
            ctx,
            hirsel_core_rt::causes::Cause::new(hirsel_core_rt::causes::BOUNDS, caller_site),
        );
    }
}

/// PACK-9.7 (L2): one cause chain containing causes from two releases with the
/// same site ordinal, a foreign literal at the other release's site, a full long
/// literal (FAIL-10 zero truncation), a moved heap string, a shared-core inherited
/// site, and a failing finalizer. Exhausts the payer before recording, unloads and
/// reclaims 100% of RX while the chain survives, reuses the addresses, then formats
/// and drops the chain verifying exact bytes, ordinals, order, and payer refund.
#[test]
fn l2_owned_cause_metadata_survives_full_rx_reclaim_and_address_reuse() {
    use hirsel_core_layout::source_positions;
    use hirsel_core_rt::{
        Context,
        abi::*,
        abi_sites,
        block::Block,
        causes::{self, Cause, Causes},
    };

    let _alone = alone();
    let loader = loader();
    let _ = loader.reclaim();
    assert_eq!(loader.installed(), 0);

    // Release A and Release B share the EXACT SAME site ordinal `SAME_ORDINAL = 15`,
    // mapping to completely different source files and line:column positions.
    const SAME_ORDINAL: u64 = 15;
    const FIN_ORDINAL: u64 = 12;
    let src_a = "fn alpha()\n  fail(\"long\")\n"; // byte 14 (site 1+14=15) => line 2, col 4
    let src_b_main = "a\nb\nc\nd\ne\nf\n  fail(msg)\n"; // byte 14 (site 1+14=15) => line 7, col 3
    let src_b_fin = "fn fin()\n  1/0\n"; // starts after src_b_main (1 + 25 = 26); wait: put fin at start 1? No, use byte 11 => line 2, col 3!
    let enc_a = source_positions::encode(src_a);
    let enc_b = source_positions::encode(src_b_main);
    let enc_b_fin = source_positions::encode(src_b_fin);
    let sites_a = source_positions::encode_source_table(&[("alpha/mod.hirsel", 1, &enc_a)]);
    let fin_start = 1 + src_b_main.len() as u64 + 1;
    let fin_site = fin_start + FIN_ORDINAL - 1;
    let sites_b = source_positions::encode_source_table(&[
        ("beta/service.hirsel", 1, &enc_b),
        ("beta/finalizer.hirsel", fin_start, &enc_b_fin),
    ]);

    // Full >8 KiB literal in Release A to prove FAIL-10 zero truncation.
    let long_lit_a: String = (0..300)
        .map(|i| format!("[A-chunk-{i:03}:0123456789abcdef]"))
        .collect();
    assert!(long_lit_a.len() > 8192);
    let lit_b = "release-b-own-literal";

    let mod_a = cause_module(101, long_lit_a.as_bytes(), sites_a);
    let mod_b = cause_module(102, lit_b.as_bytes(), sites_b);
    let code_a = load::<RaiseEntry>(&mod_a).unwrap();
    let code_b = load::<RaiseEntry>(&mod_b).unwrap();
    let text_a = code_a.text();
    let data_a_lit_ptr = (code_a.data() + 24) as *const u8;
    let owner_a_ptr = code_a.cause_owner().unwrap() as *const causes::CauseData;
    let owner_b_ptr = code_b.cause_owner().unwrap() as *const causes::CauseData;
    assert_ne!(owner_a_ptr, owner_b_ptr);
    assert!(loader.installed() > 0);

    // Put Release A in a slot so we can swap the active slot to Release B mid-flight
    // (proving inherited sites never infer from the currently active slot).
    let slot = loader.slot::<RaiseEntry>();
    assert_eq!(slot.activate(0, code_a.clone()), Ok(1));

    // Set up a task context with 5 prepaid cells (1 primary + 4 resources whose
    // finalizer cells let us build one multi-cause chain cleanly under zero payer room).
    let mut ctx = Context::new(8192);
    causes::task_start(&mut ctx).unwrap();
    let after_task_start = ctx.available();

    // Allocate a moved heap String BEFORE exhausting the payer.
    let heap_msg = b"moved-heap-string-survives-unload-and-refunds";
    let heap_cap = 64u64;
    let heap_block = Block::new(&ctx, Some(heap_cap), heap_cap as usize, 8).unwrap();
    let heap_ptr = heap_block.into_raw().as_ptr();
    // SAFETY: `heap_ptr` is an exclusively owned 64-byte buffer.
    unsafe {
        std::ptr::copy_nonoverlapping(heap_msg.as_ptr(), heap_ptr, heap_msg.len());
    }

    // Reserve 4 resource cells (for causes 2..=5 in the single chain).
    let resources: Vec<_> = (0..4)
        .map(|_| unsafe { causes::resource_new(&ctx, 8, 0, 8, 8) }.unwrap())
        .collect();

    // EXHAUST the payer to 0 available bytes BEFORE recording ANY cause!
    let leftover = ctx.available();
    let drain = Block::new(&ctx, Some(leftover), leftover as usize, 8).unwrap();
    assert_eq!(ctx.available(), 0);
    assert!(Block::new(&ctx, Some(8), 8, 8).is_err());

    // 1. Primary cause: Release A raises its full >8 KiB literal at SAME_ORDINAL.
    code_a.with_entry(|f| unsafe { (f.0)(&mut ctx, SAME_ORDINAL, std::ptr::null(), 0) });

    // 2. Foreign literal at the other release's site: Release B raises Release A's
    //    literal (pointing into Release A's data band while A is pinned) at SAME_ORDINAL!
    unsafe {
        causes::finalizer_begin(&mut ctx, resources[0]);
        code_b.with_entry(|f| {
            (f.0)(
                &mut ctx,
                SAME_ORDINAL,
                data_a_lit_ptr,
                long_lit_a.len() as u64,
            )
        });
        causes::finalizer_end(&mut ctx, resources[0]);
        causes::resource_free(resources[0], 8);
    }

    // 3. Shared host `core`-style helper inheriting Release A's site while an active
    //    slot concurrently swaps from Release A to Release B and clobbers ctx.site_owner!
    unsafe {
        causes::finalizer_begin(&mut ctx, resources[1]);
        ctx.site_owner = owner_a_ptr;
        shared_core_inheriting_helper(&mut ctx, SAME_ORDINAL, || {
            assert_eq!(slot.activate(1, code_b.clone()), Ok(2));
            ctx.site_owner = owner_b_ptr; // Simulate an inner call touching Release B.
        });
        causes::finalizer_end(&mut ctx, resources[1]);
        causes::resource_free(resources[1], 8);
    }

    // 4. Moved heap string raised at Release B's site (capacity > 0 -> moved buffer).
    unsafe {
        causes::finalizer_begin(&mut ctx, resources[2]);
        ctx.site_owner = owner_b_ptr;
        hirsel_core_raise_string(
            &mut ctx,
            SAME_ORDINAL,
            heap_ptr,
            heap_msg.len() as u64,
            heap_cap,
        );
        causes::finalizer_end(&mut ctx, resources[2]);
        causes::resource_free(resources[2], 8);
    }

    // 5. Failing finalizer from Release B at `fin_site`.
    unsafe {
        causes::finalizer_begin(&mut ctx, resources[3]);
        ctx.site_owner = owner_b_ptr;
        hirsel_core_raise(&mut ctx, Cause::new(causes::DIVISION, fin_site));
        causes::finalizer_end(&mut ctx, resources[3]);
        causes::resource_free(resources[3], 8);
    }

    // Take the chain out (simulating splice/catch/join delivery) and drop `drain`.
    // SAFETY: `ctx` owns this complete chain.
    let raw_chain = unsafe { hirsel_core_causes_take(&mut ctx) };
    drop(drain);
    // While the 5-cause chain survives, 4 extra resource cells (4 * 32) + 64-byte heap
    // string + the primary cell (already charged in `after_task_start`) remain charged.
    assert_eq!(ctx.available(), after_task_start - 4 * 32 - heap_cap);

    // UNLOAD & RECLAIM 100% of RX while the cause chain survives!
    drop(slot);
    drop(code_a);
    drop(code_b);
    loader.reclaim().unwrap();
    assert_eq!(
        loader.installed(),
        0,
        "100% of RX must reclaim while causes survive"
    );
    assert_eq!(loader.retired(), 0);

    // REUSE the dead text & data addresses with fresh modules!
    let mut reused_text = false;
    let mut churn_kept = Vec::new();
    for k in 200..280u64 {
        let replacement = cause_module(k, b"OVERWRITTEN-REUSED-DATA-BYTES", vec![]);
        let handle = load::<RaiseEntry>(&replacement).unwrap();
        if handle.text() == text_a {
            reused_text = true;
        }
        churn_kept.push(handle);
        if reused_text {
            break;
        }
    }
    assert!(
        reused_text,
        "recycled text range must be reused before formatting"
    );
    drop(churn_kept);
    loader.reclaim().unwrap();
    assert_eq!(loader.installed(), 0);

    // Now format and verify EVERY cause in the surviving chain!
    // SAFETY: `raw_chain` is exclusively owned.
    let chain = unsafe { Causes::from_raw(raw_chain) };
    let cells: Vec<&causes::Cell> = chain.iter_cells().collect();
    assert_eq!(cells.len(), 5);

    // Exact unchanged release-local site ordinals (zero address, owner tag, or prefix).
    let ordinals: Vec<u64> = cells.iter().map(|c| c.cause().site).collect();
    assert_eq!(
        ordinals,
        vec![
            SAME_ORDINAL,
            SAME_ORDINAL,
            SAME_ORDINAL,
            SAME_ORDINAL,
            fin_site
        ]
    );

    // Cell 1 (foreign literal at B's site) must be a tagged pair (B, A); cell 0 single A.
    assert_eq!(cells[0].owner_word() & 1, 0);
    assert_eq!(cells[1].owner_word() & 1, 1);

    // Exact formatted diagnostics against source fixtures!
    let formatted: Vec<String> = cells
        .iter()
        .map(|cell| unsafe { abi_sites::format_cell(cell, std::ptr::null()) })
        .collect();
    assert_eq!(
        formatted[0],
        format!("Fail({long_lit_a:?}) at alpha/mod.hirsel:2:4")
    );
    assert_eq!(
        formatted[1],
        format!("Fail({long_lit_a:?}) at beta/service.hirsel:7:3")
    );
    assert_eq!(formatted[2], "Bounds at alpha/mod.hirsel:2:4");
    assert_eq!(
        formatted[3],
        "Fail(\"moved-heap-string-survives-unload-and-refunds\") at beta/service.hirsel:7:3"
    );
    assert_eq!(formatted[4], "Division at beta/finalizer.hirsel:2:3");

    // Detached message bytes copy owns its backing and survives chain drop.
    let detached = cells[1].detached_message_bytes().unwrap();
    assert_eq!(detached, long_lit_a.as_bytes());

    // Drop the chain -> releases CauseData owners + moved heap string + all 5 cells!
    drop(chain);
    assert_eq!(detached, long_lit_a.as_bytes());
    // All 5 cells (including primary cell's 32 bytes) + 64-byte moved heap string refunded!
    assert_eq!(
        ctx.available(),
        after_task_start + 32,
        "full payer refund after dropping cause chain"
    );
    drop(ctx);
    assert_eq!(loader.installed(), 0);
}

/// PACK-9.7 (L2): 10^5 cause-surviving unload/reclaim/reuse/format/drop rounds
/// with zero RX retention and bounded resident memory.
#[test]
fn l2_cause_surviving_reuse_churn_100k() {
    use hirsel_core_layout::source_positions;
    use hirsel_core_rt::{
        Context,
        abi::hirsel_core_causes_take,
        abi_sites,
        causes::{self, Causes},
    };

    let _alone = alone();
    let loader = loader();
    let _ = loader.reclaim();
    assert_eq!(loader.installed(), 0);

    const ROUNDS: usize = 100_000;
    // Build a small ring of distinct modules with distinct source paths & literals
    // so every round unloads, reclaims 100% RX, and recycles slab cohorts.
    let templates: Vec<(Vec<u8>, String)> = (0..4usize)
        .map(|i| {
            let path = format!("churn/r{i}.hirsel");
            let lit = format!("churn-literal-{i}-payload");
            let enc = source_positions::encode("header\n  fail(x)\n");
            let sites = source_positions::encode_source_table(&[(&path, 1, &enc)]);
            let bytes = cause_module(10_000 + i as u64, lit.as_bytes(), sites);
            let parsed = hirsel_mod::Module::parse(&bytes).unwrap();
            loader.admit(parsed.header.key(), hirsel_mod::digest(&bytes));
            let expected = format!("Fail({lit:?}) at {path}:2:3");
            (bytes, expected)
        })
        .collect();

    let mut ctx = Context::new(4096);
    causes::task_start(&mut ctx).unwrap();
    let baseline_avail = ctx.available();

    // Warm up a few rounds before sampling RSS baseline (resident KiB via host seam).
    let mut rss_after_warmup = 0u64;
    let read_rss_kib = hirsel_core_rt::host::resident_kib;

    let mut worker = loader.worker();
    let mut first_cause = None;
    let mut first_text = 0usize;
    let mut reused_count = 0usize;
    for round in 0..=ROUNDS {
        let (bytes, expected) = &templates[round % templates.len()];
        let image = loader.image(bytes, true).unwrap();
        let code = image.code::<RaiseEntry>(&IDENTITY).unwrap();
        let entry = image.resolve::<RaiseEntry>(&IDENTITY).unwrap();
        let registered = code.register([entry.candidate()]).unwrap();
        let slot = PlaceSlot::new(loader).unwrap();
        let place = slot.activate(0, |_| Ok::<_, ()>((registered, ()))).unwrap();
        if round == 0 {
            first_text = code.text();
        } else if code.text() == first_text {
            reused_count += 1;
        }
        // Record a cause using `isolate` so the task keeps its primary cell across all 10^5 rounds.
        let entered = unsafe { hirsel_core_rt::isolate::hirsel_core_isolate_enter(&mut ctx) };
        assert!(entered.entered);
        place
            .dispatch(&mut worker, 0, |f: RaiseEntry, _, _| {
                // SAFETY: the checkout pins the test module's cause entry for this call.
                unsafe { (f.0)(&mut ctx, 10, std::ptr::null(), 0) };
            })
            .unwrap()
            .unwrap();
        let caught = unsafe { hirsel_core_rt::isolate::hirsel_core_isolate_catch(&mut ctx) };
        assert_eq!(caught.status, 0);

        drop((place, slot, code, image));
        worker.park();
        loader.reclaim().unwrap();
        assert_eq!(loader.installed(), 0);

        // Format and drop the surviving cause after RX is gone.
        // SAFETY: `caught.causes` is exclusively owned.
        let chain = unsafe { Causes::from_raw(caught.causes) };
        let cell = chain.iter_cells().next().unwrap();
        assert_eq!(cell.cause().site, 10);
        assert_eq!(
            unsafe { abi_sites::format_cell(cell, std::ptr::null()) },
            *expected
        );
        if round == 0 {
            first_cause = Some(chain);
        }

        if round == 500 {
            rss_after_warmup = read_rss_kib();
        }
    }
    let first = first_cause.unwrap();
    let cell = first.iter_cells().next().unwrap();
    // SAFETY: this retained chain owns the original literal and site table.
    assert_eq!(
        unsafe { abi_sites::format_cell(cell, std::ptr::null()) },
        templates[0].1
    );
    drop(first);
    let rss_end = read_rss_kib();
    let raw_left = unsafe { hirsel_core_causes_take(&mut ctx) };
    assert!(raw_left.is_null());
    assert_eq!(ctx.available(), baseline_avail);
    drop(ctx);
    assert_eq!(loader.installed(), 0);
    assert!(
        reused_count > 0,
        "slab text addresses must be recycled across 10^5 rounds"
    );
    // Ensure no unbounded memory leak across 10^5 rounds (< 32 MiB RSS growth after warmup).
    assert!(
        rss_end.saturating_sub(rss_after_warmup) < 32 * 1024,
        "RSS grew by {} KiB across {ROUNDS} rounds (warmup={rss_after_warmup}, end={rss_end})",
        rss_end.saturating_sub(rss_after_warmup)
    );
}

#[test]
fn ended_place_refuses_registration_and_fresh_calls() {
    let _alone = alone();
    let loader = loader();
    let bytes = root(61, "place", sound());
    loader.admit_module(&bytes).unwrap();
    let image = loader.image(&bytes, true).unwrap();
    let code = image.code::<Root>(&IDENTITY).unwrap();
    let entry = image.resolve::<Root>(&IDENTITY).unwrap();
    let registered = code.register([entry.candidate()]).unwrap();
    let slot = PlaceSlot::new(loader).unwrap();
    let first = slot
        .activate(0, |_| Ok::<_, ()>((registered.clone(), ())))
        .unwrap();
    let attempted = slot.activate(first.version(), |execution| {
        execution.record_stop_bytes(b"RegistrationStop");
        Ok::<_, ()>((registered.clone(), ()))
    });
    assert!(
        matches!(attempted, Err(PlaceError::Ended(EndReason::RunStop(stop))) if stop.as_bytes() == b"RegistrationStop")
    );

    let mut worker = loader.worker();
    let outcome = first.dispatch(&mut worker, 0, |root: Root, exec, _| run(exec, root.0));
    assert_eq!(outcome.unwrap(), Some(Ok(0)));
    first.execution().request_host_stop();
    let later: Result<Option<()>, _> = first.dispatch(&mut worker, 0, |_: Root, _, _| {
        panic!("entered a stopped Place")
    });
    assert_eq!(later, Err(DispatchError::Ended(EndReason::HostStop)));
    let second = slot
        .activate(first.version(), |_| Ok::<_, ()>((registered, ())))
        .unwrap();
    assert_eq!(
        second
            .dispatch(&mut worker, 0, |root: Root, exec, _| run(exec, root.0))
            .unwrap(),
        Some(Ok(0))
    );
}
