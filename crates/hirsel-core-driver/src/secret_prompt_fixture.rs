//! The laws of `stdin.read_secret` (core/std/api/stdin.md, Secret input laws).
//! Each runs a native program on the slave of a pty from `openpty` and
//! compares the pty's modes after the run with the modes before it.
use crate::{
    Backend, NativeOptions,
    terminal_fixture::{options, product_of, pty},
};
use std::{
    ffi::CString,
    fs::File,
    io::Write,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::{ffi::OsStrExt, process::CommandExt, process::ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// Reads one secret and writes `out`: the 32-byte `hmac_sha256(secret, "data")`
/// on success, else the error's number. After `NotTerminal` it reads stdin
/// again and appends what it got.
const PROMPT: &str = r#"
pub fn main(d: Dir, i: Stdin, PARAMS) -> Result[(), Exit] waits {
PRELUDE
let mut out: Bytes = list.with_capacity(64)
let mut rest: Bytes = list.with_capacity(16)
let r = stdin.read_secret(i, "mac", UNTIL)
match take r {
Ok(s) => {
let tag = result.unwrap(secret.hmac_sha256(s, string.as_bytes("data")))
if !bytebuf.append_inline(mut out, tag, 0, 32) { return Error(Exit(status: 5)) }
},
Error(e) => {
let n = code(e)
list.push(mut out, n)
if n == 1u8 { let _ = result.unwrap(stdin.read(i, mut rest, Never)) }
},
}
let mut f = result.unwrap(host.open(d, "out", CreateNew))
result.unwrap(host.write(mut f, out))
result.unwrap(host.write(mut f, rest))
Ok(())
}

fn code(e: stdin.SecretReadError) -> u8 {
match e {
NotTerminal => 1u8,
Busy => 2u8,
Empty => 3u8,
TooLong => 4u8,
Eof => 5u8,
Aborted => 6u8,
TimedOut => 7u8,
Policy(_) => 8u8,
Other(_) => 9u8,
}
}

fn deep() -> u64 { deep() + 1u64 }

fn crash(take c: Clock) -> u64 waits {
host.sleep_until(c, host.mono_ns(c) + 500000000u64)
deep()
}
"#;

/// Reads in a task that main cancels once `host.stopped` returns, and
/// writes the task's error number.
const CANCELLED: &str = r#"
fn reader(take i: Stdin) -> u8 waits {
let r = stdin.read_secret(i, "mac", Never)
match take r {
Ok(_) => 0u8,
Error(Aborted) => 6u8,
Error(_) => 9u8,
}
}

pub fn main(d: Dir, i: Stdin, p: Process, tasks: Tasks) -> Result[(), Exit] waits {
let mut h = task.spawn(tasks, bytes 65536, i, reader)
host.stopped(p)
task.cancel(mut h)
let mut out: Bytes = list.with_capacity(1)
match task.join(take h) {
Ok(n) => list.push(mut out, n),
Error(Failed(take causes)) => { let _ = take causes; list.push(mut out, 254u8) },
Error(Cancelled(take causes)) => { let _ = take causes; list.push(mut out, 255u8) },
}
let mut f = result.unwrap(host.open(d, "out", CreateNew))
result.unwrap(host.write(mut f, out))
Ok(())
}
"#;

/// A secret read in task `a`, with a plain `stdin.read` in task `b` queued
/// behind it. Main runs STOP, then writes `a`'s error number and what `b`
/// read.
const QUEUED: &str = r#"
type Reader: send {
i: Stdin,
c: Clock
}

fn secret_read(take reader: Reader) -> u8 waits {
let Reader(i: i, c: c) = take reader
let r = stdin.read_secret(i, "mac", UNTIL)
match take r {
Ok(_) => 0u8,
Error(TooLong) => 4u8,
Error(Aborted) => 6u8,
Error(TimedOut) => 7u8,
Error(_) => 9u8,
}
}

fn plain_read(take reader: Reader) -> Bytes waits {
let Reader(i: i, c: c) = take reader
host.sleep_until(c, host.mono_ns(c) + 300000000u64)
let mut got: Bytes = list.with_capacity(4096)
let _ = result.unwrap(stdin.read(i, mut got, Never))
got
}

pub fn main(d: Dir, i: Stdin, c: Clock, tasks: TasksPARAMS) -> Result[(), Exit] waits {
let mut a = task.spawn(tasks, bytes 65536, Reader(i: i, c: c), secret_read)
let b = task.spawn(tasks, bytes 65536, Reader(i: i, c: c), plain_read)
STOP
let mut out: Bytes = list.with_capacity(1)
match task.join(take a) {
Ok(n) => list.push(mut out, n),
Error(Failed(take causes)) => { let _ = take causes; list.push(mut out, 254u8) },
Error(Cancelled(take causes)) => { let _ = take causes; list.push(mut out, 255u8) },
}
let got: Bytes = match task.join(take b) {
Ok(take line) => take line,
Error(Failed(take causes)) => { let _ = take causes; list.push(mut out, 254u8); list.with_capacity(1) },
Error(Cancelled(take causes)) => { let _ = take causes; list.push(mut out, 255u8); list.with_capacity(1) },
}
let mut f = result.unwrap(host.open(d, "out", CreateNew))
result.unwrap(host.write(mut f, out))
result.unwrap(host.write(mut f, got))
Ok(())
}
"#;

const ENTRY: &str = "app@1/main/main";

/// HMAC-SHA256 with key `hunter2` over `data` (RFC 2104), computed outside
/// Hirsel: `printf data | openssl dgst -sha256 -hmac hunter2`.
const HUNTER2_TAG: &str = "a53133580d1aae5958211b7e1cffa11b030073eb7f9021238f6631dc22c7f86a";

struct Built {
    dir: tempfile::TempDir,
    executable: PathBuf,
}

fn prompt(params: &str, prelude: &str, until: &str) -> String {
    PROMPT
        .replace("PARAMS", params)
        .replace("PRELUDE", prelude)
        .replace("UNTIL", until)
}

/// Grants the directory, `Stdin` and `grants`, one for each parameter of main.
fn granted(backend: Backend, dir: &Path, grants: &[&str]) -> NativeOptions {
    let mut options = options(backend, dir);
    options.grant_mask = 0;
    for name in ["Stdin"].iter().chain(grants) {
        options.grant_mask |= 1 << crate::grant_kind(name).unwrap();
    }
    options
}

fn build(source: &str, backend: Backend, grants: &[&str]) -> Built {
    let dir = tempfile::tempdir().unwrap();
    let product = product_of(dir.path(), source);
    let entry = crate::DefPath::from_full(ENTRY).unwrap();
    let program = product
        .build(&entry, &granted(backend, dir.path(), grants))
        .unwrap();
    Built {
        executable: program.executable.clone(),
        dir,
    }
}

impl Built {
    fn out(&self) -> Vec<u8> {
        let path = self.dir.path().join("out");
        let out = std::fs::read(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        out
    }
}

/// The modes a law compares.
#[derive(Debug, PartialEq)]
struct Modes {
    iflag: libc::tcflag_t,
    oflag: libc::tcflag_t,
    cflag: libc::tcflag_t,
    lflag: libc::tcflag_t,
    cc: Vec<libc::cc_t>,
}

fn termios(fd: i32) -> libc::termios {
    // SAFETY: tcgetattr fills the live termios.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        assert_eq!(
            libc::tcgetattr(fd, &mut t),
            0,
            "{}",
            std::io::Error::last_os_error()
        );
        t
    }
}

fn modes(fd: &OwnedFd) -> Modes {
    let t = termios(fd.as_raw_fd());
    Modes {
        iflag: t.c_iflag,
        oflag: t.c_oflag,
        cflag: t.c_cflag,
        lflag: t.c_lflag,
        cc: t.c_cc.to_vec(),
    }
}

/// Resets the signals a law depends on to their default actions in the child,
/// so a harness that ignores one does not decide the law, and turns off cores.
fn defaults(command: &mut Command) -> &mut Command {
    // SAFETY: signal and setrlimit are async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            reset_child();
            Ok(())
        })
    }
}

/// # Safety
/// Async-signal-safe; called between fork and exec only.
unsafe fn reset_child() {
    // SAFETY: as above.
    unsafe {
        for signal in [
            libc::SIGHUP,
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGTERM,
            libc::SIGTTOU,
            libc::SIGTTIN,
        ] {
            libc::signal(signal, libc::SIG_DFL);
        }
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut none);
        libc::sigprocmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
        let zero = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        libc::setrlimit(libc::RLIMIT_CORE, &zero);
    }
}

fn spawn(built: &Built, slave: &OwnedFd) -> Child {
    defaults(&mut Command::new(&built.executable))
        .stdin(slave.try_clone().unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

const PATIENCE: Duration = Duration::from_secs(60);

/// Waits until the read has armed: echo is off on the pty.
fn armed(slave: &OwnedFd, child: &mut Child) {
    let start = Instant::now();
    while termios(slave.as_raw_fd()).c_lflag & libc::ECHO != 0 {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("the program ended before the read armed: {status}");
        }
        assert!(start.elapsed() < PATIENCE, "the read never armed");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn signal(child: &Child, signal: i32) {
    // SAFETY: the child is this test's own, not yet reaped.
    assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, signal) }, 0);
}

fn finish(child: Child) -> (std::process::ExitStatus, String) {
    let output = child.wait_with_output().unwrap();
    (
        output.status,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Waits for the program to end and gives what its terminal wrote to the
/// master meanwhile. The read's restore drains the terminal's output first
/// (`TCSAFLUSH`), and a Darwin pty's output has drained only once the master
/// has read it, so a law that waited without reading would wait for ever.
fn finish_on(mut child: Child, master: &OwnedFd) -> (std::process::ExitStatus, String, Vec<u8>) {
    let start = Instant::now();
    let mut written = Vec::new();
    let status = loop {
        written.extend(master_output(master));
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() >= PATIENCE {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("the program never ended");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    written.extend(master_output(master));
    let mut err = Vec::new();
    std::io::Read::read_to_end(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    (status, String::from_utf8_lossy(&err).into_owned(), written)
}

/// What the program's terminal has written to the master side, read without
/// waiting.
fn master_output(master: &OwnedFd) -> Vec<u8> {
    let fd = master.as_raw_fd();
    // SAFETY: fcntl on a live descriptor this test owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut all = Vec::new();
    let mut buffer = [0u8; 256];
    loop {
        // SAFETY: the buffer is live and writable for its length.
        let n = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if n <= 0 {
            return all;
        }
        all.extend_from_slice(&buffer[..n as usize]);
    }
}

/// Types `bytes` on the master, waiting while the terminal's input is full: a
/// Darwin pty holds about 1 KiB until the program reads, and `master_output`
/// leaves the master nonblocking.
fn type_on(master: &OwnedFd, bytes: &[u8]) {
    let fd = master.as_raw_fd();
    // SAFETY: fcntl on a live descriptor this test owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
    }
    File::from(master.try_clone().unwrap())
        .write_all(bytes)
        .unwrap();
}

// cli@1's Prompts contract, via the real terminal port. The first CR in CRCRLF
// remains part of the line. An empty line and terminal EOF both decline.
#[test]
fn cli_prompts_follow_the_terminal_contract() {
    const SOURCE: &str = r#"
fn expect(ok: bool, take message: String) {
    if !ok { fail(take message) }
}

pub fn main(d: Dir, i: Stdin, e: Stderr) -> Result[(), Exit] waits {
    let Some(take s) = result.unwrap(cli.prompt_secret(i, e, "Secret: ", "mac", false)) else {
        fail("secret prompt returned EOF")
    }
    let tag = result.unwrap(secret.hmac_sha256(s, string.as_bytes("data")))
    let mut payload: Bytes = list.with_capacity(32u64)
    expect(bytebuf.append_inline(mut payload, tag, 0, 32), "secret tag did not fit")
    let mut out = result.unwrap(host.open(d, "out", CreateNew))
    result.unwrap(host.write(mut out, payload))

    let mut reader = cli.prompt_input(i, 128u64)
    let Some(take line) = result.unwrap(cli.prompt_line(mut reader, e, "Line: ", false)) else {
        fail("line prompt returned EOF")
    }
    expect(line == "line\r", "the line prompt did not strip exactly one CRLF")
    expect(!result.unwrap(cli.confirm(mut reader, e, "Empty?", false, false)), "empty input confirmed")
    expect(result.unwrap(cli.confirm(mut reader, e, "Yes?", false, false)), "yes did not confirm")
    expect(!result.unwrap(cli.confirm(mut reader, e, "EOF?", false, false)), "EOF confirmed")
    Ok(())
}
"#;
    use crate::{CheckRequest, Driver, Meter, package, package_dir::PackageDir};

    let dir = tempfile::tempdir().unwrap();
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let mut request = CheckRequest::libraries(&libraries, &dir.path().join("store")).unwrap();
    let cli = PackageDir::open(&libraries.join("programs/cli"))
        .unwrap()
        .package;
    let mut app = crate::tests::request(SOURCE, dir.path())
        .inputs
        .packages
        .remove(0);
    app.dependencies = vec![package("core"), package("cli")];
    request.inputs.packages.extend([cli, app]);
    let entry = crate::DefPath::from_full(ENTRY).unwrap();
    request.entry = Some(entry.clone());
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let program = product
        .build(
            &entry,
            &granted(Backend::Cranelift, dir.path(), &["Stderr"]),
        )
        .unwrap();
    let built = Built {
        executable: program.executable,
        dir,
    };

    let (master, slave) = pty();
    let mut settings = termios(slave.as_raw_fd());
    settings.c_iflag &= !libc::ICRNL;
    // SAFETY: a live descriptor and its initialized terminal settings.
    assert_eq!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &settings) },
        0
    );
    let before = modes(&slave);
    let mut child = spawn(&built, &slave);
    armed(&slave, &mut child);
    type_on(&master, b"hunter2\n");
    // Read the terminal output while the secret port restores canonical input.
    // Darwin waits for the master to drain its output before restoring modes.
    let start = Instant::now();
    let mut secret_output = Vec::new();
    while modes(&slave) != before {
        secret_output.extend(master_output(&master));
        assert!(
            start.elapsed() < PATIENCE,
            "the secret read never restored the terminal"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    secret_output.extend(master_output(&master));
    assert_eq!(secret_output, b"\r\n");
    type_on(&master, b"line\r\r\n\n YeS \n");
    type_on(&master, &[settings.c_cc[libc::VEOF]]);
    let (status, err, _) = finish_on(child, &master);
    assert!(status.success(), "{status} {err}");
    let tag: String = built.out().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(tag, HUNTER2_TAG);
    assert_eq!(err, "Secret: Line: Empty? [y/N] Yes? [y/N] EOF? [y/N] ");
}

// Law A: the typed line becomes a secret whose tag is the hand-computed one,
// and the master sees no typed byte and exactly one newline.
#[test]
fn a_line_becomes_a_secret_and_is_not_echoed() {
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let built = build(&prompt("", "", "Never"), backend, &[]);
        let (master, slave) = pty();
        let before = modes(&slave);
        let mut child = spawn(&built, &slave);
        armed(&slave, &mut child);
        type_on(&master, b"hunter2\n");
        let (status, err, written) = finish_on(child, &master);
        assert!(status.success(), "{backend:?}: {status} {err}");
        let tag: String = built.out().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(tag, HUNTER2_TAG, "{backend:?}");
        // ECHONL echoes the Enter; output processing makes it "\r\n".
        assert_eq!(written, b"\r\n", "{backend:?}");
        assert_eq!(modes(&slave), before, "{backend:?}");
    }

    // A terminal left in non-canonical mode still gives the whole line: the
    // first byte, read alone, is not the secret, and the modes come back raw.
    let built = build(&prompt("", "", "Never"), Backend::Cranelift, &[]);
    let (master, slave) = pty();
    let mut raw = termios(slave.as_raw_fd());
    raw.c_lflag &= !libc::ICANON;
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    // SAFETY: a live descriptor and an initialized termios.
    assert_eq!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &raw) },
        0
    );
    let before = modes(&slave);
    let mut child = spawn(&built, &slave);
    armed(&slave, &mut child);
    type_on(&master, b"h");
    std::thread::sleep(Duration::from_millis(200));
    type_on(&master, b"unter2\n");
    let (status, err, written) = finish_on(child, &master);
    assert!(status.success(), "{status} {err}");
    let tag: String = built.out().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(tag, HUNTER2_TAG, "non-canonical");
    assert_eq!(written, b"\r\n", "non-canonical");
    assert_eq!(modes(&slave), before, "non-canonical");

    // The line is edited by Hirsel's own rules, with the terminal's erase and
    // kill keys: erase takes one character (all of a UTF-8 one), kill the
    // whole line, and once a byte past 1025 has been dropped the line stays
    // TooLong (4) whatever is erased.
    let built = build(&prompt("", "", "Never"), Backend::Cranelift, &[]);
    let (master, slave) = pty();
    let keys = termios(slave.as_raw_fd()).c_cc;
    let (erase, kill) = (keys[libc::VERASE], keys[libc::VKILL]);
    let long = [b'x'; 1100];
    let tag = HUNTER2_TAG.as_bytes();
    let cases: [(&str, Vec<u8>, &[u8]); 4] = [
        (
            "erase",
            [&b"hunzz"[..], &[erase, erase], b"ter2\n"].concat(),
            tag,
        ),
        (
            "utf-8 erase",
            ["hunter2é".as_bytes(), &[erase], b"\n"].concat(),
            tag,
        ),
        ("kill", [&b"wrong"[..], &[kill], b"hunter2\n"].concat(), tag),
        (
            "erase after overflow",
            [&long[..], &[erase; 100], b"\n"].concat(),
            &[4],
        ),
    ];
    let before = modes(&slave);
    for (case, typed, want) in cases {
        let mut child = spawn(&built, &slave);
        armed(&slave, &mut child);
        type_on(&master, &typed);
        let (status, err, written) = finish_on(child, &master);
        assert!(status.success(), "{case}: {status} {err}");
        let out = built.out();
        let got = if out.len() == 32 {
            out.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
                .into_bytes()
        } else {
            out
        };
        assert_eq!(got, want, "{case}");
        assert_eq!(written, b"\r\n", "{case}");
        assert_eq!(modes(&slave), before, "{case}");
    }
}

// Law B: with no Process grant, each terminating signal kills a program
// blocked in the read by that signal, with the modes restored.
#[test]
fn b_terminating_signals_restore_the_modes() {
    let built = build(&prompt("", "", "Never"), Backend::Cranelift, &[]);
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
        let (_master, slave) = pty();
        let before = modes(&slave);
        let mut child = spawn(&built, &slave);
        armed(&slave, &mut child);
        signal(&child, sig);
        let (status, err) = finish(child);
        assert_eq!(status.signal(), Some(sig), "{sig}: {status} {err}");
        assert_eq!(modes(&slave), before, "{sig}");
    }
}

// Law C: with Process, the first SIGINT completes `host.stopped`, the program
// cancels the read and gets Aborted, restored. A program that ignores
// `host.stopped` and gets SIGINT until it dies is restored too.
#[test]
fn c_the_stop_relay_keeps_the_first_signal() {
    let built = build(CANCELLED, Backend::Cranelift, &["Process", "Tasks"]);
    let (_master, slave) = pty();
    let before = modes(&slave);
    let mut child = spawn(&built, &slave);
    armed(&slave, &mut child);
    signal(&child, libc::SIGINT);
    let (status, err) = finish(child);
    assert!(status.success(), "{status} {err}");
    assert_eq!(built.out(), [6], "the read gives Aborted");
    assert_eq!(modes(&slave), before);

    let ignoring = build(
        &prompt("p: Process", "", "Never"),
        Backend::Cranelift,
        &["Process"],
    );
    let (_master, slave) = pty();
    let before = modes(&slave);
    let mut child = spawn(&ignoring, &slave);
    armed(&slave, &mut child);
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(start.elapsed() < PATIENCE, "SIGINT never ended the run");
        signal(&child, libc::SIGINT);
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status}");
    assert_eq!(modes(&slave), before);
}

// Law D: a 200 ms deadline gives TimedOut, restored; a run stop during the
// read exits 102, restored.
#[test]
fn d_a_timeout_and_a_run_stop_restore_the_modes() {
    let timed = build(
        &prompt("c: Clock", "", "At(c, host.mono_ns(c) + 200000000u64)"),
        Backend::Cranelift,
        &["Clock"],
    );
    let (_master, slave) = pty();
    let before = modes(&slave);
    let mut child = spawn(&timed, &slave);
    armed(&slave, &mut child);
    let (status, err) = finish(child);
    assert!(status.success(), "{status} {err}");
    assert_eq!(timed.out(), [7], "TimedOut");
    assert_eq!(modes(&slave), before);

    // A task overflows its stack 500 ms into the read: `run stop: Stack`.
    let stopped = build(
        &prompt(
            "c: Clock, tasks: Tasks",
            "let _h = task.spawn(tasks, bytes 65536, c, crash)",
            "Never",
        ),
        Backend::Cranelift,
        &["Clock", "Tasks"],
    );
    let (_master, slave) = pty();
    let before = modes(&slave);
    let mut child = spawn(&stopped, &slave);
    armed(&slave, &mut child);
    let (status, err) = finish(child);
    assert_eq!(status.code(), Some(102), "{status} {err}");
    assert!(err.starts_with("run stop: "), "{err}");
    assert_eq!(modes(&slave), before);
}

mod supervised;

/// The `STDIN_SECRET` issue and the outcome that answers it, and every
/// decoded payload, in a sealed tape.
fn secret_frames(tape: &Path) -> (Vec<hirsel_tape_format::Frame>, Vec<u8>) {
    use hirsel_tape_format::{Blocks, Decoder, END, Frame, op};
    use std::io::Read;
    let sealed = std::fs::read(tape).unwrap();
    let mut input = sealed.as_slice();
    let (mut blocks, _) = Blocks::open(|buffer| input.read(buffer)).unwrap();
    let mut decoder = Decoder::default();
    let mut frames = Vec::new();
    let mut payloads = sealed.clone();
    let mut reader = None;
    while let Some((flags, payload)) = blocks.next(|buffer| input.read(buffer)).unwrap() {
        payloads.extend_from_slice(payload);
        for frame in decoder.block(payload, flags & END != 0).unwrap() {
            match &frame {
                Frame::Issue { op, task, .. } if *op == op::STDIN_SECRET => {
                    reader = Some(*task);
                    frames.push(frame);
                }
                Frame::Op { op, .. } if *op == op::STDIN_SECRET => frames.push(frame),
                Frame::Done { task, out, .. } if reader == Some(*task) => {
                    reader = None;
                    frames.push(Frame::Done {
                        gap: 0,
                        task: *task,
                        out: out.clone(),
                    });
                }
                _ => {}
            }
        }
    }
    (frames, payloads)
}

// Law F: two recorded runs typing secrets of different lengths hold neither
// secret and the same STDIN_SECRET frames; a replay on pipes ends Sealed.
#[test]
fn f_the_recording_holds_no_secret() {
    let dir = tempfile::tempdir().unwrap();
    let product = product_of(dir.path(), &prompt("", "", "Never"));
    let entry = crate::DefPath::from_full(ENTRY).unwrap();
    let mut options = granted(Backend::Cranelift, dir.path(), &[]);
    options.runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(std::env::var_os("HIRSEL_CORE_RT_RECORD_ARCHIVE").expect("recording runtime archive"))
        .canonicalize()
        .unwrap();
    options.recording = true;
    let runtime = options.runtime.clone();
    let record = product.build(&entry, &options).unwrap();
    let secrets: [&[u8]; 2] = [b"hunter2", b"correct horse battery staple"];
    let mut recorded = Vec::new();
    for (n, typed) in secrets.iter().enumerate() {
        let tape = dir.path().join(format!("{n}.tape"));
        let (master, slave) = pty();
        let before = modes(&slave);
        let mut child = defaults(&mut Command::new(&record.executable))
            .env("HIRSEL_RECORD", &tape)
            .stdin(slave.try_clone().unwrap())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        armed(&slave, &mut child);
        type_on(&master, &[*typed, b"\n"].concat());
        let (status, err, _) = finish_on(child, &master);
        assert!(status.success(), "{status} {err}");
        assert_eq!(modes(&slave), before);
        std::fs::remove_file(dir.path().join("out")).unwrap();
        let (frames, bytes) = secret_frames(&tape);
        assert!(
            !bytes.windows(typed.len()).any(|w| w == *typed),
            "the tape holds the secret"
        );
        recorded.push((tape, frames));
    }
    assert_eq!(recorded[0].1.len(), 2, "{:?}", recorded[0].1);
    assert_eq!(recorded[0].1, recorded[1].1);

    options.grant_mask = 0;
    options.directories.clear();
    options.replay = Some(crate::ReplayPlan {
        path: recorded[1].0.to_str().unwrap().into(),
        diagnose: true,
    });
    let replay = product.build(&entry, &options).unwrap();
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

    // A recorded TimedOut replays too: the tape takes the read's own failures.
    let product = product_of(
        dir.path(),
        &prompt("c: Clock", "", "At(c, host.mono_ns(c) + 200000000u64)"),
    );
    let mut options = granted(Backend::Cranelift, dir.path(), &["Clock"]);
    options.runtime = runtime;
    options.recording = true;
    let record = product.build(&entry, &options).unwrap();
    let tape = dir.path().join("timed.tape");
    let (_master, slave) = pty();
    let mut child = defaults(&mut Command::new(&record.executable))
        .env("HIRSEL_RECORD", &tape)
        .stdin(slave.try_clone().unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    armed(&slave, &mut child);
    let (status, err) = finish(child);
    assert!(status.success(), "{status} {err}");
    assert_eq!(
        std::fs::read(dir.path().join("out")).unwrap(),
        [7],
        "TimedOut"
    );
    options.grant_mask = 0;
    options.directories.clear();
    options.replay = Some(crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: true,
    });
    let replay = product.build(&entry, &options).unwrap();
    let output = Command::new(replay.executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{err}");
    assert!(!err.contains("replay: "), "{err}");
}

// Law H: a plain read queued behind the secret read gets nothing typed
// into the secret read, whichever way it ends. The test stops the terminal's
// output (Ctrl-S) and holds a write to it from a thread of its own, so the
// restore's TCSAFLUSH waits in its drain, and types into that window. After
// Ctrl-Q the queued read must get only the next line. A kill after an
// over-long line starts it afresh, so that case reads its line (0).
#[test]
fn h_nothing_reaches_a_read_queued_behind() {
    const STOP_OUTPUT: u8 = 0x13;
    const START_OUTPUT: u8 = 0x11;
    let cases: [(&str, &str, &str, &[&str], u8); 4] = [
        ("", "Never", "", &["Clock", "Tasks"], 4),
        ("", "Never", "", &["Clock", "Tasks"], 0),
        (
            "",
            "At(c, host.mono_ns(c) + 1500000000u64)",
            "",
            &["Clock", "Tasks"],
            7,
        ),
        (
            ", p: Process",
            "Never",
            "host.stopped(p)\ntask.cancel(mut a)",
            &["Clock", "Tasks", "Process"],
            6,
        ),
    ];
    for (params, until, stop, grants, code) in cases {
        let source = QUEUED
            .replace("PARAMS", params)
            .replace("UNTIL", until)
            .replace("STOP", stop);
        let built = build(&source, Backend::Cranelift, grants);
        let (master, slave) = pty();
        let before = modes(&slave);
        let mut child = spawn(&built, &slave);
        armed(&slave, &mut child);
        // Let the queued read reach the queue behind the secret read.
        std::thread::sleep(Duration::from_millis(600));
        type_on(&master, &[STOP_OUTPUT]);
        std::thread::sleep(Duration::from_millis(100));
        let held = slave.try_clone().unwrap();
        let writer = std::thread::spawn(move || File::from(held).write_all(b".").unwrap());
        std::thread::sleep(Duration::from_millis(100));
        match code {
            4 => type_on(&master, &[&[b'x'; 1100][..], b"\n"].concat()),
            0 => {
                let kill = termios(slave.as_raw_fd()).c_cc[libc::VKILL];
                type_on(
                    &master,
                    &[&[b'x'; 1100][..], &[kill], b"hunter2\n"].concat(),
                );
            }
            7 => {
                std::thread::sleep(Duration::from_millis(1500));
                type_on(&master, b"tail\n");
            }
            _ => {
                signal(&child, libc::SIGINT);
                std::thread::sleep(Duration::from_millis(500));
                type_on(&master, b"tail\n");
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        type_on(&master, &[START_OUTPUT]);
        writer.join().unwrap();
        // On Darwin the restore's drain ends only once the master has read
        // the held write and the echoed newline.
        std::thread::sleep(Duration::from_millis(100));
        master_output(&master);
        std::thread::sleep(Duration::from_millis(300));
        type_on(&master, b"after\n");
        let (status, err, _) = finish_on(child, &master);
        assert!(status.success(), "{code}: {status} {err}");
        let out = built.out();
        assert_eq!(out.first(), Some(&code), "{code}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out[1..]),
            "after\n",
            "{code}: the queued read"
        );
        assert_eq!(modes(&slave), before, "{code}");
    }
}

// Law G: on a pipe holding "abc" the read gives NotTerminal and consumes
// nothing, so a following `stdin.read` reads "abc".
#[test]
fn g_not_a_terminal_reads_nothing() {
    let built = build(&prompt("", "", "Never"), Backend::Cranelift, &[]);
    let mut child = defaults(&mut Command::new(&built.executable))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"abc").unwrap();
    let (status, err) = finish(child);
    assert!(status.success(), "{status} {err}");
    assert_eq!(built.out(), b"\x01abc");
}
