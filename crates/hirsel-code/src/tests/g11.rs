//! Whole-slab retirement, same-address reuse, and synchronization failure.
use super::*;
#[path = "../host/g11_laws.rs"]
mod platform;

static EXPECTED_SITE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn retired_fault(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let pc = host::pc(signal, info, context);
    // SAFETY: the child retains the loader throughout execution of this handler.
    unsafe {
        let loader = &*FAULTING.load(Ordering::Acquire);
        if pc != EXPECTED_SITE.load(Ordering::Acquire) || loader.resolve(pc).is_some() {
            libc::_exit(2);
        }
        // SA_RESETHAND restores default disposition. Deliver the original signal after return.
        libc::raise(signal);
    }
}

/// PACK-4 step 5 and PACK-9: when an image dies in a cohort that lives on, its address
/// stops resolving in every chunk it spanned and its whole extent traps, while a
/// batch neighbour on its last page still runs and resolves. Dropping
/// the last handle does not wait for the loader.
#[test]
fn retired_text_traps_while_its_shared_page_neighbor_runs() {
    if !host::slab() {
        return;
    }
    let loader = loader_for_law(Config {
        installed_bytes: 8 << 20,
        batch: Duration::from_secs(60),
        ..config()
    })
    .unwrap();
    // The first image's text runs past the first 2 MiB chunk and ends inside a page.
    const SHARED_ENTRY: usize = (2 << 20) + 4096;
    const WIDE: usize = SHARED_ENTRY
        + match TARGET.arch() {
            Arch::X86_64 => 128,  // The last INT3 borders the live image.
            Arch::Aarch64 => 127, // The final UDF owns one byte of private padding.
        };
    let mut wide = image(1);
    // The callable entry itself sits on the shared page. Keep a PC row spanning
    // the chunk boundary so the same retirement also exercises multi-chunk removal.
    let body = std::mem::take(&mut wide.text);
    wide.text.resize(WIDE, 0);
    wide.text[SHARED_ENTRY..SHARED_ENTRY + body.len()].copy_from_slice(&body);
    for fixup in &mut wide.text_fixups {
        fixup.site += SHARED_ENTRY as u32;
    }
    wide.entries[0].text = SHARED_ENTRY as u32;
    wide.spans = vec![
        Span {
            start: 0,
            length: SHARED_ENTRY as u32,
        },
        Span {
            start: SHARED_ENTRY as u32,
            length: (WIDE - SHARED_ENTRY) as u32,
        },
    ];
    wide.names = vec!["lead".into(), "leaf".into()];
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
    assert_eq!(wide.entry() as usize / page, small.text() / page);
    assert_eq!((call(&wide, 1), call(&small, 1)), (3, 4));
    let installed = loader.installed();
    let neighbor_bytes = || {
        // SAFETY: small holds this published Rx extent alive.
        unsafe { std::slice::from_raw_parts(small.text() as *const u8, leaf().0.len()).to_vec() }
    };
    let original = neighbor_bytes();

    let stop = Arc::new(AtomicBool::new(false));
    let neighbor = small.clone();
    let running = stop.clone();
    let (started, ready) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let check = || {
            assert_eq!(call(&neighbor, 1), 4);
            let table = &neighbor.image.shared.table;
            assert_eq!(
                table.find(neighbor.text(), 1, |row, _| row.digest).unwrap(),
                neighbor.digest().0[..16]
            );
        };
        check();
        started.send(()).unwrap();
        while !running.load(Ordering::Acquire) {
            check();
        }
    });
    ready.recv().unwrap();
    // The remote worker keeps its count through a suspended call scope.
    let (resume, parked) = std::sync::mpsc::channel();
    let (warmed, active) = std::sync::mpsc::channel();
    let (last, released) = std::sync::mpsc::channel();
    let (jump, reclaimed) = std::sync::mpsc::channel();
    let stale = std::env::var_os(CHILD);
    if stale.is_some() {
        FAULTING.store(std::ptr::from_ref(&loader).cast_mut(), Ordering::Release);
        // SAFETY: only the isolated law child changes its trap disposition.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = retired_fault as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_RESETHAND;
            assert_eq!(
                libc::sigaction(host::FAULT, &action, std::ptr::null_mut()),
                0
            );
        }
    }
    let executor = std::thread::spawn(move || {
        let entry = wide.entry();
        assert_eq!(call(&wide, 1), 3);
        warmed.send(()).unwrap();
        parked.recv().unwrap();
        assert_eq!(call(&wide, 2), 5);
        drop(wide);
        last.send(()).unwrap();
        reclaimed.recv().unwrap();
        if let Some(role) = stale {
            let site = if role == "entry" {
                entry as usize
            } else {
                text + WIDE.next_multiple_of(64)
                    - match TARGET.arch() {
                        Arch::X86_64 => 1,
                        Arch::Aarch64 => 4,
                    }
            };
            EXPECTED_SITE.store(site, Ordering::Release);
            // SAFETY: confined to the law child. Its retired RX site must raise the ISA trap.
            Loader::on_executor(|| unsafe { std::mem::transmute::<usize, Leaf>(site)(1) });
            std::process::exit(1);
        }
    });
    active.recv().unwrap();
    loader.reclaim().unwrap();
    assert!(loader.resolve(text).is_some());
    let held = loader.0.slab.lock().unwrap();
    resume.send(()).unwrap();
    released
        .recv_timeout(Duration::from_secs(10))
        .expect("last drop waited for the slab");
    // Pinning the image of a code address does not wait for the slab either, and the
    // dead image, whose row the loader has not removed yet, pins nothing.
    let (pinned, pin) = std::sync::mpsc::channel();
    let (pool, live, dead) = (loader.clone(), small.text(), text + SHARED_ENTRY);
    std::thread::spawn(move || {
        pinned.send((pool.pin_pc(live).is_some(), pool.pin_pc(dead).is_some()))
    });
    assert_eq!(
        pin.recv_timeout(Duration::from_secs(10)),
        Ok((true, false)),
        "a pin waited for the slab"
    );
    drop(held);
    loader.reclaim().unwrap();
    // SAFETY: only this test loads or retires images in this slab; the RX view stays mapped.
    let dead = unsafe { std::slice::from_raw_parts(text as *const u8, small.text() - text) };
    let trap = match TARGET.arch() {
        Arch::X86_64 => 0xcc,
        Arch::Aarch64 => 0,
    };
    assert!(dead.iter().all(|&byte| byte == trap));
    assert_eq!((small.text() - 1) / page, small.text() / page);
    jump.send(()).unwrap();
    executor.join().unwrap();
    stop.store(true, Ordering::Release);
    worker.join().unwrap();

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
        [
            host::LIVE.to_string(),
            host::LIVE.to_string(),
            host::LIVE.into()
        ]
    );
    // The neighbour runs, and the dead image's bytes stay installed until the cohort goes.
    assert_eq!((call(&small, 1), loader.installed()), (4, installed));
    assert_eq!(neighbor_bytes(), original);
    assert_eq!(loader.unrevoked(), 0);
    for role in ["entry", "tail"] {
        let status = child(
            "tests::g11::retired_text_traps_while_its_shared_page_neighbor_runs",
            role.as_ref(),
        );
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(host::FAULT)
        );
    }
    drop(small);
    assert_eq!(loader.installed(), 0);
    let mut replacement = image(7);
    replacement.text.resize(WIDE, 0);
    replacement.spans[0].length = WIDE as u32;
    replacement.data.extend_from_slice(&[0; 8]);
    replacement
        .data_fixups
        .push(fixup(16, Kind::Abs64Text, 0, 0));
    let replacement = load(&loader, &replacement.encode()).unwrap();
    let data = loader.0.slab.lock().unwrap().cohorts[&replacement.image.cohort.unwrap()]
        .data
        .0;
    // SAFETY: the replacement's read-only data holds its aligned text-address fixup at byte 16.
    assert_eq!(unsafe { *(loader.0.data(data + 16) as *const usize) }, text);
    assert_eq!(replacement.text(), text);
    std::thread::scope(|scope| {
        scope
            .spawn(|| assert_eq!(call(&replacement, 11), 29))
            .join()
            .unwrap();
    });
    platform::check(&loader, replacement);
}
