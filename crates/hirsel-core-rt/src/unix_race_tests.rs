//! NET-13: a racing writer cannot redirect a Unix connect.
use crate::provider::{Buffer, IoError};
use crate::provider_tests::Fixture;
use std::fs::File;

/// A connect resolves its socket beneath the root and reaches only that socket
/// (docs/spec/net.md, NET-13). A writer to the confined directory swaps the
/// slot between the socket and a symbolic link to a listener outside the root
/// as fast as it can while the program connects: no connect may reach the
/// outside listener, and every connect that succeeds reaches the socket.
#[test]
fn unix_connect_cannot_be_redirected_by_a_racing_rename() {
    use crate::{net, unix_net::*};
    use hirsel_core_net::GrantRule;
    use std::{
        fs,
        os::unix::{fs::symlink, net::UnixListener},
        sync::atomic::{AtomicBool, Ordering},
    };

    crate::openat2_required::require();
    let mut files = Box::new(crate::host_file::Files::default());
    let mut f = Fixture::new();
    // SAFETY: both allocations are pinned until the fixture is dropped.
    unsafe { (*f.context).host_files = &raw mut *files };
    // The executor's TMPDIR can exceed the kernel's Unix socket path limit.
    let sockets = tempfile::tempdir_in("/tmp").unwrap();
    let dir = sockets.path().join("confined");
    fs::create_dir(&dir).unwrap();
    let outside_path = sockets.path().join("outside");
    let outside = UnixListener::bind(&outside_path).unwrap();
    outside.set_nonblocking(true).unwrap();
    let slot = dir.join("slot");
    let inside = UnixListener::bind(&slot).unwrap();
    inside.set_nonblocking(true).unwrap();
    let (spare, link) = (dir.join("spare"), dir.join("link"));
    fs::hard_link(&slot, &spare).unwrap();
    symlink(&outside_path, &link).unwrap();
    f.os.unix_roots
        .insert("test".into(), File::open(&dir).unwrap().into());
    let provider = f.os.provider();
    let rules = net::RulesStorage::new([GrantRule::parse("UnixConnect test slot").unwrap()]);
    let n = files.registry.grant(provider, rules.handle());
    let root = Buffer {
        data: b"test".as_ptr().cast_mut(),
        len: 4,
        capacity: 0,
    };
    let path = Buffer {
        data: b"slot".as_ptr().cast_mut(),
        len: 4,
        capacity: 0,
    };
    let stop = AtomicBool::new(false);
    let (mut reached, mut refused) = (0, 0);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                // The slot holds the link, then the socket again.
                let _ = fs::rename(&link, &slot);
                let _ = fs::rename(&spare, &slot);
                let _ = fs::hard_link(&slot, &spare);
                let _ = symlink(&outside_path, &link);
            }
        });
        // Stop the racer also when an assertion below unwinds.
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let _stop = Stop(&stop);
        for _ in 0..20_000 {
            // SAFETY: the context, grant and carriers live through the call.
            let out =
                unsafe { hirsel_core_host_connect_unix(f.context, n, &root, &path, 0, 0, 0, 2) };
            assert!(
                outside.accept().is_err(),
                "a connect reached the listener outside the root"
            );
            if out.error == IoError::OK {
                assert!(inside.accept().is_ok(), "a connect reached no listener");
                reached += 1;
                // SAFETY: the connect delivered this socket key.
                unsafe { net::hirsel_core_socket_drop(f.context, out.value) };
            } else {
                refused += 1;
            }
        }
    });
    assert!(
        reached > 0 && refused > 0,
        "{reached} reached, {refused} refused"
    );
    let left: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| !["slot", "spare", "link"].contains(&name.to_str().unwrap()))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}
