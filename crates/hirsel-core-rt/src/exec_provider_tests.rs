//! EXEC-4: the provider runs the pinned image.
use super::*;

#[test]
fn exec_provider_runs_the_sealed_image_and_uses_directional_pipes() {
    use std::process::Command;
    crate::openat2_required::require();
    const PROBE: &str = "HIRSEL_EXEC_PROVIDER_TEST_IMAGE";
    if let Some(image) = std::env::var_os(PROBE) {
        exec_provider_law(std::path::Path::new(&image));
        return;
    }
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("digest.c");
    std::fs::create_dir(fixture.path().join("plain")).unwrap();
    let image = fixture.path().join("plain/digest");
    std::fs::write(
        &source,
        r#"
#include <stdio.h>
#include <unistd.h>
#ifdef __APPLE__
#include <CommonCrypto/CommonDigest.h>
#include <mach-o/dyld.h>
#else
#include <openssl/evp.h>
#endif

int main(void) {
    if (getchar() != 'x' || getchar() != 'y' || getchar() != EOF) return 1;
    unsigned char block[4096], digest[32];
    size_t n;
#ifdef __APPLE__
    char path[4096];
    uint32_t size = sizeof path;
    CC_SHA256_CTX state;
    if (_NSGetExecutablePath(path, &size)) return 2;
    FILE *self = fopen(path, "rb");
    if (!self || !CC_SHA256_Init(&state)) return 2;
    while ((n = fread(block, 1, sizeof block, self))) CC_SHA256_Update(&state, block, n);
    if (ferror(self) || !CC_SHA256_Final(digest, &state)) return 4;
#else
    FILE *self = fopen("/proc/self/exe", "rb");
    EVP_MD_CTX *state = EVP_MD_CTX_new();
    if (!self || !state || !EVP_DigestInit_ex(state, EVP_sha256(), NULL)) return 2;
    while ((n = fread(block, 1, sizeof block, self))) {
        if (!EVP_DigestUpdate(state, block, n)) return 3;
    }
    if (ferror(self) || !EVP_DigestFinal_ex(state, digest, NULL)) return 4;
#endif
    for (int i = 0; i < 32; ++i) printf("%02x", digest[i]);
    fputs("digest", stderr);
    return 23;
}
"#,
    )
    .unwrap();
    // The digest comes from OpenSSL on Linux and from libSystem on Darwin.
    #[cfg(target_os = "linux")]
    let library: &[&str] = &["-lcrypto"];
    #[cfg(target_os = "macos")]
    let library: &[&str] = &[];
    let compilation = Command::new("cc")
        .arg(&source)
        .args(["-o"])
        .arg(&image)
        .args(library)
        .output()
        .unwrap();
    assert!(
        compilation.status.success(),
        "{}",
        String::from_utf8_lossy(&compilation.stderr)
    );

    // A compressed file (most of what macOS installs) has its bytes only while its
    // compression flag is set, so a pin that drops the flag runs an empty file.
    #[cfg(target_os = "macos")]
    let forms = {
        use std::os::darwin::fs::MetadataExt;
        std::fs::create_dir(fixture.path().join("compressed")).unwrap();
        let compressed = fixture.path().join("compressed/digest");
        let copy = Command::new("ditto")
            .arg("--hfsCompression")
            .args([&image, &compressed])
            .output()
            .unwrap();
        assert!(
            copy.status.success(),
            "{}",
            String::from_utf8_lossy(&copy.stderr)
        );
        let flags = std::fs::metadata(&compressed).unwrap().st_flags();
        assert_ne!(flags & libc::UF_COMPRESSED, 0, "ditto left the image plain");
        // A universal file holds an image per machine: the pin covers them all, and
        // this machine runs its own.
        std::fs::create_dir(fixture.path().join("universal")).unwrap();
        let universal = fixture.path().join("universal/digest");
        let compilation = Command::new("cc")
            .args(["-arch", "arm64", "-arch", "x86_64"])
            .arg(&source)
            .args(["-o"])
            .arg(&universal)
            .output()
            .unwrap();
        assert!(
            compilation.status.success(),
            "{}",
            String::from_utf8_lossy(&compilation.stderr)
        );
        vec![compressed, universal]
    };
    #[cfg(target_os = "linux")]
    let forms = Vec::new();
    // Each image is pinned from its own directory, which the law writes in.
    for image in std::iter::once(image).chain(forms) {
        let check = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "provider_tests::exec_provider_tests::exec_provider_runs_the_sealed_image_and_uses_directional_pipes",
                "--nocapture",
            ])
            .env(PROBE, &image)
            .output()
            .unwrap();
        assert!(
            check.status.success(),
            "Exec provider child for {} {:?}\n{}\n{}",
            image.display(),
            check.status,
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
    }
}

/// A universal file is admitted for the images it holds for this machine, each of which
/// must lie inside it (EXEC-4).
#[cfg(target_os = "macos")]
#[test]
fn exec_pin_admits_a_universal_file_only_for_an_image_of_this_machine() {
    use hirsel_core_net::exec::Request as PolicyRequest;
    use ring::digest::{SHA256, digest};
    const ARM64: u32 = 0x0100_000c;
    const X86_64: u32 = 0x0100_0007;
    // A universal header (magic, slice count) and one slice entry (CPU type, subtype,
    // offset, size, alignment), all big-endian, then the slice: the header of a 64-bit
    // arm64 Mach-O executable.
    let universal = |cpu: u32, size: u32| {
        let table = [0xcafe_babe, 1, cpu, 0, 28, size, 0].map(u32::to_be_bytes);
        let slice = [0xfeed_facf, ARM64, 0, 2, 0, 0, 0, 0].map(u32::to_le_bytes);
        [table.as_flattened(), slice.as_flattened()].concat()
    };
    let root = tempfile::tempdir().unwrap();
    let image = root.path().join("image");
    let profile = root.path().join("policy.toml");
    for (case, bytes, admitted) in [
        ("an image of this machine", universal(ARM64, 32), true),
        ("an image of another machine", universal(X86_64, 32), false),
        (
            "an image that ends outside the file",
            universal(ARM64, 33),
            false,
        ),
    ] {
        std::fs::write(&image, &bytes).unwrap();
        let pin = hirsel_core_ids::hex::encode(digest(&SHA256, &bytes).as_ref());
        std::fs::write(
            &profile,
            format!(
                "[held]\nimage = {:?}\nsha256 = {:?}\nargv0 = \"held\"\nargs = [[]]\nenv = {{}}\ndirs = {{ work = {:?} }}\n",
                image,
                pin,
                root.path()
            ),
        )
        .unwrap();
        let policy = PolicyRequest::parse(&format!("{} held", profile.display()))
            .unwrap()
            .load()
            .unwrap();
        let refusal = policy.pin_images().err();
        assert_eq!(refusal.is_none(), admitted, "{case}: {refusal:?}");
        if let Some(refusal) = refusal {
            assert!(refusal.contains("parse sealed image"), "{case}: {refusal}");
        }
    }
}

fn exec_provider_law(image: &std::path::Path) {
    use crate::{
        entry::{self, Grant as EntryGrant},
        exec::Grant,
        exec_runtime::{hirsel_core_exec_signal, hirsel_core_exec_spawn, hirsel_core_exec_wait},
        host::exec::Supervisor,
    };
    use hirsel_core_net::exec::Request as PolicyRequest;
    use ring::digest::{SHA256, digest};
    use std::{ptr, sync::Arc};

    let root = image.parent().unwrap();
    let profile = root.join("policy.toml");
    let hash: [u8; 32] = digest(&SHA256, &std::fs::read(image).unwrap())
        .as_ref()
        .try_into()
        .unwrap();
    let wanted = hirsel_core_ids::hex::encode(&hash);
    std::fs::write(
        &profile,
        format!(
            "[digest]\nimage = {:?}\nsha256 = {:?}\nargv0 = \"pinned digest\"\nargs = [[]]\nenv = {{}}\ndirs = {{ work = {:?} }}\n",
            image, wanted, root
        ),
    )
    .unwrap();
    let supervisor = Arc::new(Supervisor::start(crate::exec::probe).unwrap());
    let policy = PolicyRequest::parse(&format!("{} digest", profile.display()))
        .unwrap()
        .load()
        .unwrap();
    let rules = policy.rules();
    let (profiles, approval) = policy.pin_images().unwrap();
    // An ELF image names its loader; a Mach-O's dyld is implicit.
    #[cfg(target_os = "linux")]
    let expected_interpreter = Some(if cfg!(target_arch = "aarch64") {
        "/lib/ld-linux-aarch64.so.1"
    } else {
        "/lib64/ld-linux-x86-64.so.2"
    });
    #[cfg(target_os = "macos")]
    let expected_interpreter: Option<&str> = None;
    let interpreter = profiles["digest"].image.interpreter.as_deref();
    assert_eq!(
        interpreter.map(std::ffi::CStr::to_bytes),
        expected_interpreter.map(str::as_bytes)
    );
    let named = expected_interpreter.unwrap_or("none");
    assert!(approval[0].contains(&format!("interpreter {named}, policy ")));

    // A regular file at the original path now contains an entirely different
    // image. A path-based launch cannot run the program that reports this pin.
    std::fs::write(image, b"replaced after seal").unwrap();
    std::fs::create_dir(root.join("here")).unwrap();
    std::os::unix::fs::symlink("here", root.join("link")).unwrap();
    let grant = Grant::Native {
        supervisor,
        profiles: Arc::new(profiles),
        rules,
    };
    let mut os = OsProvider::from_streams(None, None);
    let provider = os.provider();
    let grants = [
        EntryGrant {
            kind: entry::EXEC,
            provider: &provider,
            handle: std::ptr::from_ref(&grant) as u64,
            terminal: false,
        },
        EntryGrant {
            kind: entry::CLOCK,
            provider: &provider,
            handle: 0,
            terminal: false,
        },
    ];
    unsafe extern "C" fn ignore_diagnostic(_: *mut u8, _: *const u8, _: usize) {}
    let config = Config {
        root_limit: 1 << 20,
        args: ptr::null(),
        args_len: 0,
        grants: ptr::null(),
        grants_len: 0,
        provider: &provider,
        state: ptr::null_mut(),
        diagnostic: ignore_diagnostic,
        describe: None,
        scheduler: crate::tasks::SchedulerProvider::default(),
        stop_fd: -1,
        capabilities: grants.as_ptr(),
        capabilities_len: grants.len(),
        close_grants: None,
        launch: ptr::null(),
        describe_output: None,
    };

    fn bytes(prefix: &[u8]) -> Buffer {
        Buffer {
            data: prefix.as_ptr().cast_mut(),
            len: prefix.len(),
            capacity: 0,
        }
    }

    let mut files = crate::host_file::Files::default();
    let mut context = crate::Context::new(1 << 20);
    context.host_files = &mut files;
    context.run = &config;
    // SAFETY: all grants, buffers and the context pin their complete extents
    // until delivery. The fixture consumes every handle exactly once.
    unsafe {
        let mut launch = |program: &[u8], path: &[u8]| {
            hirsel_core_exec_spawn(
                &mut context,
                1,
                &bytes(program),
                &bytes(b""),
                &bytes(b""),
                &bytes(b"work"),
                &bytes(path),
                1,
                1,
                1,
                601,
            )
        };
        for (program, path) in [
            (b"other".as_slice(), b"here".as_slice()),
            (b"digest", b"link"),
        ] {
            let denied = launch(program, path);
            assert_eq!(denied.outcome.error, IoError { tag: 1, code: 0 });
            assert_eq!(denied.pipes, [0; 3]);
        }
        let started = launch(b"digest", b"here");
        assert_eq!(started.outcome.error, IoError::OK);
        let proc = started.outcome.value;
        let [stdin, stdout, stderr] = started.pipes;
        let timed = hirsel_core_exec_wait(&mut context, proc, 1, 2, 0, 602);
        assert_eq!(timed.error, IoError { tag: 2, code: 0 });
        let sent = crate::net::hirsel_core_host_send_pair(
            &mut context,
            stdin,
            &bytes(b"x"),
            &bytes(b"y"),
            0,
            0,
            0,
            603,
        );
        assert_eq!((sent.error, sent.value), (IoError::OK, 2));
        crate::net::hirsel_core_socket_drop(&mut context, stdin);
        let waited = hirsel_core_exec_wait(&mut context, proc, 0, 0, 0, 604);
        assert_eq!((waited.error, waited.value), (IoError::OK, 23));

        for (pipe, expected) in [(stdout, wanted.as_bytes()), (stderr, b"digest".as_slice())] {
            let mut data = [0; 75];
            let mut buffer = Buffer {
                data: data.as_mut_ptr(),
                len: 0,
                capacity: data.len(),
            };
            let got = crate::net::hirsel_core_host_recv_bounded(
                &mut context,
                pipe,
                &mut buffer,
                75,
                0,
                0,
                0,
                605,
            );
            assert_eq!((got.error, got.value), (IoError::OK, expected.len() as u64));
            assert_eq!(&data[..buffer.len], expected);
            let eof = crate::net::hirsel_core_host_recv_bounded(
                &mut context,
                pipe,
                &mut buffer,
                75,
                0,
                0,
                0,
                606,
            );
            assert_eq!((eof.error, eof.value), (IoError::OK, 0));
            crate::net::hirsel_core_socket_drop(&mut context, pipe);
        }
        let cached = hirsel_core_exec_wait(&mut context, proc, 1, 2, 0, 607);
        assert_eq!((cached.error, cached.value), (IoError::OK, 23));
        assert_eq!(
            hirsel_core_exec_signal(&mut context, proc, 2, 608).error,
            IoError::OK
        );
        crate::net::hirsel_core_socket_drop(&mut context, proc);
        assert!(files.sockets.is_empty());
    }
}

/// EXEC-4 on macOS: after the pin, a same-user writer clears `UF_IMMUTABLE` and
/// rewrites or replaces the pinned copy; the next spawn fails instead of running
/// the altered bytes.
#[cfg(target_os = "macos")]
#[test]
fn exec_provider_refuses_a_same_user_rewrite_of_the_pinned_copy() {
    use crate::{
        entry::{self, Grant as EntryGrant},
        exec::Grant,
        exec_runtime::{hirsel_core_exec_spawn, hirsel_core_exec_wait},
        host::exec::Supervisor,
    };
    use hirsel_core_net::exec::Request as PolicyRequest;
    use ring::digest::{SHA256, digest};
    use std::{
        ffi::{CStr, OsStr},
        os::{darwin::fs::MetadataExt, fd::AsRawFd, unix::ffi::OsStrExt},
        path::Path,
        process::Command,
        ptr,
        sync::Arc,
    };

    if crate::test_process::run_isolated(
        "provider_tests::exec_provider_tests::exec_provider_refuses_a_same_user_rewrite_of_the_pinned_copy",
    ) {
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let compile = |name: &str, code: u8| {
        let source = root.join(format!("{name}.c"));
        let binary = root.join(name);
        std::fs::write(&source, format!("int main(void) {{ return {code}; }}\n")).unwrap();
        let output = Command::new("cc")
            .arg(&source)
            .args(["-o"])
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        binary
    };
    let pinned = compile("pinned", 41);
    let altered = compile("altered", 99);
    let altered_bytes = std::fs::read(&altered).unwrap();

    let hash: [u8; 32] = digest(&SHA256, &std::fs::read(&pinned).unwrap())
        .as_ref()
        .try_into()
        .unwrap();
    let wanted = hirsel_core_ids::hex::encode(&hash);
    let profile = root.join("policy.toml");
    std::fs::write(
        &profile,
        format!(
            "[pinned]\nimage = {:?}\nsha256 = {:?}\nargv0 = \"pinned\"\nargs = [[]]\nenv = {{}}\ndirs = {{ work = {:?} }}\n",
            pinned, wanted, root
        ),
    )
    .unwrap();

    let supervisor = Arc::new(Supervisor::start(crate::exec::probe).unwrap());
    let policy = PolicyRequest::parse(&format!("{} pinned", profile.display()))
        .unwrap()
        .load()
        .unwrap();
    let rules = policy.rules();
    let (profiles, _) = policy.pin_images().unwrap();

    let mut path_buf = [0u8; libc::MAXPATHLEN as usize];
    // SAFETY: F_GETPATH writes a NUL-terminated path of at most MAXPATHLEN bytes.
    assert_eq!(
        unsafe {
            libc::fcntl(
                profiles["pinned"].image.file.as_raw_fd(),
                libc::F_GETPATH,
                path_buf.as_mut_ptr(),
            )
        },
        0
    );
    let copy_path = CStr::from_bytes_until_nul(&path_buf).unwrap().to_owned();
    let copy_os = Path::new(OsStr::from_bytes(copy_path.to_bytes()));
    let original_flags = std::fs::metadata(copy_os).unwrap().st_flags();
    assert_ne!(original_flags & libc::UF_IMMUTABLE, 0);

    let grant = Grant::Native {
        supervisor,
        profiles: Arc::new(profiles),
        rules,
    };
    let mut os = OsProvider::from_streams(None, None);
    let provider = os.provider();
    let grants = [
        EntryGrant {
            kind: entry::EXEC,
            provider: &provider,
            handle: std::ptr::from_ref(&grant) as u64,
            terminal: false,
        },
        EntryGrant {
            kind: entry::CLOCK,
            provider: &provider,
            handle: 0,
            terminal: false,
        },
    ];
    unsafe extern "C" fn ignore_diagnostic(_: *mut u8, _: *const u8, _: usize) {}
    let config = Config {
        root_limit: 1 << 20,
        args: ptr::null(),
        args_len: 0,
        grants: ptr::null(),
        grants_len: 0,
        provider: &provider,
        state: ptr::null_mut(),
        diagnostic: ignore_diagnostic,
        describe: None,
        scheduler: crate::tasks::SchedulerProvider::default(),
        stop_fd: -1,
        capabilities: grants.as_ptr(),
        capabilities_len: grants.len(),
        close_grants: None,
        launch: ptr::null(),
        describe_output: None,
    };

    fn bytes(prefix: &[u8]) -> Buffer {
        Buffer {
            data: prefix.as_ptr().cast_mut(),
            len: prefix.len(),
            capacity: 0,
        }
    }

    let mut files = crate::host_file::Files::default();
    let mut context = crate::Context::new(1 << 20);
    context.host_files = &mut files;
    context.run = &config;
    // SAFETY: all grants, buffers and the context pin their extents through delivery.
    unsafe {
        let spawn = |context: &mut crate::Context, site| {
            hirsel_core_exec_spawn(
                context,
                1,
                &bytes(b"pinned"),
                &bytes(b""),
                &bytes(b""),
                &bytes(b"work"),
                &bytes(b""),
                0,
                0,
                0,
                site,
            )
        };

        // Untampered copy runs on the fast path.
        let started = spawn(&mut context, 701);
        assert_eq!(started.outcome.error, IoError::OK);
        let waited = hirsel_core_exec_wait(&mut context, started.outcome.value, 0, 0, 0, 702);
        assert_eq!((waited.error, waited.value), (IoError::OK, 41));
        crate::net::hirsel_core_socket_drop(&mut context, started.outcome.value);

        // Clearing UF_IMMUTABLE without changing bytes triggers a re-hash and still runs the pin.
        assert_eq!(
            libc::chflags(copy_path.as_ptr(), original_flags & !libc::UF_IMMUTABLE),
            0
        );
        let cleared = spawn(&mut context, 703);
        assert_eq!(cleared.outcome.error, IoError::OK);
        let waited = hirsel_core_exec_wait(&mut context, cleared.outcome.value, 0, 0, 0, 704);
        assert_eq!((waited.error, waited.value), (IoError::OK, 41));
        crate::net::hirsel_core_socket_drop(&mut context, cleared.outcome.value);

        // Rewriting the copy in place with altered bytes must be refused.
        assert_eq!(libc::chmod(copy_path.as_ptr(), 0o700), 0);
        std::fs::write(copy_os, &altered_bytes).unwrap();
        let rewritten = spawn(&mut context, 705);
        assert_eq!(rewritten.outcome.error.tag, 4);
        assert_eq!(rewritten.pipes, [0; 3]);

        // Re-applying mode 0500 and UF_IMMUTABLE on the altered copy must still be refused.
        assert_eq!(libc::chmod(copy_path.as_ptr(), 0o500), 0);
        assert_eq!(libc::chflags(copy_path.as_ptr(), original_flags), 0);
        let relocked = spawn(&mut context, 706);
        assert_eq!(relocked.outcome.error.tag, 4);
        assert_eq!(relocked.pipes, [0; 3]);

        // Unlinking and replacing the copy with a fresh inode must also be refused.
        assert_eq!(
            libc::chflags(copy_path.as_ptr(), original_flags & !libc::UF_IMMUTABLE),
            0
        );
        std::fs::remove_file(copy_os).unwrap();
        std::fs::write(copy_os, &altered_bytes).unwrap();
        assert_eq!(libc::chmod(copy_path.as_ptr(), 0o500), 0);
        let replaced = spawn(&mut context, 707);
        assert_eq!(replaced.outcome.error.tag, 4);
        assert_eq!(replaced.pipes, [0; 3]);

        assert!(files.sockets.is_empty());
    }
}
