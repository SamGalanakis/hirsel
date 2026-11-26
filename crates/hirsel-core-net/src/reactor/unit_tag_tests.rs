use super::*;
use std::{
    os::unix::net::UnixStream,
    sync::atomic::{AtomicI32, Ordering},
};

#[test]
fn wide_unit_tags_route_socket_and_file_completions() {
    const HIGH: u32 = 0x10001;
    let route = shared_tag(HIGH, 0, 1);
    assert_eq!(route, 0x0100_0100_0100_0000);
    assert!(route < (u64::from(RESERVED_UNIT) << SHARED_UNIT_SHIFT));

    let (socket, _peer) = UnixStream::pair().unwrap();
    let mut sockets = Sockets::default();
    sockets.poller = Some(Poller::new_readiness().unwrap());
    let key = sockets.insert(socket.into()).unwrap();
    assert_eq!(sockets.arm(key, false, 43).unwrap(), 1 << 24);
    let mut delivered = None;
    sockets.dispatch(
        route & ((1 << SHARED_UNIT_SHIFT) - 1),
        crate::ready::Ready::READABLE,
        |token| {
            delivered = Some(token);
        },
    );
    assert_eq!(delivered, Some(43));

    let ring = match Uring::open_with(ReactorPlan::DEFAULT) {
        Ok(ring) => ring,
        Err(error) if !super::kernel_offers_io_uring() => {
            eprintln!("io_uring unavailable: {error}");
            return;
        }
        Err(error) => panic!("host offers io_uring but refused the file ring: {error}"),
    };
    let poller = Poller::from_uring(ring);
    let first = AtomicI32::new(-1);
    let second = AtomicI32::new(-1);
    unsafe fn completed(context: *const (), result: i32) {
        // SAFETY: the fixture pins an AtomicI32 until the file result.
        unsafe { (*context.cast::<AtomicI32>()).store(result, Ordering::Release) };
    }
    for (unit, answer) in [(1, &first), (HIGH, &second)] {
        // SAFETY: NOP emits exactly one CQE and each result has its own pin.
        assert!(unsafe {
            poller.submit_file(
                unit,
                io_uring::opcode::Nop::new().build(),
                ptr(answer),
                completed,
            )
        });
    }
    let mut events = [Event::EMPTY; 256];
    poller.wait_slot(&mut events, Some(10_000_000)).unwrap();
    assert_eq!(first.load(Ordering::Acquire), 0);
    assert_eq!(second.load(Ordering::Acquire), 0);
}

fn ptr(answer: &AtomicI32) -> *const () {
    std::ptr::from_ref(answer).cast()
}
