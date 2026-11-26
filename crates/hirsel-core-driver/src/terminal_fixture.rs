//! A program that reports which of its standard descriptors are terminals, for tests of
//! how native builds, runs and replays see the terminals they are granted.
use crate::{Backend, CheckProduct, Driver, Meter, NativeOptions, PackageManifest, package};
use std::{
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
};

pub(crate) fn product(dir: &Path) -> Arc<CheckProduct> {
    product_of(dir, BITS)
}

const BITS: &str = r#"
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

/// Writes `sizes` as "<stdout> <stderr>", each "COLSxROWS" or "none".
const SIZES: &str = r#"
pub fn main(d: Dir, i: Stdin, o: Stdout, e: Stderr) -> Result[(), Exit] waits {
let _ = stdin.is_terminal(i)
let mut s = string.new()
push_size(mut s, stdout.size(o))
string.push(mut s, " ")
push_size(mut s, stderr.size(e))
let mut f = result.unwrap(host.open(d, "sizes", CreateNew))
result.unwrap(host.write(mut f, string.into_bytes(take s)))
Ok(())
}

fn push_size(mut s: String, size: Option[TermSize]) {
match size {
Some(TermSize(cols: c, rows: r)) => {
let cols: u64 = int.widen(c)
let rows: u64 = int.widen(r)
string.push_u64(mut s, cols)
string.push(mut s, "x")
string.push_u64(mut s, rows)
},
None => string.push(mut s, "none"),
}
}
"#;

pub(crate) fn product_of(dir: &Path, source: &str) -> Arc<CheckProduct> {
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
                std::ptr::null_mut(),
                std::ptr::null_mut(),
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

/// A pty whose window is `cols` by `rows`, set on the master as a terminal
/// emulator does.
fn sized_pty(cols: u16, rows: u16) -> (OwnedFd, OwnedFd) {
    let (master, slave) = pty();
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ reads one winsize from the live struct.
    let rc = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) };
    assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
    (master, slave)
}

fn sizes(dir: &Path) -> String {
    let path = dir.join("sizes");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    text
}

/// TERM_SIZE frames in a sealed tape, issued or answered.
fn size_frames(tape: &Path) -> usize {
    use std::io::Read;
    let sealed = std::fs::read(tape).unwrap();
    let mut input = sealed.as_slice();
    let (mut blocks, _) = hirsel_tape_format::Blocks::open(|buffer| input.read(buffer)).unwrap();
    let mut decoder = hirsel_tape_format::Decoder::default();
    let mut count = 0;
    while let Some((flags, payload)) = blocks.next(|buffer| input.read(buffer)).unwrap() {
        use hirsel_tape_format::{Frame, op};
        for frame in decoder
            .block(payload, flags & hirsel_tape_format::END != 0)
            .unwrap()
        {
            if matches!(frame, Frame::Op { op, .. } | Frame::Issue { op, .. } if op == op::TERM_SIZE)
            {
                count += 1;
            }
        }
    }
    count
}

// A size law (FABLE-RULING R4): a pty set to 132x43 reports 132 and 43 on both
// streams, and a pipe reports none.
#[test]
fn terminal_size_follows_each_granted_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let entry = crate::DefPath::from_full("app@1/main/main").unwrap();
    let product = product_of(dir.path(), SIZES);
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let program = product
            .build(&entry, &options(backend, dir.path()))
            .unwrap();
        let (_master, slave) = sized_pty(132, 43);
        let on_pty = Command::new(&program.executable)
            .stdin(Stdio::null())
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave))
            .status()
            .unwrap();
        assert!(on_pty.success(), "{backend:?}: {on_pty}");
        assert_eq!(sizes(dir.path()), "132x43 132x43", "{backend:?}");

        let piped = Command::new(&program.executable)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert!(piped.status.success(), "{backend:?}: {:?}", piped.stderr);
        assert_eq!(sizes(dir.path()), "none none", "{backend:?}");
    }
}

// A size law (R4): a run recorded on a pty replays on pipes with the recorded
// sizes and ends Sealed, and a piped run's recording holds no size frame.
#[test]
fn terminal_size_replays_from_the_tape_and_a_pipe_records_none() {
    let dir = tempfile::tempdir().unwrap();
    let entry = crate::DefPath::from_full("app@1/main/main").unwrap();
    let product = product_of(dir.path(), SIZES);
    let mut options = options(Backend::Cranelift, dir.path());
    options.runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(std::env::var_os("HIRSEL_CORE_RT_RECORD_ARCHIVE").expect("recording runtime archive"))
        .canonicalize()
        .unwrap();
    options.recording = true;
    let record = product.build(&entry, &options).unwrap();

    let tape = dir.path().join("sized.tape");
    let (_master, slave) = sized_pty(132, 43);
    let output = Command::new(&record.executable)
        .env("HIRSEL_RECORD", &tape)
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(sizes(dir.path()), "132x43 none");
    // The pty's query is the one size frame; the piped stderr records none.
    assert_eq!(size_frames(&tape), 1);

    let piped_tape = dir.path().join("piped.tape");
    let output = Command::new(&record.executable)
        .env("HIRSEL_RECORD", &piped_tape)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(sizes(dir.path()), "none none");
    assert_eq!(size_frames(&piped_tape), 0);

    options.grant_mask = 0;
    options.directories.clear();
    options.replay = Some(crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: true,
    });
    let replay = product.build(&entry, &options).unwrap();
    // On pipes a live query answers none, so the recorded write of
    // "132x43 none" would diverge unless the size comes from the tape.
    let output = Command::new(replay.executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{err}");
    // A replay that ends short of Sealed names its fault on a "replay: " line.
    assert!(!err.contains("replay: "), "{err}");
    assert!(!dir.path().join("sizes").exists());
}
