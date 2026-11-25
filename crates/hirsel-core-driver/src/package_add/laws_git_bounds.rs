// Git source law L37 (packages.md section 12): the bounds of GIT-6 and GIT-9 against hostile
// servers of the laws_git.rs fixture, included after it. Packs too large to hold are streamed.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const MIB: u64 = 1 << 20;
/// The largest size one copy instruction names (three size bytes).
const COPY_MAX: u32 = 0x00ff_ffff;

/// A pack written as it is sent: every byte goes to the side band and into the checksum.
struct PackStream<'a> {
    send: &'a mut dyn FnMut(&[u8]) -> bool,
    sum: sha1_checked::Sha1,
    written: u64,
}

impl PackStream<'_> {
    fn put(&mut self, bytes: &[u8]) -> bool {
        use sha2::Digest;
        self.sum.update(bytes);
        self.written += bytes.len() as u64;
        (self.send)(bytes)
    }

    fn header(&mut self, count: u32) -> bool {
        let mut out = b"PACK".to_vec();
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        self.put(&out)
    }

    /// A blob of `size` zero bytes, its zlib stream written as stored blocks by hand (RFC 1950
    /// and 1951): the Adler-32 of `n` zeros is `(n mod 65521) << 16 | 1`.
    fn zero_blob(&mut self, size: u64) -> bool {
        let mut head = Vec::new();
        entry_header(&mut head, Kind::Blob as u8, size);
        head.extend_from_slice(&[0x78, 0x01]);
        if !self.put(&head) {
            return false;
        }
        let zeros = vec![0u8; 65535];
        let mut left = size;
        loop {
            let n = left.min(65535) as u16;
            left -= u64::from(n);
            let mut block = vec![u8::from(left == 0)];
            block.extend_from_slice(&n.to_le_bytes());
            block.extend_from_slice(&(!n).to_le_bytes());
            block.extend_from_slice(&zeros[..usize::from(n)]);
            if !self.put(&block) {
                return false;
            }
            if left == 0 {
                break;
            }
        }
        let adler = ((size % 65521) << 16 | 1) as u32;
        self.put(&adler.to_be_bytes())
    }

    fn entry(&mut self, kind: Kind, data: &[u8]) -> bool {
        let mut out = Vec::new();
        entry_header(&mut out, kind as u8, data.len() as u64);
        out.extend(deflate(data));
        self.put(&out)
    }

    /// An `OFS_DELTA` entry against the entry written at `base`.
    fn offset_delta(&mut self, base: u64, data: &[u8]) -> bool {
        let mut out = Vec::new();
        entry_header(&mut out, 6, data.len() as u64);
        let mut n = self.written - base;
        let mut bytes = vec![(n & 0x7f) as u8];
        n >>= 7;
        while n != 0 {
            n -= 1;
            bytes.push(0x80 | (n & 0x7f) as u8);
            n >>= 7;
        }
        bytes.reverse();
        out.extend(bytes);
        out.extend(deflate(data));
        self.put(&out)
    }

    /// Sends what has been written so far.
    fn flush(&mut self) -> bool {
        (self.send)(&[])
    }

    fn finish(&mut self) -> bool {
        use sha2::Digest;
        let sum = std::mem::take(&mut self.sum).finalize().to_vec();
        (self.send)(&sum)
    }
}

/// A `fetch` answer streamed in HTTP chunks: shallow-info for C1, then whatever `body`
/// writes on side band 1. `body` returns whether the response is to be ended.
fn streamed(
    body: impl FnOnce(&mut PackStream) -> bool + Send + 'static,
) -> impl FnMut(&Request) -> Option<Reply> + Send + 'static {
    let mut body = Some(body);
    move |request| {
        if request.command.as_deref() != Some("fetch") {
            return None;
        }
        let body = body.take().expect("one fetch");
        Some(Reply::Raw(Box::new(move |out: &mut dyn Write| {
            let head = b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/x-git-upload-pack-result\r\nTransfer-Encoding: chunked\r\n\r\n";
            if out.write_all(head).is_err() {
                return;
            }
            // One write per HTTP chunk: small writes stall on Nagle's algorithm.
            let mut chunk = |bytes: &[u8]| {
                let mut framed = format!("{:x}\r\n", bytes.len()).into_bytes();
                framed.extend_from_slice(bytes);
                framed.extend_from_slice(b"\r\n");
                out.write_all(&framed).and_then(|()| out.flush()).is_ok()
            };
            let mut first = Vec::new();
            pkt(&mut first, b"shallow-info\n");
            pkt(&mut first, format!("shallow {C1}\n").as_bytes());
            first.extend_from_slice(b"0001");
            pkt(&mut first, b"packfile\n");
            if !chunk(&first) {
                return;
            }
            // Side-band packets gathered into HTTP chunks of about 1 MiB; an empty `data`
            // sends what is gathered.
            let mut pending = Vec::new();
            let mut send = |data: &[u8]| {
                for piece in data.chunks(65515) {
                    let mut payload = vec![1u8];
                    payload.extend_from_slice(piece);
                    pkt(&mut pending, &payload);
                }
                if (data.is_empty() || pending.len() >= 1 << 20) && !pending.is_empty() {
                    let sent = chunk(&pending);
                    pending.clear();
                    return sent;
                }
                true
            };
            let mut stream = PackStream {
                send: &mut send,
                sum: sha1_checked::Sha1::default(),
                written: 0,
            };
            if body(&mut stream) && stream.flush() {
                drop(stream);
                let _ = chunk(b"0000") && chunk(b"");
            }
        })))
    }
}

/// A delta of `copies` copy instructions of `COPY_MAX` bytes from offset 0 of a base of
/// `base` bytes, declaring a result of `result` bytes.
fn copies(base: u64, result: u64, copies: usize) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, base);
    varint(&mut out, result);
    for _ in 0..copies {
        copy_op(&mut out, 0, COPY_MAX);
    }
    out
}

/// L37's binding: L31's, pinned to W.
fn bounds_app(law: &PackageLaw) {
    law.app("app", &hand_binding(PACKAGE_W));
}

/// L37. bug: a client that reads a hostile pack to its end before applying a bound, a delta
/// whose result is built before its size is checked, and nesting with no bound.
/// oracle: the limits of GIT-6 and GIT-9 and the sizes of each hostile pack, derived by hand
/// from gitformat-pack (16 MiB is 16777216 bytes; a copy names at most 16777215); the refusal
/// texts are section 9's rows written by hand.
#[test]
fn l37_bounds_hold_before_memory() {
    let law = PackageLaw::new();
    let server = GitServer::start(&law);
    server.serve("alpha.test/r1", Repo::r1());
    bounds_app(&law);
    let source = format!("git:https://alpha.test/r1#{C1}/wrap");
    let bad = |failure: &str| {
        PackageLaw::refuse(
            server.run(&law, &["check", "app"], "packages"),
            &format!("wrap@1: {source} sent a bad pack: {failure}"),
        );
        law.empty_store();
    };

    // 4194305 objects declared, and the response held open: the refusal comes from the header.
    let release = Arc::new(AtomicBool::new(false));
    let held = release.clone();
    server.hook(streamed(move |pack| {
        if pack.header(4_194_305) && pack.flush() {
            for _ in 0..1200 {
                if held.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
        false
    }));
    bad("over the limit of 4194304 objects");
    release.store(true, Ordering::SeqCst);

    // A response past 512 MiB: 1024 blobs of 1 MiB, cut off by the client.
    server.hook(streamed(|pack| {
        pack.header(1024) && (0..1024).all(|_| pack.zero_blob(MIB))
    }));
    PackageLaw::refuse(
        server.run(&law, &["check", "app"], "packages"),
        &format!(
            "wrap@1: fetching {source} failed: the response is over the limit of 512 MiB; hirsel fetches the whole commit, so a repository whose commit is larger cannot be a git source"
        ),
    );
    law.empty_store();

    // A 16 MiB blob and nine deltas of 30 copies each, 503316450 bytes: 16777216 plus eight
    // results is 4043308816, the ninth passes 4 GiB (4294967296).
    let base = vec![0u8; (16 * MIB) as usize];
    let mut entries = vec![Entry::Whole(Kind::Blob, base.clone())];
    for _ in 0..9 {
        entries.push(Entry::Offset(0, copies(16 * MIB, 30 * u64::from(COPY_MAX), 30)));
    }
    server.hook(fetch_answers(write_pack(&entries)));
    bad("over the limit of 4 GiB inflated");

    // A chain of 4096 deltas over a one-byte blob: the last is the 4096th link.
    let mut entries = vec![Entry::Whole(Kind::Blob, b"x".to_vec())];
    for n in 0..4096 {
        let mut one = Vec::new();
        varint(&mut one, 1);
        varint(&mut one, 1);
        copy_op(&mut one, 0, 1);
        entries.push(Entry::Offset(n, one));
    }
    server.hook(fetch_answers(write_pack(&entries)));
    bad("over the limit of 4095 delta links");

    // A delta bomb: 4096 copies of 16777215 bytes from a 16 MiB blob, 16 KiB of instructions
    // for a result of 68719472640 bytes.
    let bomb = copies(16 * MIB, 4096 * u64::from(COPY_MAX), 4096);
    assert_eq!(bomb.len(), 4 + 6 + 4096 * 4, "hand count of the instructions");
    server.hook(fetch_answers(write_pack(&[
        Entry::Whole(Kind::Blob, base),
        Entry::Offset(0, bomb),
    ])));
    bad("over the limit of 512 MiB object");

    // A blob whose header says 5 bytes and whose data inflates to 6, at offset 12.
    let mut raw = Vec::new();
    entry_header(&mut raw, Kind::Blob as u8, 5);
    raw.extend(deflate(b"hello!"));
    server.hook(fetch_answers(write_pack(&[Entry::Raw(raw)])));
    bad("object at 12 inflates to 6 bytes, not 5");
    server.with(|served| served.hook = None);

    near_the_bounds(&law, &server);
    fetch_chain(&law, &server);
}

/// C1's objects, a blob of 500 MiB of zeros and seven deltas that copy it and insert one byte:
/// 524288000 bytes on the wire and 4194304007 inflated, under 512 MiB and 4 GiB. The check
/// passes in a child whose peak RSS stays under L21's 2 GiB.
fn near_the_bounds(law: &PackageLaw, server: &GitServer) {
    let objects = r1_objects();
    let ids = objects.reachable(C1);
    let size = 500 * MIB;
    server.hook(streamed(move |pack| {
        if !pack.header(ids.len() as u32 + 8) {
            return false;
        }
        for id in &ids {
            let (kind, data) = &objects.0[id];
            if !pack.entry(*kind, data) {
                return false;
            }
        }
        let base = pack.written;
        if !pack.zero_blob(size) {
            return false;
        }
        for n in 0..7u8 {
            let mut delta = Vec::new();
            varint(&mut delta, size);
            varint(&mut delta, size + 1);
            let mut at = 0u64;
            while at < size {
                let take = (size - at).min(u64::from(COPY_MAX));
                copy_op(&mut delta, at as u32, take as u32);
                at += take;
            }
            delta.extend_from_slice(&[1, n]);
            if !pack.offset_delta(base, &delta) {
                return false;
            }
        }
        pack.finish()
    }));
    let mut command = law.command(&["check", "app"], "packages");
    server.route(&mut command);
    let (status, stderr, peak_kib) = peak_rss(command);
    server.with(|served| served.hook = None);
    assert_eq!(status, Some(0), "{stderr}");
    assert!(peak_kib < 2 * 1024 * 1024, "peak RSS {peak_kib} KiB");
    assert_eq!(stored(law, "packages"), roots(&[PACKAGE_X, PACKAGE_W]));
    fs::remove_dir_all(law.path("packages")).unwrap();
    fs::create_dir(law.path("packages")).unwrap();
}

/// Runs `command` to its end and reads the child's own peak RSS from `wait4`.
fn peak_rss(mut command: Command) -> (Option<i32>, String, i64) {
    use std::io::Read;
    let mut child = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let reader = thread::spawn(move || {
        let mut text = String::new();
        stderr.read_to_string(&mut text).unwrap();
        text
    });
    let pid = child.id() as libc::pid_t;
    let mut status = 0;
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: the pid is this test's own unreaped child; wait4 fills the rusage it is given.
    let waited = unsafe { libc::wait4(pid, &mut status, 0, usage.as_mut_ptr()) };
    assert_eq!(waited, pid);
    std::mem::forget(child);
    let code = libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status));
    // SAFETY: wait4 succeeded, so the rusage is filled.
    let peak = unsafe { usage.assume_init() }.ru_maxrss;
    (code, reader.join().unwrap(), peak)
}

/// 257 repositories, each commit's package binding the next by `git` and `rev`: the 257th
/// fetch refuses before it is made.
fn fetch_chain(law: &PackageLaw, server: &GitServer) {
    let mut next: Option<String> = None;
    for n in (0..257).rev() {
        let binding = next.as_ref().map_or(String::new(), |rev| {
            format!(
                "\n[dependencies]\nn{} = {{ major = 1, root = \"{PACKAGE_X}\", git = \"https://alpha.test/n{}\", rev = \"{rev}\" }}\n",
                n + 1,
                n + 1
            )
        });
        let tree = Tree::default()
            .file(
                "hirsel.toml",
                format!("[package]\nname = \"n{n}\"\nmajor = 1\ncharge = \"charge-v1\"\n\n[sources]\n\"n.hirsel\" = \"n\"\n{binding}"),
            )
            .file("n.hirsel", "pub fn one() -> u64 { 1u64 }\n");
        let (repo, commit) = built(tree);
        server.serve(&format!("alpha.test/n{n}"), repo);
        next = Some(commit);
    }
    let first = next.unwrap();
    app_at(law, "chain");
    let before = manifest_text(law, "chain");
    let (status, _, stderr) =
        server.add(law, "chain", &[&format!("git+https://alpha.test/n0#{first}")]);
    assert_eq!(status, Some(2), "{stderr}");
    assert_eq!(
        stderr,
        format!("hirsel: n0@1: git:https://alpha.test/n0#{first} causes more than 256 fetches\n")
    );
    assert_eq!(manifest_text(law, "chain"), before);
    let log = server.log();
    assert!(log.iter().any(|entry| entry.starts_with("alpha.test fetch /n255/")), "{log:?}");
    assert!(log.iter().all(|entry| !entry.contains("/n256/")), "{log:?}");
    law.empty_store();
}
