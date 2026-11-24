use super::*;
use std::{io::Write, os::unix::net::UnixStream};

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
#[cfg(target_os = "linux")]
#[test]
fn a_writer_parked_beside_a_waiting_reader_is_woken_alone() {
    use std::io::Read;
    let mut makers: Vec<fn() -> io::Result<Arc<Poller>>> = vec![Poller::new_epoll];
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
#[cfg(target_os = "linux")]
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
    let mut events = [crate::sys::epoll_event { events: 0, u64: 0 }; 128];
    let pending = || {
        // SAFETY: this test exclusively owns the ring and does not consume the queue here.
        unsafe { ring.ring.completion_shared() }.len()
    };
    for initial in [129, 100] {
        for submitted in 1..=129 {
            ring.push(PollRemove::new(1).build().user_data(REMOVED), None);
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
        let mut events = [crate::sys::epoll_event { events: 0, u64: 0 }; 128];
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
        ring.recover_watches(true);
        ring.submit_watch_recovery().unwrap();
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
        ring.recover_watches(true);
        ring.submit_watch_recovery().unwrap();
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
#[cfg(target_os = "linux")]
#[test]
fn readiness_beyond_the_completion_queue_is_all_reported() {
    const CQ_ENTRIES: u32 = 512;
    let (poller, completion_entries) = match reactor().unwrap() {
        Reactor::IoUring => {
            let ring = Uring::with_cq_size(CQ_ENTRIES).unwrap();
            let entries = ring.ring.params().cq_entries();
            (Poller::from_uring(ring), entries)
        }
        Reactor::Epoll => (Poller::new_epoll().unwrap(), CQ_ENTRIES),
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
#[cfg(target_os = "linux")]
fn reap_until(sockets: &mut Sockets, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "no completion");
        sockets.poll(10, |_| {}).unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn completion_ring_never_overflows() {
    let ring = Uring::with_cq_sizes(64, 64).unwrap();
    let poller = Poller::from_uring(ring);
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (socket, _) = listener.accept().unwrap();
    socket.set_nonblocking(true).unwrap();
    socket.set_nodelay(true).unwrap();
    let mut sockets = Sockets::default();
    sockets.poller = Some(poller.clone());
    let key = sockets.insert(socket.into()).unwrap();
    sockets.record_mut(key).zero_copy = Some(true);
    let (_, tag) = sockets.send_target(key).unwrap().unwrap();
    let Backend::Uring(ring) = &poller.backend else {
        unreachable!()
    };
    let count = ring.completion.ring.params().cq_entries() as usize * 2;
    let mut copied = 0;
    for _ in 0..count {
        let mut slot = poller.tx_slot(1).unwrap();
        slot.bytes()[0] = b'a';
        slot.send(tag, 0..1).unwrap();
        match slot.state() {
            TxState::Sending => {}
            TxState::Sent(1) => copied += 1,
            other => panic!("unexpected transmit state {other:?}"),
        }
        drop(slot);
    }
    assert_eq!(copied, count - 21);
    for _ in 0..2 {
        for _ in 0..64 {
            ring.push(io_uring::opcode::Nop::new().build(), None);
        }
        ring.enter(Some(0));
    }
    // SAFETY: the test owns the rings and these observations consume nothing.
    unsafe {
        assert!(ring.ring.submission_shared().cq_overflow());
        assert_eq!(ring.completion.ring.completion_shared().overflow(), 0);
        assert!(!ring.completion.ring.submission_shared().cq_overflow());
    }
    reap_until(&mut sockets, || ring.busy_tx_slots() == 0);
    assert_eq!(poller.tx_notifications(), (21, 21));
    // SAFETY: the registry still owns the socket.
    assert_eq!(unsafe { libc::shutdown(sockets.fd(key), libc::SHUT_WR) }, 0);
    read_all_a(&mut peer, count);
}

#[cfg(target_os = "linux")]
#[test]
fn close_waits_for_completion_credit() {
    use std::{
        os::fd::IntoRawFd,
        sync::atomic::{AtomicI32, Ordering},
    };

    unsafe fn complete(context: *const (), result: i32) {
        // SAFETY: every test reply stays pinned until all calls finish.
        unsafe { &*context.cast::<AtomicI32>() }.store(result, Ordering::Release);
    }

    let ring = Uring::with_cq_sizes(64, 64).unwrap();
    let slots = ring.completion.ring.params().cq_entries() as usize;
    let poller = Poller::from_uring(ring);
    let Backend::Uring(ring) = &poller.backend else {
        unreachable!()
    };
    let replies: Vec<_> = (0..=slots).map(|_| AtomicI32::new(i32::MIN)).collect();
    for (unit, reply) in replies[..slots].iter().enumerate() {
        // SAFETY: NOP has one result and the reply is pinned.
        assert!(unsafe {
            poller.submit_file(
                unit as u16,
                io_uring::opcode::Nop::new().build(),
                std::ptr::from_ref(reply).cast(),
                complete,
            )
        });
    }
    ring.enter(Some(0));
    let fd = std::fs::File::open("/dev/null").unwrap().into_raw_fd();
    let close = io_uring::opcode::Close::new(io_uring::types::Fd(fd)).build();
    // SAFETY: CLOSE consumes the sole fd and its reply is pinned.
    assert!(unsafe {
        poller.submit_file(
            slots as u16,
            close,
            std::ptr::from_ref(&replies[slots]).cast(),
            complete,
        )
    });
    ring.enter(Some(0));
    // SAFETY: F_GETFD only inspects the descriptor, whether open or closed.
    assert_ne!(
        unsafe { libc::fcntl(fd, libc::F_GETFD) },
        -1,
        "CLOSE issued with a full CQ"
    );
    let mut events = [crate::sys::epoll_event { events: 0, u64: 0 }; 128];
    for _ in 0..4 {
        ring.wait(&mut events, Some(0)).unwrap();
    }
    assert!(
        replies
            .iter()
            .all(|reply| reply.load(Ordering::Acquire) == 0)
    );
    // SAFETY: no intervening open can reuse this fd in the test.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBADF)
    );
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

/// Set by the choice law for the fresh process it runs this test in.
#[cfg(target_os = "linux")]
const EXPECT_ENV: &str = "HIRSEL_NET_REACTOR_LAW_EXPECT";
#[cfg(target_os = "linux")]
const REFUSE_ENV: &str = "HIRSEL_NET_REACTOR_LAW_REFUSE";

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

/// Make this thread's `io_uring_setup` fail with EPERM, as a container's
/// seccomp profile does.
#[cfg(target_os = "linux")]
fn refuse_io_uring_setup() {
    let statement = |code: u32, jt, jf, k| libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    };
    let ret = libc::BPF_RET | libc::BPF_K;
    let filter = [
        // seccomp_data.nr
        statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0, 0, 0),
        statement(
            libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
            0,
            1,
            libc::SYS_io_uring_setup as u32,
        ),
        statement(ret, 0, 0, libc::SECCOMP_RET_ERRNO | libc::EPERM as u32),
        statement(ret, 0, 0, libc::SECCOMP_RET_ALLOW),
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr().cast_mut(),
    };
    // SAFETY: the filter outlives the call, which copies it.
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        assert_eq!(
            libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program),
            0,
            "{}",
            io::Error::last_os_error()
        );
    }
}

/// The fresh-process half of the choice law below.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "run by the reactor choice law in a fresh process"]
fn the_reactor_this_process_chose() {
    let Some(expected) = std::env::var_os(EXPECT_ENV) else {
        return;
    };
    if std::env::var_os(REFUSE_ENV).is_some() {
        refuse_io_uring_setup();
    }
    assert_eq!(reactor().unwrap().name(), expected);
    let mut sockets = Sockets::default();
    let (socket, mut peer) = UnixStream::pair().unwrap();
    socket.set_nonblocking(true).unwrap();
    let key = sockets.insert(socket.into()).unwrap();
    sockets.arm(key, false, 3).unwrap();
    sockets.poll(0, |_| {}).unwrap();
    peer.write_all(b"x").unwrap();
    sockets.arm(key, false, 4).unwrap();
    assert!(readied(&mut sockets, 4));
}

/// D5: with the variable unset, a process takes io_uring wherever the
/// kernel offers it and is not below the release gate, and epoll, still
/// carrying readiness, where `io_uring_setup` is refused.
#[cfg(target_os = "linux")]
#[test]
fn an_unset_reactor_takes_io_uring_where_offered_and_epoll_where_refused() {
    let offered = if kernel_offers_io_uring() && release_allows_uring(&kernel_release()) {
        "io_uring"
    } else {
        "epoll"
    };
    for (refused, expected) in [(false, offered), (true, "epoll")] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "reactor::tests::the_reactor_this_process_chose",
                "--ignored",
                "--nocapture",
            ])
            .env_remove(REACTOR_ENV)
            .env(EXPECT_ENV, expected);
        if refused {
            child.env(REFUSE_ENV, "1");
        }
        let out = child.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "refused={refused} expected {expected}:\n{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The gate admits 6.13 and later by numeric (major, minor), whatever
/// the distribution suffix; everything through 6.12.y, and an unreadable
/// release, stays on epoll.
#[test]
fn the_release_gate_admits_6_13_and_later_only() {
    for (release, allowed) in [
        ("6.12.118", false),
        ("6.12.999-generic", false),
        ("6.13", true),
        ("6.13.0", true),
        ("6.13.0-rc1", true),
        ("6.13-rc1", true),
        ("6.8.0-149-generic", false),
        ("6.1.0-27-amd64", false),
        ("6.2", false),
        ("5.15.0-130-generic", false),
        ("5.130.0", false),
        ("7.0.0-43-generic", true),
        ("10.1", true),
        ("6", false),
        ("", false),
        ("generic", false),
    ] {
        assert_eq!(release_allows_uring(release), allowed, "{release}");
    }
}

#[path = "udp_tests.rs"]
mod udp_tests;
