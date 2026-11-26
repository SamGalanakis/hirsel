use super::*;
use hirsel_core_net::key_wrap::SignAlg;
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair},
};
use std::fs::OpenOptions;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::FileExt;

fn getrandom_bpf(action: u32) -> [libc::sock_filter; 4] {
    [
        libc::sock_filter {
            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
            jt: 0,
            jf: 0,
            k: 0, // struct seccomp_data::nr
        },
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: libc::SYS_getrandom as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: action,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ]
}

/// Fail only the child's getrandom syscall. This reaches SystemRandom inside
/// ring, whereas the host.random test seam only intercepts Hirsel's adapter.
fn fail_getrandom() {
    let filter = getrandom_bpf(libc::SECCOMP_RET_ERRNO | libc::EIO as u32);
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr().cast_mut(),
    };
    // SAFETY: the filter lives through the syscall. It applies to this child
    // thread only, and the kernel copies and validates the program.
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        assert_eq!(
            libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program),
            0,
            "{}",
            std::io::Error::last_os_error()
        );
    }
}

/// Intercept getrandom on this thread and answer it from a monitor thread.
fn notify_getrandom() -> OwnedFd {
    let filter = getrandom_bpf(libc::SECCOMP_RET_USER_NOTIF);
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr().cast_mut(),
    };
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        let fd = libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_NEW_LISTENER,
            &program,
        );
        assert!(fd >= 0, "{}", std::io::Error::last_os_error());
        // seccomp returned a fresh listener descriptor owned by this test.
        OwnedFd::from_raw_fd(fd as i32)
    }
}

/// Let the first intercepted getrandom succeed, then fail each later one.
fn fail_signing_getrandom() {
    let (send, receive) = std::sync::mpsc::channel::<OwnedFd>();
    // Start the monitor before installing the filter so it stays unfiltered.
    let _ = std::thread::spawn(move || {
        let listener = receive.recv().unwrap();
        let fd = listener.as_raw_fd();
        let mut first = true;
        loop {
            let mut notification: libc::seccomp_notif = unsafe { std::mem::zeroed() };
            if unsafe { libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_RECV, &mut notification) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                panic!("seccomp notification receive: {error}");
            }
            let mut response = libc::seccomp_notif_resp {
                id: notification.id,
                val: 0,
                error: -libc::EIO,
                flags: 0,
            };
            if first {
                response.error = 0;
                response.flags = libc::SECCOMP_USER_NOTIF_FLAG_CONTINUE as u32;
                first = false;
            }
            if unsafe { libc::ioctl(fd, libc::SECCOMP_IOCTL_NOTIF_SEND, &response) } != 0 {
                panic!(
                    "seccomp notification send: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    });
    send.send(notify_getrandom()).unwrap();
}

const SIGN_OUTPUT: &str = "HIRSEL_TEST_P256_SIGN_OUTPUT";
const SIGN_SENTINEL: [u8; 64] = [0x5a; 64];

/// The provider stops with _exit. The parent reads the issued request's buffer
/// length and storage from the shared file after the child has exited.
#[repr(C)]
struct SignOutput {
    request: Request,
    bytes: [u8; 64],
}

#[test]
fn host_p256_key_load_rng_failure_ends_run() {
    const CASE: &str = "HIRSEL_TEST_P256_LOAD_RNG_FAILURE";
    const TEST: &str = "secret::key::rng_tests::host_p256_key_load_rng_failure_ends_run";
    const RETURNED: &str = "secret.sign returned an outcome";
    if std::env::var_os(CASE).is_some() {
        let doc =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .unwrap();
        let secret = Secret::new(
            Origin::root(Made::Sealed(0)),
            SecretPolicy::parse("sign(p256)").unwrap(),
            Some(Arc::new(SecretBytes::new(Zeroizing::new(
                doc.as_ref().to_vec(),
            )))),
            None,
        );
        let args = KeyRequest {
            key: &secret,
            op: KeyOp::Sign {
                alg: SignAlg::P256,
                random: 0,
                source: SigningRandom::Host,
                msg: b"sample",
            },
        };
        let mut bytes = [0; 64];
        let mut request = Request {
            operation: crate::provider::SECRET_MAC,
            mode: KEY_SIGN,
            handle: 0,
            path: (&raw const args).cast(),
            path_len: std::mem::size_of::<KeyRequest<'_>>(),
            buffer: Buffer {
                data: bytes.as_mut_ptr(),
                len: 0,
                capacity: bytes.len(),
            },
            site: 0,
        };

        fail_getrandom();
        // SAFETY: args, the borrowed key and the writable buffer are pinned
        // through the synchronous issue. Issue must stop before any delivery.
        let result = unsafe { key_issue(&mut request) };
        eprintln!("{RETURNED}: {:?}", result.error);
        return;
    }
    let output = crate::test_process::command()
        .args(["--exact", TEST, "--nocapture"])
        .env(CASE, "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains(RETURNED), "{stderr}");
    assert_eq!(output.status.code(), Some(102), "{stderr}");
    assert!(
        stderr.contains("secret.sign: P-256 key loading: RNG failed"),
        "{stderr}"
    );
}

#[test]
fn host_p256_generation_and_signing_rng_failure_end_run() {
    const CASE: &str = "HIRSEL_TEST_P256_GENERATE_OR_SIGN_RNG_FAILURE";
    const TEST: &str =
        "secret::key::rng_tests::host_p256_generation_and_signing_rng_failure_end_run";
    const RETURNED: &str = "provider returned after RNG failure";
    if let Ok(stage) = std::env::var(CASE) {
        if stage == "generation" {
            let mut public = [0; 65];
            let mut call = crate::secret::p256_generation::Generation {
                random: 0,
                policy: String::new(),
                document: None,
            };
            let mut request = Request {
                operation: crate::provider::MEDIA_P256_KEY,
                mode: 0,
                handle: 0,
                path: (&raw mut call).cast(),
                path_len: std::mem::size_of_val(&call),
                buffer: Buffer {
                    data: public.as_mut_ptr(),
                    len: 0,
                    capacity: public.len(),
                },
                site: 0,
            };
            fail_getrandom();
            let provider = crate::secret::p256_generation::PROVIDER;
            // SAFETY: each operand is pinned and private to the test task.
            let _ = unsafe { crate::provider::issue(&provider, &mut request, 0) };
        } else {
            let doc = EcdsaKeyPair::generate_pkcs8(
                &ECDSA_P256_SHA256_FIXED_SIGNING,
                &SystemRandom::new(),
            )
            .unwrap();
            let secret = Secret::new(
                Origin::root(Made::Sealed(0)),
                SecretPolicy::parse("sign(p256)").unwrap(),
                Some(Arc::new(SecretBytes::new(Zeroizing::new(
                    doc.as_ref().to_vec(),
                )))),
                None,
            );
            let args = KeyRequest {
                key: &secret,
                op: KeyOp::Sign {
                    alg: SignAlg::P256,
                    random: 0,
                    source: SigningRandom::Host,
                    msg: b"sample",
                },
            };
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(std::env::var_os(SIGN_OUTPUT).unwrap())
                .unwrap();
            // SAFETY: the parent sized the file for this mapping and only
            // reads it after exit. This child owns the mapping until exit.
            let mapping = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    std::mem::size_of::<SignOutput>(),
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    file.as_raw_fd(),
                    0,
                )
            };
            assert_ne!(mapping, libc::MAP_FAILED);
            let output = mapping.cast::<SignOutput>();
            let bytes = unsafe { &raw mut (*output).bytes };
            let request = Request {
                operation: crate::provider::SECRET_MAC,
                mode: KEY_SIGN,
                handle: 0,
                path: (&raw const args).cast(),
                path_len: std::mem::size_of::<KeyRequest<'_>>(),
                buffer: Buffer {
                    data: bytes.cast(),
                    len: 0,
                    capacity: SIGN_SENTINEL.len(),
                },
                site: 0,
            };
            // SAFETY: Both fields are aligned and backed by the writable mapping.
            unsafe {
                output.write(SignOutput {
                    request,
                    bytes: SIGN_SENTINEL,
                })
            };
            fail_signing_getrandom();
            let provider = crate::secret::MAC_PROVIDER;
            // The first intercepted draw loads the key; the sign draw fails.
            // SAFETY: args, the borrowed key and the writable buffer are pinned
            // through the synchronous issue. Issue must stop before any delivery.
            let _ = unsafe { crate::provider::issue(&provider, &mut (*output).request, 0) };
        }
        eprintln!("{RETURNED}");
        return;
    }
    for (stage, diagnostic) in [
        (
            "generation",
            "secret.p256_key: P-256 generation: ring Unspecified",
        ),
        ("signing", "secret.sign: P-256 signing: ring Unspecified"),
    ] {
        let mut child = crate::test_process::command();
        child
            .args(["--exact", TEST, "--nocapture"])
            .env(CASE, stage);
        let signing = (stage == "signing").then(|| {
            let file = tempfile::NamedTempFile::new().unwrap();
            file.as_file()
                .set_len(std::mem::size_of::<SignOutput>() as u64)
                .unwrap();
            child.env(SIGN_OUTPUT, file.path());
            file
        });
        let output = child.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(RETURNED), "{stage}: {stderr}");
        assert_eq!(output.status.code(), Some(102), "{stage}: {stderr}");
        assert!(
            stderr
                .lines()
                .any(|line| line.strip_prefix("run stop: ") == Some(diagnostic)),
            "{stage}: {stderr}"
        );
        if let Some(signing) = signing {
            let file = signing.as_file();
            let mut len = [0; std::mem::size_of::<usize>()];
            let mut bytes = [0; 64];
            file.read_exact_at(
                &mut len,
                std::mem::offset_of!(SignOutput, request.buffer.len) as u64,
            )
            .unwrap();
            file.read_exact_at(&mut bytes, std::mem::offset_of!(SignOutput, bytes) as u64)
                .unwrap();
            assert_eq!(usize::from_ne_bytes(len), 0);
            assert_eq!(bytes, SIGN_SENTINEL);
        }
    }
}
