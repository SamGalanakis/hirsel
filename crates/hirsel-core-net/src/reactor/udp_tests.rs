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
