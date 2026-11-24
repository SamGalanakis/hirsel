#[test]
fn udp_receive_captures_gro_and_short_slots() {
    use crate::udp::{Captured, Options, ReceivePool, Socket, TurnBudget, TxPacket, Window};

    let full = Options::new(2, 65_792).unwrap();
    let short = Options::new(1, 1_536).unwrap();
    let loopback = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
    let sender = Socket::open(false, short).unwrap();
    let peer = sender.bind(loopback).unwrap();
    let data = [[0x41; 1200], [0x42; 1200]].concat();

    let receiver = Socket::open(false, full).unwrap();
    let to = receiver.bind(loopback).unwrap();
    let train = [
        TxPacket {
            to,
            at: 0,
            len: 1200,
            ecn: 1,
        },
        TxPacket {
            to,
            at: 1200,
            len: 1200,
            ecn: 1,
        },
    ];
    assert_eq!(sender.send_window(&data, &train).unwrap(), Window::Sent(2));
    let mut pool = ReceivePool::new(full).unwrap();
    let mut budget = TurnBudget::default();
    assert_eq!(
        pool.read(&receiver, None, &mut budget).unwrap(),
        1,
        "GRO capture"
    );
    for expected in [0x41, 0x42] {
        let Some(Captured::Datagram {
            peer: source,
            ecn,
            length,
            payload,
        }) = pool.front()
        else {
            panic!("missing segment");
        };
        assert_eq!((source, ecn, length), (peer, 1, 1200));
        assert_eq!(payload, &[expected; 1200]);
        pool.advance();
    }
    assert!(pool.front().is_none());

    for packet in train {
        assert_eq!(
            sender.send_window(&data, &[packet]).unwrap(),
            Window::Sent(1)
        );
    }
    let mut budget = TurnBudget::default();
    assert_eq!(
        pool.read(&receiver, None, &mut budget).unwrap(),
        2,
        "ordinary captures"
    );
    for expected in [0x41, 0x42] {
        let Some(Captured::Datagram { payload, .. }) = pool.front() else {
            panic!("missing ordinary datagram");
        };
        assert_eq!(payload, &[expected; 1200]);
        pool.advance();
    }

    let receiver = Socket::open(false, short).unwrap();
    let to = receiver.bind(loopback).unwrap();
    let packet = TxPacket {
        to,
        at: 0,
        len: 1500,
        ecn: 2,
    };
    assert_eq!(
        sender.send_window(&[0x61; 1500], &[packet]).unwrap(),
        Window::Sent(1)
    );
    let mut pool = ReceivePool::new(short).unwrap();
    let mut budget = TurnBudget::default();
    assert_eq!(pool.read(&receiver, None, &mut budget).unwrap(), 1);
    let Some(Captured::Datagram {
        peer: source,
        ecn,
        length,
        payload,
    }) = pool.front()
    else {
        panic!("missing short capture");
    };
    assert_eq!((source, ecn, length), (peer, 2, 1500));
    assert_eq!(payload, &[0x61; 1280]);
    pool.advance();
    assert!(pool.front().is_none());
}

#[test]
fn member_reader_never_misses_its_wake() {
    use crate::udp::{Options, member_queue};
    use std::num::NonZeroU64;
    use std::sync::atomic::{Ordering::SeqCst, fence};

    let token = NonZeroU64::new(17).unwrap();
    let peer = "127.0.0.1:1234".parse().unwrap();
    let mut schedules = 0;
    for schedule in 0..64u32 {
        if schedule.count_ones() != 3 {
            continue;
        }
        let (mut producer, mut reader) = member_queue(Options::new(1, 1536).unwrap()).unwrap();
        let (mut r, mut p) = (0, 0);
        let mut waiting = false;
        let mut parked = false;
        let mut posted = None;
        for step in 0..6 {
            if schedule & (1 << step) != 0 {
                match r {
                    0 => reader.publish_waiter(token),
                    1 => waiting = reader.recheck_waiter(),
                    2 => parked = waiting,
                    _ => unreachable!(),
                }
                r += 1;
            } else {
                match p {
                    0 => assert!(producer.push(peer, 1, 1, b"x")),
                    1 => fence(SeqCst),
                    2 => posted = producer.take_waiter(),
                    _ => unreachable!(),
                }
                p += 1;
            }
        }
        assert!(reader.front().is_some());
        assert!(
            !parked || posted == Some(token),
            "lost wake at schedule {schedule:06b}"
        );
        schedules += 1;
    }
    assert_eq!(schedules, 20);
}

#[test]
fn every_member_writer_is_woken() {
    use crate::udp::WriterList;
    use std::num::NonZeroU64;

    let writers = WriterList::new(2);
    let a = NonZeroU64::new(17).unwrap();
    let b = NonZeroU64::new(18).unwrap();
    writers.publish(0, a);
    writers.publish(1, b);
    let mut delivered = writers.take().flatten().collect::<Vec<_>>();
    delivered.sort_unstable();
    assert_eq!(delivered, [a, b]);

    writers.publish(0, a);
    writers.publish(1, b);
    writers.cancel(1);
    let visits = writers.take().collect::<Vec<_>>();
    assert_eq!(visits.len(), 2, "a timed-out node still consumes one visit");
    assert_eq!(visits.into_iter().flatten().collect::<Vec<_>>(), [a]);
}
