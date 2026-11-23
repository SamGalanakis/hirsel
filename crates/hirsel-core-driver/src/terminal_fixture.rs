//! A program that reports which of its standard descriptors are terminals, for tests of
//! how native builds, runs and replays see the terminals they are granted.
use crate::{Backend, CheckProduct, Driver, Meter, NativeOptions, PackageManifest, package};
use std::{
    fs::File,
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
};

pub(crate) fn product(dir: &Path) -> Arc<CheckProduct> {
    let source = r#"
pub fn main(d: Dir, i: Stdin, o: Stdout, e: Stderr) -> Result[(), Exit] waits {
let mut bits: Bytes = list.with_capacity(3)
list.push(mut bits, if stdin.is_terminal(i) { 1u8 } else { 0u8 })
list.push(mut bits, if stdout.is_terminal(o) { 1u8 } else { 0u8 })
list.push(mut bits, if stderr.is_terminal(e) { 1u8 } else { 0u8 })
let mut f = result.unwrap(host.open(d, "bits", CreateNew))
result.unwrap(host.write(mut f, bits))
Ok(())
}
"#;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/core");
    let mut request = crate::tests::request(source, dir);
    request.entry = Some(crate::DefPath::from_full("app@1/main/main").unwrap());
    request.inputs.packages[0]
        .dependencies
        .push(package("core"));
    request
        .inputs
        .packages
        .push(PackageManifest::directory(&root, package("core"), true).unwrap());
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    product
}

pub(crate) fn options(backend: Backend, dir: &Path) -> NativeOptions {
    let mut options = crate::native::tests::native_options(backend, dir);
    for name in ["Stdin", "Stdout", "Stderr"] {
        options.grant_mask |= 1 << crate::grant_kind(name).unwrap();
    }
    options.directories.push(dir.to_str().unwrap().into());
    options
}

pub(crate) fn pty() -> (OwnedFd, OwnedFd) {
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: the two result slots are distinct and live. Null selects
    // the default terminal settings and skips the device name.
    unsafe {
        assert_eq!(
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            ),
            0,
            "{}",
            std::io::Error::last_os_error(),
        );
        (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave))
    }
}

#[test]
fn terminal_bits_follow_each_granted_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let bits = dir.path().join("bits");
    let entry = crate::DefPath::from_full("app@1/main/main").unwrap();
    let product = product(dir.path());
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let program = product
            .build(&entry, &options(backend, dir.path()))
            .unwrap();
        let (_master, slave) = pty();
        let first = Command::new(&program.executable)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert!(first.status.success(), "{:?}: {:?}", backend, first.stderr);
        assert_eq!(std::fs::read(&bits).unwrap(), [1, 1, 0]);
        std::fs::remove_file(&bits).unwrap();

        let second = Command::new(&program.executable)
            .stdin(File::open("/dev/null").unwrap())
            .stdout(File::create(dir.path().join("stdout")).unwrap())
            .stderr(Stdio::from(slave))
            .status()
            .unwrap();
        assert!(second.success(), "{backend:?}: {second}");
        assert_eq!(std::fs::read(&bits).unwrap(), [0, 0, 1]);
        std::fs::remove_file(&bits).unwrap();

        let mut closed = Command::new(&program.executable);
        closed
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // SAFETY: change only the child's inherited stdout descriptor.
        unsafe {
            closed.pre_exec(|| {
                libc::close(libc::STDOUT_FILENO);
                Ok(())
            });
        }
        let closed = closed.output().unwrap();
        assert!(closed.status.success(), "{backend:?}: {:?}", closed.stderr);
        assert_eq!(std::fs::read(&bits).unwrap(), [0, 0, 0]);
        std::fs::remove_file(&bits).unwrap();
    }
}
