//! EXEC-4: the provider verifies and runs the pinned image in place.
use super::*;

fn exec_bytes(prefix: &[u8]) -> Buffer {
    Buffer {
        data: prefix.as_ptr().cast_mut(),
        len: prefix.len(),
        capacity: 0,
    }
}

fn sha256_hex(path: &std::path::Path) -> String {
    use ring::digest::{SHA256, digest};
    let hash: [u8; 32] = digest(&SHA256, &std::fs::read(path).unwrap())
        .as_ref()
        .try_into()
        .unwrap();
    hirsel_core_ids::hex::encode(&hash)
}

unsafe fn drain_pipe(context: &mut crate::Context, pipe: u64, site: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let mut buffer = Buffer {
            data: chunk.as_mut_ptr(),
            len: 0,
            capacity: chunk.len(),
        };
        let got = unsafe {
            crate::net::hirsel_core_host_recv_bounded(
                context,
                pipe,
                &mut buffer,
                chunk.len() as u64,
                0,
                0,
                0,
                site,
            )
        };
        assert_eq!(got.error, IoError::OK);
        if got.value == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..buffer.len]);
    }
    unsafe { crate::net::hirsel_core_socket_drop(context, pipe) };
    out
}

fn with_exec_context(
    grant: &crate::exec::Grant,
    body: impl FnOnce(&mut crate::Context, &crate::host_file::Files),
) {
    use crate::entry::{self, Grant as EntryGrant};
    let mut os = OsProvider::from_streams(None, None);
    let provider = os.provider();
    let grants = [
        EntryGrant {
            kind: entry::EXEC,
            provider: &provider,
            handle: std::ptr::from_ref(grant) as u64,
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
        args: std::ptr::null(),
        args_len: 0,
        grants: std::ptr::null(),
        grants_len: 0,
        provider: &provider,
        state: std::ptr::null_mut(),
        diagnostic: ignore_diagnostic,
        describe: None,
        scheduler: crate::tasks::SchedulerProvider::default(),
        stop_fd: -1,
        capabilities: grants.as_ptr(),
        capabilities_len: grants.len(),
        close_grants: None,
        launch: std::ptr::null(),
        describe_output: None,
    };
    let mut files = crate::host_file::Files::default();
    let mut context = crate::Context::new(1 << 20);
    context.host_files = &mut files;
    context.run = &config;
    body(&mut context, &files);
}

#[test]
fn exec_provider_runs_in_place_and_uses_directional_pipes() {
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
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
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
    char resolved[PATH_MAX];
    size_t n;
#ifdef __APPLE__
    char raw[PATH_MAX];
    uint32_t size = sizeof raw;
    CC_SHA256_CTX state;
    if (_NSGetExecutablePath(raw, &size) || !realpath(raw, resolved)) return 2;
    FILE *self = fopen(resolved, "rb");
    if (!self || !CC_SHA256_Init(&state)) return 2;
    while ((n = fread(block, 1, sizeof block, self))) CC_SHA256_Update(&state, block, n);
    if (ferror(self) || !CC_SHA256_Final(digest, &state)) return 4;
#else
    ssize_t len = readlink("/proc/self/exe", resolved, sizeof resolved - 1);
    if (len <= 0 || (size_t)len >= sizeof resolved) return 2;
    resolved[len] = '\0';
    FILE *self = fopen(resolved, "rb");
    EVP_MD_CTX *state = EVP_MD_CTX_new();
    if (!self || !state || !EVP_DigestInit_ex(state, EVP_sha256(), NULL)) return 2;
    while ((n = fread(block, 1, sizeof block, self))) {
        if (!EVP_DigestUpdate(state, block, n)) return 3;
    }
    if (ferror(self) || !EVP_DigestFinal_ex(state, digest, NULL)) return 4;
#endif
    printf("%s\n", resolved);
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

    #[cfg(target_os = "macos")]
    let forms = {
        // A universal file holds an image per machine: the pin covers them all, and
        // this machine runs its own in place.
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
        vec![universal]
    };
    #[cfg(target_os = "linux")]
    let forms = Vec::new();
    for image in std::iter::once(image).chain(forms) {
        let check = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "provider_tests::exec_provider_tests::exec_provider_runs_in_place_and_uses_directional_pipes",
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
            assert!(refusal.contains("parse image"), "{case}: {refusal}");
        }
    }
}

fn exec_provider_law(image: &std::path::Path) {
    use crate::{
        exec::Grant,
        exec_runtime::{hirsel_core_exec_signal, hirsel_core_exec_spawn, hirsel_core_exec_wait},
        host::exec::Supervisor,
    };
    use hirsel_core_net::exec::Request as PolicyRequest;
    use std::sync::Arc;

    let root = image.parent().unwrap();
    let real_image = std::fs::canonicalize(image).unwrap();
    let profile = root.join("policy.toml");
    let wanted = sha256_hex(image);
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

    std::fs::create_dir(root.join("here")).unwrap();
    std::os::unix::fs::symlink("here", root.join("link")).unwrap();
    let grant = Grant::Native {
        custody: crate::host::exec_helper::Custody::Local(supervisor),
        profiles: Arc::new(profiles),
        rules,
    };
    with_exec_context(&grant, |context, files| unsafe {
        let mut launch = |program: &[u8], path: &[u8]| {
            hirsel_core_exec_spawn(
                context,
                1,
                &exec_bytes(program),
                &exec_bytes(b""),
                &exec_bytes(b""),
                &exec_bytes(b"work"),
                &exec_bytes(path),
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
        let timed = hirsel_core_exec_wait(context, proc, 1, 2, 0, 602);
        assert_eq!(timed.error, IoError { tag: 2, code: 0 });
        let sent = crate::net::hirsel_core_host_send_pair(
            context,
            stdin,
            &exec_bytes(b"x"),
            &exec_bytes(b"y"),
            0,
            0,
            0,
            603,
        );
        assert_eq!((sent.error, sent.value), (IoError::OK, 2));
        crate::net::hirsel_core_socket_drop(context, stdin);
        let waited = hirsel_core_exec_wait(context, proc, 0, 0, 0, 604);
        assert_eq!((waited.error, waited.value), (IoError::OK, 23));

        let stdout_bytes = drain_pipe(context, stdout, 605);
        let stdout_text = std::str::from_utf8(&stdout_bytes).unwrap();
        let (executed_path, reported_digest) = stdout_text.split_once('\n').unwrap();
        assert_eq!(executed_path, real_image.to_str().unwrap());
        assert!(!executed_path.contains("memfd:"));
        assert!(!executed_path.contains("hirsel-exec-"));
        assert_eq!(reported_digest, wanted);

        let stderr_bytes = drain_pipe(context, stderr, 606);
        assert_eq!(stderr_bytes, b"digest");

        let cached = hirsel_core_exec_wait(context, proc, 1, 2, 0, 607);
        assert_eq!((cached.error, cached.value), (IoError::OK, 23));
        assert_eq!(
            hirsel_core_exec_signal(context, proc, 2, 608).error,
            IoError::OK
        );
        crate::net::hirsel_core_socket_drop(context, proc);
        assert!(files.sockets.is_empty());
    });
}

/// EXEC-4 on both Linux and macOS: changing the binary after launch (same size
/// with a new mtime, replacement by a new inode, or swapping in a symlink)
/// causes later spawns to refuse with `Other(errno)`.
#[test]
fn exec_provider_refuses_post_launch_modification_replacement_and_symlink_swap() {
    use crate::{
        exec::Grant,
        exec_runtime::{hirsel_core_exec_spawn, hirsel_core_exec_wait},
        host::exec::Supervisor,
    };
    use hirsel_core_net::exec::{PinnedImage, Request as PolicyRequest};
    use std::{
        io::Write,
        os::unix::fs::{MetadataExt, PermissionsExt},
        process::Command,
        sync::Arc,
        time::Duration,
    };

    crate::openat2_required::require();
    if crate::test_process::run_isolated(
        "provider_tests::exec_provider_tests::exec_provider_refuses_post_launch_modification_replacement_and_symlink_swap",
    ) {
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let compile = |name: &str, tag: &str, code: u8| {
        let source = root.join(format!("{name}.c"));
        let binary = root.join(name);
        std::fs::write(
            &source,
            format!(
                "#include <stdio.h>\nstatic const char TAG[] = \"{tag}\";\nint main(void) {{ return TAG[0] == 'H' ? {code} : 1; }}\n"
            ),
        )
        .unwrap();
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
    let pinned = compile("pinned", "HIRSEL_EXEC4_AAAA", 41);
    let altered = compile("altered", "HIRSEL_EXEC4_DIFF", 99);
    let original_bytes = std::fs::read(&pinned).unwrap();
    let original_meta = std::fs::metadata(&pinned).unwrap();
    let original_ino = original_meta.ino();
    let original_len = original_meta.len();
    let original_mtime = original_meta.modified().unwrap();

    let wanted = sha256_hex(&pinned);
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
    let grant = Grant::Native {
        custody: crate::host::exec_helper::Custody::Local(supervisor),
        profiles: Arc::new(profiles),
        rules,
    };

    with_exec_context(&grant, |context, files| unsafe {
        let spawn = |context: &mut crate::Context, site| {
            hirsel_core_exec_spawn(
                context,
                1,
                &exec_bytes(b"pinned"),
                &exec_bytes(b""),
                &exec_bytes(b""),
                &exec_bytes(b"work"),
                &exec_bytes(b""),
                0,
                0,
                0,
                site,
            )
        };

        // Untampered binary runs on the fast path.
        let started = spawn(context, 701);
        assert_eq!(started.outcome.error, IoError::OK);
        let waited = hirsel_core_exec_wait(context, started.outcome.value, 0, 0, 0, 702);
        assert_eq!((waited.error, waited.value), (IoError::OK, 41));
        crate::net::hirsel_core_socket_drop(context, started.outcome.value);

        // Touching mtime without changing bytes triggers a re-hash and still runs the pin.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&pinned)
            .unwrap();
        file.set_modified(original_mtime + Duration::from_secs(2))
            .unwrap();
        drop(file);
        let touched = spawn(context, 703);
        assert_eq!(touched.outcome.error, IoError::OK);
        let waited = hirsel_core_exec_wait(context, touched.outcome.value, 0, 0, 0, 704);
        assert_eq!((waited.error, waited.value), (IoError::OK, 41));
        crate::net::hirsel_core_socket_drop(context, touched.outcome.value);

        // Same size with a new mtime on the same inode: re-hashes and refuses with Other(errno).
        let mut mutated = original_bytes.clone();
        let needle = b"HIRSEL_EXEC4_AAAA";
        let pos = mutated
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("marker in compiled binary");
        mutated[pos..pos + needle.len()].copy_from_slice(b"HIRSEL_EXEC4_BBBB");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&pinned)
            .unwrap();
        file.write_all(&mutated).unwrap();
        file.set_modified(original_mtime + Duration::from_secs(4))
            .unwrap();
        drop(file);
        let mutated_meta = std::fs::metadata(&pinned).unwrap();
        assert_eq!(mutated_meta.ino(), original_ino);
        assert_eq!(mutated_meta.len(), original_len);
        assert_ne!(mutated_meta.modified().unwrap(), original_mtime);
        let rewritten = spawn(context, 705);
        assert_eq!(rewritten.outcome.error.tag, 4);
        assert_ne!(rewritten.outcome.error.code, 0);
        assert_eq!(rewritten.pipes, [0; 3]);

        // Replacement by a new inode must also be refused.
        std::fs::remove_file(&pinned).unwrap();
        std::fs::copy(&altered, &pinned).unwrap();
        assert_ne!(std::fs::metadata(&pinned).unwrap().ino(), original_ino);
        let replaced = spawn(context, 706);
        assert_eq!(replaced.outcome.error.tag, 4);
        assert_ne!(replaced.outcome.error.code, 0);
        assert_eq!(replaced.pipes, [0; 3]);

        // Swapping the path to a symlink (even pointing to the original pinned bytes) must refuse.
        let real_copy = root.join("real-copy");
        std::fs::write(&real_copy, &original_bytes).unwrap();
        std::fs::set_permissions(&real_copy, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_file(&pinned).unwrap();
        std::os::unix::fs::symlink(&real_copy, &pinned).unwrap();
        let swapped = spawn(context, 707);
        assert_eq!(swapped.outcome.error.tag, 4);
        assert_eq!(swapped.outcome.error.code, libc::ELOOP as u32);
        assert_eq!(swapped.pipes, [0; 3]);

        // Opening a symlink at launch is also refused by O_NOFOLLOW.
        let fresh = PolicyRequest::parse(&format!("{} pinned", profile.display()))
            .unwrap()
            .load()
            .unwrap();
        assert!(PinnedImage::open(&fresh.profiles["pinned"]).is_err());

        assert!(files.sockets.is_empty());
    });
}

/// EXEC-4: clean in-place execution of `/bin/sh`, `/bin/cat`, and a tool that
/// resolves `$ORIGIN` (Linux) or `@executable_path` (macOS) with an empty env.
#[test]
fn exec_provider_runs_system_shells_and_origin_tools() {
    use crate::{
        exec::Grant,
        exec_runtime::{hirsel_core_exec_spawn, hirsel_core_exec_wait},
        host::exec::Supervisor,
    };
    use hirsel_core_net::exec::Request as PolicyRequest;
    use std::{path::Path, process::Command, sync::Arc};

    crate::openat2_required::require();
    if crate::test_process::run_isolated(
        "provider_tests::exec_provider_tests::exec_provider_runs_system_shells_and_origin_tools",
    ) {
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let helper_src = root.join("helper.c");
    let tool_src = root.join("tool.c");
    let tool = root.join("origin-tool");
    std::fs::write(&helper_src, "int origin_magic(void) { return 42; }\n").unwrap();
    std::fs::write(
        &tool_src,
        "#include <stdio.h>\nint origin_magic(void);\nint main(void) { printf(\"origin=%d\", origin_magic()); return 0; }\n",
    )
    .unwrap();

    #[cfg(target_os = "linux")]
    {
        let lib = root.join("liborigin_helper.so");
        let lib_build = Command::new("cc")
            .args(["-shared", "-fPIC"])
            .arg(&helper_src)
            .args(["-o"])
            .arg(&lib)
            .output()
            .unwrap();
        assert!(
            lib_build.status.success(),
            "{}",
            String::from_utf8_lossy(&lib_build.stderr)
        );
        let tool_build = Command::new("cc")
            .arg(&tool_src)
            .arg(format!("-L{}", root.display()))
            .args(["-lorigin_helper", "-Wl,-rpath,$ORIGIN", "-o"])
            .arg(&tool)
            .output()
            .unwrap();
        assert!(
            tool_build.status.success(),
            "{}",
            String::from_utf8_lossy(&tool_build.stderr)
        );
    }

    #[cfg(target_os = "macos")]
    {
        let lib = root.join("liborigin_helper.dylib");
        let lib_build = Command::new("cc")
            .args(["-dynamiclib"])
            .arg(&helper_src)
            .args([
                "-install_name",
                "@executable_path/liborigin_helper.dylib",
                "-o",
            ])
            .arg(&lib)
            .output()
            .unwrap();
        assert!(
            lib_build.status.success(),
            "{}",
            String::from_utf8_lossy(&lib_build.stderr)
        );
        let tool_build = Command::new("cc")
            .arg(&tool_src)
            .arg(&lib)
            .args(["-o"])
            .arg(&tool)
            .output()
            .unwrap();
        assert!(
            tool_build.status.success(),
            "{}",
            String::from_utf8_lossy(&tool_build.stderr)
        );
    }

    // On macOS `/bin/sh` is a regular platform binary; on Linux `/bin/sh` is a
    // symlink to dash/bash, so resolve it to the regular executable. `/bin/cat`
    // is a regular file on both hosts.
    let sh_path = std::fs::canonicalize("/bin/sh").unwrap();
    let cat_path = Path::new("/bin/cat");
    let sh_pin = sha256_hex(&sh_path);
    let cat_pin = sha256_hex(cat_path);
    let tool_pin = sha256_hex(&tool);

    let profile = root.join("policy.toml");
    std::fs::write(
        &profile,
        format!(
            "[sh]\nimage = {sh:?}\nsha256 = {sh_pin:?}\nargv0 = \"sh\"\nargs = [[\"-c\", \"printf sh-ok\"]]\nenv = {{}}\ndirs = {{ work = {root:?} }}\n\n\
             [cat]\nimage = \"/bin/cat\"\nsha256 = {cat_pin:?}\nargv0 = \"cat\"\nargs = [[]]\nenv = {{}}\ndirs = {{ work = {root:?} }}\n\n\
             [origin]\nimage = {tool:?}\nsha256 = {tool_pin:?}\nargv0 = \"origin\"\nargs = [[]]\nenv = {{}}\ndirs = {{ work = {root:?} }}\n",
            sh = sh_path,
            tool = tool,
            root = root,
        ),
    )
    .unwrap();

    let supervisor = Arc::new(Supervisor::start(crate::exec::probe).unwrap());
    let policy = PolicyRequest::parse(&format!("{} sh,cat,origin", profile.display()))
        .unwrap()
        .load()
        .unwrap();
    let rules = policy.rules();
    let (profiles, _) = policy.pin_images().unwrap();
    let grant = Grant::Native {
        custody: crate::host::exec_helper::Custody::Local(supervisor),
        profiles: Arc::new(profiles),
        rules,
    };

    with_exec_context(&grant, |context, files| unsafe {
        // 1. /bin/sh
        let sh_args = [exec_bytes(b"-c"), exec_bytes(b"printf sh-ok")];
        let sh_args_list = Buffer {
            data: sh_args.as_ptr().cast_mut().cast(),
            len: sh_args.len(),
            capacity: 0,
        };
        let started = hirsel_core_exec_spawn(
            context,
            1,
            &exec_bytes(b"sh"),
            &sh_args_list,
            &exec_bytes(b""),
            &exec_bytes(b"work"),
            &exec_bytes(b""),
            0,
            1,
            0,
            801,
        );
        assert_eq!(started.outcome.error, IoError::OK);
        let [_, stdout, _] = started.pipes;
        let out = drain_pipe(context, stdout, 802);
        let waited = hirsel_core_exec_wait(context, started.outcome.value, 0, 0, 0, 803);
        assert_eq!((waited.error, waited.value), (IoError::OK, 0));
        assert_eq!(out, b"sh-ok");
        crate::net::hirsel_core_socket_drop(context, started.outcome.value);

        // 2. /bin/cat
        let started = hirsel_core_exec_spawn(
            context,
            1,
            &exec_bytes(b"cat"),
            &exec_bytes(b""),
            &exec_bytes(b""),
            &exec_bytes(b"work"),
            &exec_bytes(b""),
            1,
            1,
            0,
            804,
        );
        assert_eq!(started.outcome.error, IoError::OK);
        let [stdin, stdout, _] = started.pipes;
        let sent = crate::net::hirsel_core_host_send_pair(
            context,
            stdin,
            &exec_bytes(b"cat-"),
            &exec_bytes(b"ok"),
            0,
            0,
            0,
            805,
        );
        assert_eq!((sent.error, sent.value), (IoError::OK, 6));
        crate::net::hirsel_core_socket_drop(context, stdin);
        let out = drain_pipe(context, stdout, 806);
        let waited = hirsel_core_exec_wait(context, started.outcome.value, 0, 0, 0, 807);
        assert_eq!((waited.error, waited.value), (IoError::OK, 0));
        assert_eq!(out, b"cat-ok");
        crate::net::hirsel_core_socket_drop(context, started.outcome.value);

        // 3. $ORIGIN (Linux) / @executable_path (macOS) tool with empty env
        let started = hirsel_core_exec_spawn(
            context,
            1,
            &exec_bytes(b"origin"),
            &exec_bytes(b""),
            &exec_bytes(b""),
            &exec_bytes(b"work"),
            &exec_bytes(b""),
            0,
            1,
            0,
            808,
        );
        assert_eq!(started.outcome.error, IoError::OK);
        let [_, stdout, _] = started.pipes;
        let out = drain_pipe(context, stdout, 809);
        let waited = hirsel_core_exec_wait(context, started.outcome.value, 0, 0, 0, 810);
        assert_eq!((waited.error, waited.value), (IoError::OK, 0));
        assert_eq!(out, b"origin=42");
        crate::net::hirsel_core_socket_drop(context, started.outcome.value);

        assert!(files.sockets.is_empty());
    });
}
