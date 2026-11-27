use super::*;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
use std::{io::Write, os::unix::net::UnixStream};

/// An edge wakes only its direction. A batch collected before closing a
/// socket cannot wake a waiter on the replacement in the same slot.
#[test]
fn s0_net_rearm_ignores_stale_directional_readiness() {
    use crate::ready::Ready;

    let (socket, _peer) = UnixStream::pair().unwrap();
    let mut sockets = Sockets::default();
    let key = sockets.insert(socket.into()).unwrap();
    sockets.arm(key, false, 101).unwrap();
    let first = sockets.arm(key, true, 201).unwrap();
    let mut ready = Vec::new();
    sockets.dispatch(first, Ready::READABLE, |t| ready.push(t));
    assert_eq!(ready, [101]);
    sockets.dispatch(first, Ready::WRITABLE, |t| ready.push(t));
    assert_eq!(ready, [101, 201]);

    assert!(sockets.release(key));
    let (socket, _peer) = UnixStream::pair().unwrap();
    let replacement = sockets.insert(socket.into()).unwrap();
    let second = sockets.arm(replacement, false, 102).unwrap();
    let slot_mask = (SOCKET_SLOT_LIMIT - 1) as u64;
    assert_eq!(
        first & slot_mask,
        second & slot_mask,
        "the registry reuses the slot"
    );
    assert_ne!(first, second, "a replacement has a fresh readiness tag");
    sockets.dispatch(first, Ready::READABLE, |t| ready.push(t));
    assert_eq!(ready, [101, 201], "the old edge must not wake a new reader");
    sockets.dispatch(second, Ready::READABLE, |t| ready.push(t));
    assert_eq!(ready, [101, 201, 102]);
}

#[test]
fn a_busy_driver_turn_leaves_readiness_for_the_next_worker() {
    let mut sockets = Sockets::default();
    let (socket, mut peer) = UnixStream::pair().unwrap();
    socket.set_nonblocking(true).unwrap();
    let key = sockets.insert(socket.into()).unwrap();
    sockets.arm(key, false, 17).unwrap();
    let driver = sockets.poller().unwrap();
    let held = driver.try_turn().unwrap();
    peer.write_all(b"x").unwrap();
    let busy = std::thread::scope(|scope| {
        scope
            .spawn(|| driver.poll(0).unwrap().is_none())
            .join()
            .unwrap()
    });
    drop(held);
    assert!(busy);
    let events = driver.poll(0).unwrap().unwrap();
    assert!(driver.poll(0).unwrap().is_none());
    let mut tokens = Vec::new();
    sockets.dispatch_polled(events, |token| tokens.push(token));
    assert_eq!(tokens, [17]);

    let events = driver.poll(0).unwrap().unwrap();
    assert_eq!(sockets.dispatch_polled(events, |_| unreachable!()), 0);
}

/// Poll until `token` is readied or two seconds pass.
fn readied(sockets: &mut Sockets, token: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        let mut found = false;
        sockets.poll(20, |t| found |= t == token).unwrap();
        if found {
            return true;
        }
    }
    false
}

#[cfg(target_os = "linux")]
#[test]
fn refused_submit_keeps_a_closed_socket_until_consumed() {
    use std::sync::atomic::Ordering::Relaxed;
    if reactor().unwrap() != Reactor::IoUring {
        return;
    }
    let (socket, _peer) = UnixStream::pair().unwrap();
    let mut sockets = Sockets::default();
    let key = sockets.insert(socket.into()).unwrap();
    sockets.record_mut(key).zero_copy = Some(true);
    let (poller, tag) = sockets.send_target(key).unwrap().unwrap();
    let Backend::Uring(ring) = &poller.backend else {
        unreachable!()
    };
    let owner = Arc::downgrade(&sockets.record(key).fd);
    let mut slot = poller.tx_slot(1).unwrap();
    slot.bytes()[0] = b'x';

    ring.refuse_submit.store(true, Relaxed);
    sockets.arm(key, false, 17).unwrap();
    slot.send(tag, 0..1).unwrap();
    slot.cancel();
    drop(slot);
    assert!(sockets.release(key));
    assert!(owner.upgrade().is_some(), "closed before SQ consumption");

    ring.refuse_submit.store(false, Relaxed);
    poller.poll(0).unwrap();
    assert!(owner.upgrade().is_none(), "retained after SQ consumption");
}

/// Workers migrate, so the thread that registered a socket may be gone
/// when it becomes readable. io_uring ties a poll to its submitting task.
#[test]
fn a_socket_registered_by_an_exited_thread_still_reports_readiness() {
    let mut sockets = Sockets::default();
    let (socket, mut peer) = UnixStream::pair().unwrap();
    socket.set_nonblocking(true).unwrap();
    let key = sockets.insert(socket.into()).unwrap();
    let sockets = std::thread::scope(|scope| {
        scope
            .spawn(move || {
                sockets.arm(key, false, 5).unwrap();
                sockets
            })
            .join()
            .unwrap()
    });
    let mut sockets = sockets;
    // The registration's first report, before any byte arrived.
    sockets.poll(0, |_| {}).unwrap();
    peer.write_all(b"x").unwrap();
    sockets.arm(key, false, 6).unwrap();
    assert!(readied(&mut sockets, 6), "{:?}", reactor());
}

/// Each direction of a socket waits on its own readiness. A writer that
/// parks on a full socket while a reader's wait is already in the kernel
/// is woken once the peer drains it, and that wakes no reader, on epoll
/// and, where the kernel offers it, on io_uring.
///
/// Slot pollers, with epoll as the oracle: a socket registered on slot 0
/// and released from slot 1 (not its home) is unregistered at its home
/// and closed, so its peer reads the end of stream; a socket opened next,
/// usually on the freed descriptor number, and registered on slot 0 is
/// reported under its own token alone, never the released one's.
#[test]
fn a_writer_parked_beside_a_waiting_reader_is_woken_alone() {
    use std::io::Read;
    #[cfg_attr(not(target_os = "linux"), expect(unused_mut))]
    let mut makers: Vec<fn() -> io::Result<Arc<Poller>>> = vec![Poller::new_readiness];
    #[cfg(target_os = "linux")]
    if kernel_offers_io_uring() {
        makers.push(Poller::new_uring);
    }
    let mut oracle = None;
    for make in makers {
        let seen = cross_home_release(make);
        // Whether the number was reused depends on descriptors other tests
        // open meanwhile, so it is reported, not compared.
        let law = (seen.1, seen.2, seen.3.clone());
        assert_eq!(law, (true, true, vec![2]), "{seen:?}");
        assert_eq!(*oracle.get_or_insert_with(|| law.clone()), law);
        let poller = make().unwrap();
        let backend = poller.reactor();
        let mut sockets = Sockets::default();
        sockets.poller = Some(poller);
        let (mut socket, mut peer) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        peer.set_nonblocking(true).unwrap();
        let fd = socket.try_clone().unwrap();
        let key = sockets.insert(fd.into()).unwrap();
        sockets.arm(key, false, 1).unwrap();
        // Let any first report settle so the reader's wait is in the kernel.
        sockets.poll(0, |_| {}).unwrap();
        sockets.arm(key, false, 1).unwrap();
        // The writer parks only after its write would block.
        let chunk = [0u8; 4096];
        while socket.write(&chunk).is_ok() {}
        sockets.arm(key, true, 2).unwrap();
        let mut drain = [0u8; 65536];
        while peer.read(&mut drain).is_ok() {}
        let mut woken = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !woken.contains(&2) && std::time::Instant::now() < deadline {
            sockets.poll(20, |token| woken.push(token)).unwrap();
        }
        assert_eq!(woken, [2], "{backend:?}");
    }
}

/// See `a_writer_parked_beside_a_waiting_reader_is_woken_alone`. Returns
/// whether the new socket reused the released one's number, whether the
/// home reported nothing before B turned readable, whether the released
/// socket's peer read its end, and the tokens the home reported.
fn cross_home_release(make: fn() -> io::Result<Arc<Poller>>) -> (bool, bool, bool, Vec<u64>) {
    use std::cell::Cell;
    use std::io::Read;
    thread_local! {
        static HERE: Cell<Option<Here>> = const { Cell::new(None) };
    }
    fn here() -> Option<Here> {
        HERE.get()
    }
    let at = |slot| HERE.set(Some(Here { unit: 1, slot }));
    let pollers = share(vec![make().unwrap(), make().unwrap()], here).unwrap();
    let mut sockets = Sockets::default();
    let mut woken = Vec::new();
    let turn = |sockets: &mut Sockets, woken: &mut Vec<u64>| {
        if let Some(events) = pollers[0].poll(20).unwrap() {
            sockets.dispatch_polled(events, |token| woken.push(token));
        }
    };
    let (a, mut peer_a) = UnixStream::pair().unwrap();
    a.set_nonblocking(true).unwrap();
    peer_a.set_nonblocking(true).unwrap();
    let fd_a = a.as_raw_fd();
    at(0);
    let ka = sockets.insert(a.into()).unwrap();
    sockets.arm(ka, false, 1).unwrap();
    // The home's turn puts the poll in the kernel; then A turns readable.
    turn(&mut sockets, &mut woken);
    woken.clear();
    peer_a.write_all(b"x").unwrap();
    // Released by a unit now on slot 1: the removal goes to slot 0, and
    // the descriptor closes with A's readiness still pending there.
    at(1);
    assert!(sockets.release(ka));
    let (b, mut peer_b) = UnixStream::pair().unwrap();
    b.set_nonblocking(true).unwrap();
    let reused = b.as_raw_fd() == fd_a;
    at(0);
    let kb = sockets.insert(b.into()).unwrap();
    sockets.arm(kb, false, 2).unwrap();
    // Nothing is readable on B yet: no report may reach its reader.
    turn(&mut sockets, &mut woken);
    let quiet = woken.is_empty();
    peer_b.write_all(b"y").unwrap();
    let mut ended = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !(ended && woken.contains(&2)) && std::time::Instant::now() < deadline {
        turn(&mut sockets, &mut woken);
        // Unread bytes make the close a reset; the end of stream follows.
        ended |= matches!(peer_a.read(&mut [0u8; 8]), Ok(0));
    }
    drop(sockets);
    HERE.set(None);
    *SHARED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    (reused, quiet, ended, woken)
}

#[cfg(target_os = "linux")]
#[test]
fn reaping_counts_every_completion() {
    use io_uring::opcode::PollRemove;

    // Removal completions produce no event in the caller's array.
    let ring = Uring::with_cq_size(512).unwrap();
    let mut events = [crate::ready::Event::EMPTY; 128];
    let pending = || ring.ring.cq_len();
    for initial in [129, 100] {
        for submitted in 1..=129 {
            ring.push(PollRemove::new(0).build().user_data(REMOVED), None);
            if submitted <= initial && (submitted % 64 == 0 || submitted == initial) {
                ring.enter(Some(0));
            }
        }
        assert_eq!(pending(), initial);
        assert_eq!(ring.wait(&mut events, Some(0)).unwrap(), 0);
        assert_eq!(pending(), 1, "initial backlog {initial}");
        assert!(ring.due());
        assert_eq!(ring.wait(&mut events, Some(0)).unwrap(), 0);
        assert_eq!(pending(), 0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn replaced_watch_leaves_one_poll() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let ring = Uring::open().unwrap();
    ring.enter(Some(0));
    let poll_count = || {
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", ring.ring.as_raw_fd()))
            .unwrap();
        let (_, polls) = info.split_once("PollList:\n").unwrap();
        polls
            .lines()
            .take_while(|line| line.starts_with("  op="))
            .count()
    };
    let baseline = poll_count();
    let socket = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = socket.local_addr().unwrap();
    let peer = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let fd = Arc::new(OwnedFd::from(socket));
    let tag = 3;
    let settle = || {
        let mut events = [crate::ready::Event::EMPTY; 128];
        for _ in 0..16 {
            if !ring.due() {
                return;
            }
            ring.wait(&mut events, Some(0)).unwrap();
        }
        assert!(!ring.due(), "poll removals did not settle");
    };
    let send = AtomicBool::new(false);
    let sent = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let peer = scope.spawn(|| {
            while !send.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            let result = peer.send_to(b"x", address);
            sent.store(true, Ordering::Release);
            result
        });
        ring.add(tag, fd.clone()).unwrap();
        ring.enter(Some(0));
        settle();
        // Let a peer make the watch readable without this owner entering
        // the kernel. The removal meets a poll owned by pending task work.
        send.store(true, Ordering::Release);
        while !sent.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        ring.recover_watches();
        ring.watches().submit_recovery(&ring).unwrap();
        ring.enter(Some(0));
        assert_eq!(peer.join().unwrap().unwrap(), 1);
    });
    settle();
    assert!(ring.watches().retried > 0, "no retriable removal observed");
    assert_eq!(poll_count(), baseline + 1);
    assert!(ring.remove(tag));
    settle();
    assert_eq!(poll_count(), baseline);

    // Thirty-two replacements followed by another recovery before reaping.
    // The second recovery has 32 old removals and 64 replacement SQEs.
    for tag in 1..=32 {
        ring.add(tag, fd.clone()).unwrap();
    }
    settle();
    for _ in 0..2 {
        ring.recover_watches();
        ring.watches().submit_recovery(&ring).unwrap();
        assert_eq!(ring.pending_submissions(), 64);
        ring.enter(Some(0));
    }
    settle();
    assert_eq!(poll_count(), baseline + 32);
    for tag in 1..=32 {
        assert!(ring.remove(tag));
    }
    settle();
    assert_eq!(poll_count(), baseline);
}

/// More sockets turn ready at once than the io_uring completion queue
/// holds. Every one must still be reported: none of the edges past the
/// queue's capacity may be lost.
#[test]
fn readiness_beyond_the_completion_queue_is_all_reported() {
    const CQ_ENTRIES: u32 = 512;
    let (poller, completion_entries) = match reactor().unwrap() {
        #[cfg(target_os = "linux")]
        Reactor::IoUring => {
            let ring = Uring::with_cq_size(CQ_ENTRIES).unwrap();
            let entries = ring.ring.cq_entries() as u32;
            (Poller::from_uring(ring), entries)
        }
        #[cfg(target_os = "linux")]
        Reactor::Epoll => (Poller::new_readiness().unwrap(), CQ_ENTRIES),
        #[cfg(target_os = "macos")]
        Reactor::Kqueue => (Poller::new_readiness().unwrap(), CQ_ENTRIES),
    };
    // Keep the burst at three times the CQ while fitting within 4,096 fds.
    let socket_count = 3 * completion_entries as usize;
    let mut sockets = Sockets::default();
    sockets.poller = Some(poller);
    let mut peers = Vec::with_capacity(socket_count);
    let mut keys = Vec::with_capacity(socket_count);
    for token in 0..socket_count as u64 {
        let (socket, peer) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let key = sockets.insert(socket.into()).unwrap();
        sockets.arm(key, false, token).unwrap();
        keys.push(key);
        peers.push(peer);
    }
    // Each registration's first report, before any byte arrived. A reader
    // woken now finds no byte and waits again, as a real waiter does. The
    // reactor must settle: no readiness is reported while nothing changes.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut woken = Vec::new();
    while sockets.poll(0, |token| woken.push(token)).unwrap() != 0 {
        for token in woken.drain(..) {
            sockets.arm(keys[token as usize], false, token).unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "readiness never settled under {:?}",
            reactor()
        );
    }
    for peer in &mut peers {
        peer.write_all(b"x").unwrap();
    }
    let mut seen = vec![false; socket_count];
    let mut left = socket_count;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while left != 0 && std::time::Instant::now() < deadline {
        sockets
            .poll(20, |token| {
                let seen = &mut seen[token as usize];
                left -= usize::from(!*seen);
                *seen = true;
            })
            .unwrap();
    }
    assert_eq!(
        left,
        0,
        "{left} of {socket_count} unreported under {:?}",
        reactor()
    );
}

/// A ready listener whose watch waits in the mailbox behind more idle
/// watches than the SQ holds must still be reported: the owner may not
/// sleep while mailbox work is unsubmitted.
#[cfg(target_os = "linux")]
#[test]
fn a_ready_listener_behind_a_full_sq_of_idle_watches_is_reported() {
    if reactor().unwrap() != Reactor::IoUring {
        return;
    }
    let ring = Uring::open().unwrap();
    let listen = || {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    };
    let idle: Vec<_> = (0..=Uring::ENTRIES as u64).map(|_| listen()).collect();
    for (tag, listener) in (1..).zip(&idle) {
        ring.add(tag, Arc::new(listener.try_clone().unwrap().into()))
            .unwrap();
    }
    let ready = listen();
    let _client = std::net::TcpStream::connect(ready.local_addr().unwrap()).unwrap();
    let ready_tag = idle.len() as u64 + 1;
    ring.add(ready_tag, Arc::new(ready.try_clone().unwrap().into()))
        .unwrap();
    // The completion ring's watch and the idle ones fill the SQ first.
    assert!(ring.pending_submissions() > Uring::ENTRIES as usize + 1);

    let bound = std::time::Duration::from_secs(5);
    let start = std::time::Instant::now();
    let mut out = vec![crate::ready::Event::EMPTY; 2 * POLL_BATCH as usize];
    let mut reported = false;
    while !reported && start.elapsed() < bound {
        let left = bound.saturating_sub(start.elapsed()).as_nanos() as u64;
        let n = ring.wait(&mut out, Some(left)).unwrap();
        reported = out[..n]
            .iter()
            .any(|event| event.token() == ready_tag && event.ready().is_readable());
    }
    assert!(reported, "ready listener unreported after {bound:?}");
}

#[cfg(target_os = "linux")]
const SLOT: usize = 256 << 10;

/// A loopback TCP connection whose peer has stopped reading, with small
/// buffers, so a large send waits in the kernel. None unless this
/// process's poller sends zero-copy.
#[cfg(target_os = "linux")]
fn stuck_peer() -> Option<(Sockets, u64, Arc<Poller>, u64, std::net::TcpStream)> {
    let (mut sockets, key, peer) = small_pair();
    // These laws are about the slots, so their loopback peer sends
    // zero-copy as under `always`.
    sockets.record_mut(key).zero_copy = Some(true);
    let (poller, tag) = sockets.send_target(key).unwrap()?;
    Some((sockets, key, poller, tag, peer))
}

/// A registered loopback client whose writes back up after a few
/// 16 KiB buffers, and its peer.
fn small_pair() -> (Sockets, u64, std::net::TcpStream) {
    use std::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let peer =
        crate::loopback::accept_from_address(&listener, client.local_addr().unwrap()).unwrap();
    for (socket, option) in [(&client, libc::SO_SNDBUF), (&peer, libc::SO_RCVBUF)] {
        let size = 16384i32;
        // SAFETY: a live socket; an initialized option value.
        unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&raw const size).cast(),
                4,
            )
        };
    }
    client.set_nonblocking(true).unwrap();
    let mut sockets = Sockets::default();
    let key = sockets.insert(OwnedFd::from(client)).unwrap();
    (sockets, key, peer)
}

/// Reap completions until `done` holds, for at most five seconds.
fn reap_until(sockets: &mut Sockets, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "no completion");
        sockets.poll(10, |_| {}).unwrap();
    }
}

/// A burst of sole-record completions far past the completion CQ: zero-copy
/// sends (a result and a notification each) and one-shot file operations,
/// entered without reaping so the kernel parks the excess on its overflow
/// list. Every operation submitted completes exactly once, and no turn
/// sleeps while the overflow holds completions.
#[cfg(target_os = "linux")]
#[test]
fn a_completion_overflow_burst_loses_no_completion() {
    use std::sync::atomic::{AtomicU32, Ordering};

    const SENDS: usize = 1024;
    const FILES: usize = 1024;
    let ring = Uring::open().unwrap();
    assert!(ring.zc, "the kernel runs SEND_ZC");
    let poller = Poller::from_uring(ring);
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (socket, _) = listener.accept().unwrap();
    socket.set_nonblocking(true).unwrap();
    socket.set_nodelay(true).unwrap();
    let reading = std::thread::spawn(move || -> io::Result<(usize, usize)> {
        use std::io::Read;
        let mut got = vec![0; SENDS];
        let mut read = 0;
        while read < SENDS {
            match peer.read(&mut got[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(error) => return Err(error),
            }
        }
        let wrong = got[..read].iter().filter(|&&b| b != b'a').count();
        Ok((read, wrong))
    });
    let mut sockets = Sockets::default();
    sockets.poller = Some(poller.clone());
    let key = sockets.insert(socket.into()).unwrap();
    sockets.record_mut(key).zero_copy = Some(true);
    let (_, tag) = sockets.send_target(key).unwrap().unwrap();
    let Backend::Uring(ring) = &poller.backend else {
        unreachable!()
    };
    for _ in 0..SENDS {
        let mut slot = poller.tx_slot(1).unwrap();
        slot.bytes()[0] = b'a';
        slot.send(tag, 0..1).unwrap();
        assert!(matches!(slot.state(), TxState::Sending));
    }
    let calls: Vec<_> = (0..FILES).map(|_| AtomicU32::new(0)).collect();
    let files: Vec<_> = calls
        .iter()
        .map(|calls| {
            FileRequest::new(
                io_uring::opcode::Nop::new().build(),
                std::ptr::from_ref(calls).cast(),
                count,
            )
        })
        .collect();
    for file in &files {
        // SAFETY: NOP has one result and both request and counter are pinned.
        assert!(unsafe { poller.submit_file(file) });
    }
    for _ in 0..=(2 * SENDS + FILES) / Uring::ENTRIES as usize {
        ring.enter(Some(0));
    }
    let completion = &ring.completion.ring;
    assert!(completion.overflowed(), "the burst overflowed the CQ");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while ring.busy_tx_slots() != 0 || calls.iter().any(|c| c.load(Ordering::Acquire) == 0) {
        assert!(
            std::time::Instant::now() < deadline,
            "a completion was lost"
        );
        // A turn that sleeps while completions wait in the overflow list
        // spends this whole second.
        sockets.poll(1000, |_| {}).unwrap();
    }
    assert!(calls.iter().all(|c| c.load(Ordering::Acquire) == 1));
    assert_eq!(poller.tx_notifications().0, SENDS as u64);
    assert_eq!(completion.dropped(), 0);
    // SAFETY: the registry still owns the socket.
    assert_eq!(unsafe { libc::shutdown(sockets.fd(key), libc::SHUT_WR) }, 0);
    let (received, wrong) = reading.join().unwrap().unwrap();
    assert_eq!(received, SENDS);
    assert_eq!(
        wrong, 0,
        "{wrong} of {SENDS} bytes the peer read are not the sent bytes"
    );
}

unsafe fn count(context: *const (), _: i32) {
    // SAFETY: every counter stays pinned until all calls finish.
    let calls = unsafe { &*context.cast::<std::sync::atomic::AtomicU32>() };
    calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

/// A slot whose send waits, cancelled once part of it is in the kernel's
/// write queue: the kernel still reads that part from the slot.
#[cfg(target_os = "linux")]
fn cancelled_send(sockets: &mut Sockets, poller: &Arc<Poller>, tag: u64) -> TxSlot {
    let mut slot = poller.tx_slot(SLOT).unwrap();
    slot.bytes().fill(b'a');
    slot.send(tag, 0..SLOT).unwrap();
    // The send takes what the window allows and then waits.
    std::thread::sleep(std::time::Duration::from_millis(20));
    sockets.poll(0, |_| {}).unwrap();
    assert_eq!(slot.state(), TxState::Sending);
    slot
}

#[cfg(target_os = "linux")]
fn sent(slot: &TxSlot) -> usize {
    match slot.state() {
        TxState::Sent(n) if n > 0 => n as usize,
        state => panic!("{state:?}"),
    }
}

/// Fill the slots the next sends would take.
#[cfg(target_os = "linux")]
fn refill(poller: &Arc<Poller>) -> Vec<TxSlot> {
    (0..4)
        .map(|_| {
            let mut slot = poller.tx_slot(SLOT).unwrap();
            slot.bytes().fill(b'b');
            slot
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn read_all_a(peer: &mut std::net::TcpStream, n: usize) {
    use std::io::Read;
    let mut got = vec![0; n];
    peer.read_exact(&mut got).unwrap();
    let wrong = got.iter().filter(|&&b| b != b'a').count();
    assert_eq!(
        wrong, 0,
        "{wrong} of {n} bytes the peer read are not the sent bytes"
    );
}

/// A send's answer does not end the kernel's reads of its slot: until
/// the notification, the queued part is still read from it. A claim
/// dropped after the answer must not hand the slot to the next send.
#[cfg(target_os = "linux")]
#[test]
fn an_answered_slot_is_not_reused_before_its_notification() {
    let Some((mut sockets, key, poller, tag, mut peer)) = stuck_peer() else {
        return;
    };
    let slot = cancelled_send(&mut sockets, &poller, tag);
    slot.cancel();
    reap_until(&mut sockets, || matches!(slot.state(), TxState::Sent(_)));
    let n = sent(&slot);
    drop(slot);
    let _next = refill(&poller);
    read_all_a(&mut peer, n);
}

/// A claim dropped before its cancelled send answers: the answer comes
/// with more to follow, and the slot waits for that notification too.
#[cfg(target_os = "linux")]
#[test]
fn a_cancelled_send_keeps_its_slot_until_the_notification() {
    let Some((mut sockets, key, poller, tag, mut peer)) = stuck_peer() else {
        return;
    };
    let slot = cancelled_send(&mut sockets, &poller, tag);
    let (notified, _) = poller.tx_notifications();
    slot.cancel();
    drop(slot);
    // The answer, and not yet the notification, which waits for the peer.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    while std::time::Instant::now() < deadline {
        sockets.poll(10, |_| {}).unwrap();
    }
    assert_eq!(poller.tx_notifications().0, notified);
    let _next = refill(&poller);
    // Everything the cancelled send took: the window's worth, copied when
    // it arrived, then the write queue's, read from the slot only now.
    use std::io::Read;
    peer.set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    let mut got = Vec::new();
    let mut buf = vec![0; 1 << 16];
    while let Ok(n @ 1..) = peer.read(&mut buf) {
        got.extend_from_slice(&buf[..n]);
    }
    assert!(got.len() > 2 * 16384, "{}", got.len());
    let wrong = got.iter().filter(|&&b| b != b'a').count();
    assert_eq!(
        wrong,
        0,
        "{wrong} of {} bytes the peer read are not the sent bytes",
        got.len()
    );
}

/// A ring closed while the kernel still reads a slot: those bytes must
/// never return to the allocator.
#[cfg(target_os = "linux")]
#[test]
fn a_ring_dropped_with_notifications_owed_keeps_their_bytes() {
    // Slots come from the heap, where a freed one is handed out again.
    // SAFETY: a process-wide malloc tuning, harmless to other tests.
    unsafe { libc::mallopt(libc::M_MMAP_THRESHOLD, 64 << 20) };
    let Some((mut sockets, key, poller, tag, mut peer)) = stuck_peer() else {
        return;
    };
    let slot = cancelled_send(&mut sockets, &poller, tag);
    slot.cancel();
    reap_until(&mut sockets, || matches!(slot.state(), TxState::Sent(_)));
    let n = sent(&slot);
    drop((slot, sockets, poller));
    let reused: Vec<Vec<u8>> = (0..8).map(|_| vec![b'b'; SLOT]).collect();
    read_all_a(&mut peer, n);
    drop(reused);
}

/// Slots stay kept while sends go on, and a ring that stopped sending
/// frees them: a blocking turn wakes for the trim, which comes a full
/// interval after the last slot taken, so idle memory returns to 0.
#[cfg(target_os = "linux")]
#[test]
fn a_ring_that_stopped_sending_frees_its_slots() {
    let Some((mut sockets, key, poller, tag, mut peer)) = stuck_peer() else {
        return;
    };
    let Backend::Uring(ring) = &poller.backend else {
        unreachable!()
    };
    let mut slot = poller.tx_slot(1024).unwrap();
    slot.bytes().fill(b'a');
    slot.send(tag, 0..1024).unwrap();
    drop(slot);
    let (notified, _) = poller.tx_notifications();
    reap_until(&mut sockets, || poller.tx_notifications().0 > notified);
    read_all_a(&mut peer, 1024);
    assert!(ring.tx_kept());
    let start = std::time::Instant::now();
    let long = (3 * sendzc::TRIM).as_millis() as i32;
    while ring.tx_kept() && start.elapsed() < 3 * sendzc::TRIM {
        sockets.poll(long, |_| {}).unwrap();
    }
    let took = start.elapsed();
    assert!(
        (sendzc::TRIM / 2..2 * sendzc::TRIM).contains(&took) && !ring.tx_kept(),
        "idle slots freed after {took:?}, kept: {}",
        ring.tx_kept()
    );
}

/// An abandoned send on a peer that stopped reading: its queued bytes
/// are dropped at the close, the peer sees a reset rather than every
/// committed byte, and the slot's notification follows promptly.
#[cfg(target_os = "linux")]
#[test]
fn an_abandoned_send_leaves_no_queued_bytes_after_close() {
    use std::io::Read;
    let Some((mut sockets, key, poller, tag, mut peer)) = stuck_peer() else {
        return;
    };
    let slot = cancelled_send(&mut sockets, &poller, tag);
    let (notified, _) = poller.tx_notifications();
    slot.cancel();
    sockets.abandon_send(key);
    drop(slot);
    assert!(sockets.release(key));
    reap_until(&mut sockets, || poller.tx_notifications().0 > notified);
    let mut total = 0;
    let mut buf = vec![0; 1 << 16];
    let end = loop {
        match peer.read(&mut buf) {
            Ok(0) => break None,
            Ok(n) => total += n,
            Err(e) => break Some(e.kind()),
        }
    };
    assert_eq!(
        end,
        Some(io::ErrorKind::ConnectionReset),
        "after {total} bytes"
    );
}

/// `auto` treats every loopback form as loopback, including an IPv4
/// loopback peer of an IPv6 socket, and nothing else.
#[cfg(target_os = "linux")]
#[test]
fn auto_send_zc_skips_loopback_peers_only() {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    assert!(is_loopback(&SocketAddr::from((
        Ipv4Addr::new(127, 3, 2, 1),
        9
    ))));
    assert!(is_loopback(&SocketAddr::from((Ipv6Addr::LOCALHOST, 9))));
    assert!(is_loopback(&SocketAddr::from((
        Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped(),
        9
    ))));
    assert!(!is_loopback(&SocketAddr::from((
        Ipv4Addr::new(10, 0, 0, 1),
        9
    ))));
    assert!(!is_loopback(&SocketAddr::from((
        Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped(),
        9
    ))));
}

/// A send that starts after an abandoned one takes back its reset: the
/// close then delivers every queued byte and a FIN, under both reactors.
#[test]
fn a_send_after_an_abandoned_one_closes_without_a_reset() {
    use std::io::Read;
    let (mut sockets, key, mut peer) = small_pair();
    sockets.abandon_send(key);
    sockets.resume_send(key);
    let bytes = vec![b'c'; 1 << 16];
    let mut queued = 0;
    while queued < bytes.len() {
        // SAFETY: a live socket and an in-bounds buffer.
        let n = unsafe {
            libc::send(
                sockets.fd(key),
                bytes[queued..].as_ptr().cast(),
                bytes.len() - queued,
                libc::MSG_NOSIGNAL,
            )
        };
        if n <= 0 {
            break;
        }
        queued += n as usize;
    }
    assert!(sockets.release(key));
    let mut received = Vec::new();
    peer.read_to_end(&mut received)
        .unwrap_or_else(|e| panic!("{e} after {} of {queued} bytes", received.len()));
    assert_eq!(received.len(), queued);
}

/// Releasing a listener frees its address on return, as closing it
/// does, including when another thread registered it.
#[test]
fn a_released_listener_can_be_bound_again_at_once() {
    for _ in 0..50 {
        let mut sockets = Sockets::default();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let key = sockets.insert(listener.into()).unwrap();
        sockets.mark_own_listener(key);
        let mut sockets = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    sockets.arm(key, false, 1).unwrap();
                    sockets
                })
                .join()
                .unwrap()
        });
        assert!(sockets.release(key));
        std::net::TcpListener::bind(address)
            .unwrap_or_else(|e| panic!("{address} under {:?}: {e}", reactor()));
    }
}

/// Whether a ring with the features the reactor needs (NODROP, EXT_ARG)
/// can be set up here, asked of the kernel directly.
#[cfg(target_os = "linux")]
fn kernel_offers_io_uring() -> bool {
    const FEAT_NODROP: u32 = 1 << 1;
    const FEAT_EXT_ARG: u32 = 1 << 8;
    // struct io_uring_params: 120 bytes, `features` at byte 20.
    let mut params = [0u32; 30];
    // SAFETY: io_uring_setup writes only into the 120-byte params.
    let fd = unsafe { libc::syscall(libc::SYS_io_uring_setup, 1u32, params.as_mut_ptr()) };
    if fd < 0 {
        return false;
    }
    // SAFETY: the ring descriptor was just created and is owned here.
    drop(unsafe { OwnedFd::from_raw_fd(fd as i32) });
    let wanted = FEAT_NODROP | FEAT_EXT_ARG;
    params[5] & wanted == wanted
}

#[cfg(target_os = "linux")]
#[path = "choice_tests.rs"]
mod choice_tests;
#[path = "udp_tests.rs"]
mod udp_tests;

#[cfg(target_os = "linux")]
#[test]
fn reactor_plan_single_source_and_memlock_fallback() {
    let config = ReactorPlan {
        iowq_bounded: 1,
        force_epoll: true,
        ..ReactorPlan::for_batch(64)
    };
    assert_eq!(choose(config).unwrap(), Reactor::Epoll);

    let config = ReactorPlan {
        force_epoll: false,
        ..config
    };
    let ring = match Uring::open_with(config) {
        Ok(ring) => ring,
        Err(error) if !kernel_offers_io_uring() => {
            eprintln!("io_uring unavailable: {error}");
            return;
        }
        Err(error) => panic!("host offers io_uring but refused the derived ring: {error}"),
    };
    assert_eq!(ring.ring.cq_entries(), 128);
    assert_eq!(ring.completion.ring.cq_entries(), 64);
    assert_eq!(ring.plan.poll_batch, 64);
    let poller = Poller::from_uring(ring);
    let mut events = vec![crate::ready::Event::EMPTY; 2 * config.poll_batch as usize];
    let mut peers = Vec::new();
    for index in 0..96 {
        let (socket, peer) = UnixStream::pair().unwrap();
        let fd = Arc::new(OwnedFd::from(socket));
        let tag = shared_tag(0x10_000, index, 1);
        poller.add(&fd, tag, true).unwrap();
        peers.push((fd, peer));
    }
    let count = poller.wait_slot(&mut events, Some(10_000_000)).unwrap();
    assert!(
        (1..=64).contains(&count),
        "readiness batch returned {count}"
    );
    assert!(
        events[..count]
            .iter()
            .all(|event| event.token() >> SHARED_UNIT_SHIFT == 0x10_000)
    );
}

#[cfg(target_os = "linux")]
#[path = "unit_tag_tests.rs"]
mod unit_tag_tests;
