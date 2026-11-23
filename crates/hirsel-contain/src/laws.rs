use super::*;

#[test]
fn contained_refused_uncontainable() {
    let state = tempfile::tempdir().unwrap();
    // Not a cgroup v2 directory.
    assert!(matches!(
        Host::open(state.path(), state.path()),
        Err(Uncontainable::NotCgroup2)
    ));
    // A cgroup this user was not delegated.
    if unsafe { libc::getuid() } != 0 {
        assert!(matches!(
            Host::open(Path::new("/sys/fs/cgroup"), state.path()),
            Err(Uncontainable::Controllers(_))
        ));
    }
}
