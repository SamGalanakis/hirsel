use super::Fixture;
use crate::provider::{Buffer, IoError};

pub(super) fn check(swapped: IoError, replaced: IoError) {
    crate::openat2_required::require();
    use crate::{net, os_unix, unix_net::*};
    use hirsel_core_net::{GrantRule, RuleView};
    use std::{
        fs::{self, File, OpenOptions},
        os::unix::{
            fs::{FileTypeExt, symlink},
            net::UnixListener,
        },
        path::Path,
    };

    fn unix_fixture() -> (Box<crate::host_file::Files>, Fixture) {
        let mut files = Box::new(crate::host_file::Files::default());
        let fixture = Fixture::new();
        // SAFETY: both allocations are pinned until the fixture is dropped.
        unsafe { (*fixture.context).host_files = &raw mut *files };
        (files, fixture)
    }

    fn carrier(data: &[u8]) -> Buffer {
        Buffer {
            data: data.as_ptr().cast_mut(),
            len: data.len(),
            capacity: 0,
        }
    }
    fn is_socket(path: &Path) -> bool {
        fs::symlink_metadata(path).unwrap().file_type().is_socket()
    }
    let error = |tag, code| IoError { tag, code };
    let root = carrier(b"test");
    // Each case acquires a different directory and checks only its own nodes.
    let (mut files, mut f) = unix_fixture();
    let dir = f.root.path();
    let clean = dir.join("real");
    fs::create_dir(&clean).unwrap();
    symlink(&clean, dir.join("alias")).unwrap();
    fs::write(dir.join("file"), b"unchanged").unwrap();
    symlink("missing", dir.join("link")).unwrap();
    let existing = UnixListener::bind(dir.join("socket")).unwrap();
    f.os.unix_roots
        .insert("test".into(), File::open(dir).unwrap().into());
    let provider = f.os.provider();
    let rules = net::RulesStorage::new(
        [
            "alias/new",
            "file/new",
            "file",
            "real",
            "link",
            "socket",
            "partial",
            "created",
        ]
        .into_iter()
        .map(|path| GrantRule::parse(&format!("UnixListen test {path}")).unwrap()),
    );
    let n = files.registry.grant(provider, rules.handle());
    for path in ["alias/new", "file/new"] {
        let out = unsafe {
            hirsel_core_host_listen_unix(f.context, n, &root, &carrier(path.as_bytes()), 4, 1)
        };
        assert_eq!(out.error, error(1, 0), "{path}");
    }
    assert!(!clean.join("new").exists());
    for path in ["file", "real", "link", "socket"] {
        let out = unsafe {
            hirsel_core_host_listen_unix(f.context, n, &root, &carrier(path.as_bytes()), 4, 1)
        };
        assert_eq!(out.error, error(4, 0), "{path}");
    }
    assert_eq!(fs::read(dir.join("file")).unwrap(), b"unchanged");
    assert_eq!(
        fs::read_link(dir.join("link")).unwrap(),
        Path::new("missing")
    );
    assert!(clean.is_dir());
    assert!(is_socket(&dir.join("socket")));
    let mut n_op = UnixRequest {
        os: &os_unix::OPERATIONS,
        root: "test",
        path: "partial",
        backlog: 4,
        authorized: true,
        until: None,
        sockets: &raw mut files.sockets,
        connecting: None,
        target: None,
        retry: None,
    };
    let out = os_unix::bound(&f.os, &n_op, &mut files.sockets, true, |_, _| {
        Err(libc::EINVAL)
    });
    assert_eq!(out.error, error(10, libc::EINVAL as u32));
    assert!(is_socket(&dir.join("partial")));
    n_op.path = "alias/early";
    let out = os_unix::bound(&f.os, &n_op, &mut files.sockets, true, |_, _| {
        panic!("listen after a failed bind")
    });
    assert_eq!(out.error, error(1, 0));
    assert!(!clean.join("early").exists());
    let out =
        unsafe { hirsel_core_host_listen_unix(f.context, n, &root, &carrier(b"created"), 4, 1) };
    assert_eq!(out.error, IoError::OK);
    unsafe { net::hirsel_core_socket_drop(f.context, out.value) };
    assert!(is_socket(&dir.join("created")));
    drop(existing);

    let (files, mut f) = unix_fixture();
    let dir = f.root.path();
    let slot = dir.join("slot");
    let old = UnixListener::bind(&slot).unwrap();
    old.set_nonblocking(true).unwrap();
    let next_path = dir.join("next");
    let next = UnixListener::bind(&next_path).unwrap();
    next.set_nonblocking(true).unwrap();
    let saved = dir.join("saved");
    let old_path = slot.clone();
    os_unix::AFTER_RESOLVE.with_borrow_mut(|hook| {
        *hook = Some(Box::new(move || {
            fs::rename(&old_path, saved).unwrap();
            fs::rename(next_path, old_path).unwrap();
        }));
    });
    f.os.unix_roots
        .insert("test".into(), File::open(dir).unwrap().into());
    let provider = f.os.provider();
    let rules = net::RulesStorage::new([GrantRule::parse("UnixConnect test slot").unwrap()]);
    let n = files.registry.grant(provider, rules.handle());
    let connect = || unsafe {
        hirsel_core_host_connect_unix(f.context, n, &root, &carrier(b"slot"), 0, 0, 0, 2)
    };
    let first = connect();
    assert_eq!(first.error, swapped);
    assert_eq!(old.accept().is_ok(), swapped == IoError::OK);
    assert_eq!(
        next.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let second = connect();
    assert_eq!(second.error, IoError::OK);
    assert!(next.accept().is_ok());
    assert!(is_socket(&slot));
    assert!(is_socket(&dir.join("saved")));
    let replacement = dir.join("replacement");
    let new = UnixListener::bind(&replacement).unwrap();
    new.set_nonblocking(true).unwrap();
    let old_path = slot.clone();
    os_unix::AFTER_RESOLVE.with_borrow_mut(|hook| {
        *hook = Some(Box::new(move || {
            drop(next);
            fs::rename(replacement, old_path).unwrap();
        }));
    });
    assert_eq!(connect().error, replaced);
    assert_eq!(
        new.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(is_socket(&slot));
    unsafe {
        for connected in [first, second] {
            if connected.error == IoError::OK {
                net::hirsel_core_socket_drop(f.context, connected.value);
            }
        }
    }

    let (files, mut f) = unix_fixture();
    let dir = f.root.path();
    f.os.unix_roots.insert(
        "test".into(),
        OpenOptions::new().read(true).open(dir).unwrap().into(),
    );
    let provider = f.os.provider();
    let rules = net::RulesStorage::new([
        GrantRule::parse("UnixBind test queue").unwrap(),
        GrantRule::parse("UnixSend test queue").unwrap(),
    ]);
    let ip = [RuleView::address(
        hirsel_core_net::Rule::parse("Connect 0.0.0.0/0 0-65535").unwrap(),
    )];
    let ip_only = unsafe { net::RulesStorage::from_views(&ip) }.unwrap();
    let n = files.registry.grant(provider, rules.handle());
    let ip_only = files.registry.grant(provider, ip_only.handle());
    let context = f.context;
    let path = carrier(b"queue");
    unsafe {
        let unbound = hirsel_core_host_open_datagram(context, ip_only, 3);
        assert_eq!(unbound.error, IoError::OK);
        let mut bytes = carrier(b"unchanged");
        crate::os::probe::CALLS.with_borrow_mut(|calls| *calls = Some(Vec::new()));
        assert_eq!(
            hirsel_core_host_recv_datagram(context, unbound.value, &mut bytes, 0, 0, 0, 3).error,
            error(1, 0)
        );
        assert_eq!(
            hirsel_core_host_send_datagram(context, unbound.value, &bytes, 0, 0, 0, 3).error,
            error(1, 0)
        );
        assert_eq!(
            hirsel_core_host_send_to(context, unbound.value, &root, &path, &bytes, 0, 0, 0, 3)
                .error,
            error(1, 0)
        );
        assert!(
            crate::os::probe::CALLS
                .with_borrow_mut(Option::take)
                .unwrap()
                .is_empty()
        );
        assert!(!dir.join("queue").exists());
        net::hirsel_core_socket_drop(context, unbound.value);
        let rx = hirsel_core_host_bind_datagram(context, n, &root, &path, 3);
        let tx = hirsel_core_host_open_datagram(context, n, 3);
        assert_eq!((rx.error, tx.error), (IoError::OK, IoError::OK));
        assert!(is_socket(&dir.join("queue")));
        assert_eq!(
            hirsel_core_host_connect_datagram(context, tx.value, &root, &path, 3).error,
            IoError::OK
        );
        let mut backing = [b'!'; 8];
        for (data, cap, expected_len, expected_cut, prefix) in [
            (b"abcdef".as_slice(), 3, 6, 1, b"!ab".as_slice()),
            (b"xy".as_slice(), 1, 2, 1, b"!".as_slice()),
            (b"".as_slice(), 1, 0, 0, b"!".as_slice()),
            (b"q".as_slice(), 8, 1, 0, b"!q".as_slice()),
        ] {
            backing.fill(b'!');
            bytes = Buffer {
                data: backing.as_mut_ptr(),
                len: 1,
                capacity: cap,
            };
            assert_eq!(
                hirsel_core_host_send_datagram(context, tx.value, &carrier(data), 0, 0, 0, 3).error,
                IoError::OK
            );
            let out = hirsel_core_host_recv_datagram(context, rx.value, &mut bytes, 0, 0, 0, 3);
            assert_eq!(
                (out.error, out.value, out.written),
                (IoError::OK, expected_len, expected_cut)
            );
            assert_eq!(bytes.len, prefix.len());
            assert_eq!(&backing[..bytes.len], prefix);
        }
        for handle in [tx.value, rx.value] {
            net::hirsel_core_socket_drop(context, handle);
        }
        assert!(is_socket(&dir.join("queue")));
        assert_eq!(
            hirsel_core_host_bind_datagram(context, n, &root, &path, 3).error,
            error(4, 0)
        );
    }
}
