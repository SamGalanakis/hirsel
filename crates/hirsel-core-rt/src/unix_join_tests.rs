//! NET-7 check 3 (TL16): a Unix stream's halves never rejoin, and the
//! refusal makes no provider call.
use crate::provider::{Buffer, IoError};
use crate::provider_tests::{Fixture, config};
use crate::{net, unix_net::*};
use hirsel_core_net::GrantRule;
use std::fs::File;

#[test]
fn unix_stream_halves_never_rejoin() {
    let carrier = |data: &'static [u8]| Buffer {
        data: data.as_ptr().cast_mut(),
        len: data.len(),
        capacity: 0,
    };
    let mut files = Box::new(crate::host_file::Files::default());
    let mut f = Fixture::new();
    // SAFETY: both allocations are pinned until the fixture is dropped.
    unsafe { (*f.context).host_files = &raw mut *files };
    f.os.unix_roots
        .insert("test".into(), File::open(f.root.path()).unwrap().into());
    let provider = f.os.provider();
    let rules = net::RulesStorage::new([
        GrantRule::parse("UnixListen test door").unwrap(),
        GrantRule::parse("UnixConnect test door").unwrap(),
    ]);
    let n = files.registry.grant(provider, rules.handle());
    let mut stderr = Vec::new();
    let mut run = config(std::ptr::null(), 0, &mut stderr);
    run.provider = &provider;
    let root = carrier(b"test");
    let door = carrier(b"door");
    // SAFETY: the context, its files and the run outlive every call.
    unsafe {
        // An OS listener's accept runs on the run's provider.
        (*f.context).run = &run;
        let listener = hirsel_core_host_listen_unix(f.context, n, &root, &door, 4, 1);
        assert_eq!(listener.error, IoError::OK);
        let connected = hirsel_core_host_connect_unix(f.context, n, &root, &door, 0, 0, 0, 2);
        assert_eq!(connected.error, IoError::OK);
        let accepted = hirsel_core_host_accept_unix(f.context, listener.value, 0, 0, 0, 3);
        assert_eq!(accepted.error, IoError::OK);
        for socket in [connected.value, accepted.value] {
            let halves = net::hirsel_core_net_split(f.context, socket, 4);
            crate::os::probe::CALLS.set(Some(Vec::new()));
            let join = net::hirsel_core_net_join(f.context, halves.value, halves.written, 5);
            assert!(crate::os::probe::CALLS.take().unwrap().is_empty());
            assert_eq!(
                (join.error, join.value, join.written),
                (IoError { tag: 1, code: 0 }, halves.value, halves.written)
            );
            net::hirsel_core_socket_drop(f.context, join.value);
            net::hirsel_core_socket_drop(f.context, join.written);
        }
        net::hirsel_core_socket_drop(f.context, listener.value);
        (*f.context).run = std::ptr::null();
    }
}
