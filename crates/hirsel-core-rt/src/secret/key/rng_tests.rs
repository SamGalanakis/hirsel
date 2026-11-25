use super::*;
use hirsel_core_net::key_wrap::SignAlg;
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair},
};

/// Fail only the child's getrandom syscall. This reaches SystemRandom inside
/// ring, whereas the host.random test seam only intercepts Hirsel's adapter.
fn fail_getrandom() {
    let filter = [
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
            k: libc::SECCOMP_RET_ERRNO | libc::EIO as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
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
        use hirsel_core_net::p256_key::HostSigner;
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
            let key = HostSigner::load(doc.as_ref()).unwrap();
            fail_getrandom();
            if let Err(error) = key.sign(b"sample") {
                let _ = key_failure(error);
            }
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
        let output = crate::test_process::command()
            .args(["--exact", TEST, "--nocapture"])
            .env(CASE, stage)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(RETURNED), "{stage}: {stderr}");
        assert_eq!(output.status.code(), Some(102), "{stage}: {stderr}");
        assert!(stderr.contains(diagnostic), "{stage}: {stderr}");
    }
}
