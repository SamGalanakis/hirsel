use super::*;

#[test]
fn turn_budget_starts_only_what_fits() {
    const LARGE: usize = 65_507;
    let poller = Poller::new_epoll().unwrap();
    let options = Options::new(8, 65_792).unwrap();
    let loopback = "127.0.0.1:0".parse().unwrap();
    let sender = std::net::UdpSocket::bind(loopback).unwrap();
    let open = || {
        let socket = Socket::open(false, options).unwrap();
        let endpoint = socket.bind(loopback).unwrap();
        let mut members = poller
            .add_udp_group(
                socket,
                options,
                NonZeroU16::MIN,
                Steer::Hash,
                |_| {},
                |_| {},
            )
            .unwrap();
        (members.pop().unwrap(), endpoint)
    };
    let (retired, _) = open();
    let (mut first, first_addr) = open();
    let (mut next, next_addr) = open();
    let data = vec![0u8; LARGE];
    {
        // The first source begins with five captures from previous turns.
        // The second source's two datagrams remain in the kernel.
        let mut groups = poller.udp_groups.get().unwrap().lock().unwrap();
        let capture = groups.live.get_mut(&first.group().tag).unwrap();
        for _ in 0..5 {
            assert_eq!(sender.send_to(&data, first_addr).unwrap(), LARGE);
            assert_eq!(
                capture
                    .pool
                    .read(&capture.group.socket, None, &mut TurnBudget::default())
                    .unwrap(),
                1
            );
        }
        for _ in 0..2 {
            assert_eq!(sender.send_to(&data, next_addr).unwrap(), LARGE);
        }

        // The retired source still has an entry at the head of the ready list.
        for member in [&retired, &first, &next] {
            let tag = member.group().tag;
            let Groups { live, runnable, .. } = &mut *groups;
            let intake = live.get_mut(&tag).unwrap();
            intake.reading = true;
            Groups::enqueue(runnable, tag, intake);
        }
    }
    drop(retired);

    let pop = |member: &mut UdpMember| {
        let reader = member.reader();
        let mut count = 0;
        while let Some(Captured::Datagram {
            length, payload, ..
        }) = reader.front()
        {
            assert_eq!(length, LARGE);
            assert_eq!(payload, data);
            reader.advance();
            count += 1;
        }
        count
    };
    poller.dispatch_udp_slot(&mut []);
    assert_eq!(pop(&mut first), 4);
    assert_eq!(pop(&mut next), 0);

    poller.dispatch_udp_slot(&mut []);
    assert_eq!(
        pop(&mut next),
        2,
        "the next live source starts with a fresh budget"
    );
    assert_eq!(
        pop(&mut first),
        0,
        "the fifth copy waits behind the next source"
    );

    poller.dispatch_udp_slot(&mut []);
    assert_eq!(pop(&mut first), 1);
}
