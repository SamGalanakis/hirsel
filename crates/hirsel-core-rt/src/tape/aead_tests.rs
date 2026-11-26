use super::*;
use crate::secret::{Granted, Made, Origin, Secret, key::*};
use hirsel_core_net::{SecretGrant, SecretPolicy};

// RFC 8439 section 2.8.2.
const KEY: &[u8] = b"\x80\x81\x82\x83\x84\x85\x86\x87\x88\x89\x8a\x8b\x8c\x8d\x8e\x8f\x90\x91\x92\x93\x94\x95\x96\x97\x98\x99\x9a\x9b\x9c\x9d\x9e\x9f";
const NONCE: &[u8] = b"\x07\x00\x00\x00\x40\x41\x42\x43\x44\x45\x46\x47";
const AAD: &[u8] = b"\x50\x51\x52\x53\xc0\xc1\xc2\xc3\xc4\xc5\xc6\xc7";
const PLAIN: &[u8] = b"\x4c\x61\x64\x69\x65\x73\x20\x61\x6e\x64\x20\x47\x65\x6e\x74\x6c\x65\x6d\x65\x6e\x20\x6f\x66\x20\x74\x68\x65\x20\x63\x6c\x61\x73\x73\x20\x6f\x66\x20\x27\x39\x39\x3a\x20\x49\x66\x20\x49\x20\x63\x6f\x75\x6c\x64\x20\x6f\x66\x66\x65\x72\x20\x79\x6f\x75\x20\x6f\x6e\x6c\x79\x20\x6f\x6e\x65\x20\x74\x69\x70\x20\x66\x6f\x72\x20\x74\x68\x65\x20\x66\x75\x74\x75\x72\x65\x2c\x20\x73\x75\x6e\x73\x63\x72\x65\x65\x6e\x20\x77\x6f\x75\x6c\x64\x20\x62\x65\x20\x69\x74\x2e";
const SEALED: &[u8] = b"\xd3\x1a\x8d\x34\x64\x8e\x60\xdb\x7b\x86\xaf\xbc\x53\xef\x7e\xc2\xa4\xad\xed\x51\x29\x6e\x08\xfe\xa9\xe2\xb5\xa7\x36\xee\x62\xd6\x3d\xbe\xa4\x5e\x8c\xa9\x67\x12\x82\xfa\xfb\x69\xda\x92\x72\x8b\x1a\x71\xde\x0a\x9e\x06\x0b\x29\x05\xd6\xa5\xb6\x7e\xcd\x3b\x36\x92\xdd\xbd\x7f\x2d\x77\x8b\x8c\x98\x03\xae\xe3\x28\x09\x1b\x58\xfa\xb3\x24\xe4\xfa\xd6\x75\x94\x55\x85\x80\x8b\x48\x31\xd7\xbc\x3f\xf4\xde\xf0\x8e\x4b\x7a\x9d\xe5\x76\xd2\x65\x86\xce\xc6\x4b\x61\x16\x1a\xe1\x0b\x59\x4f\x09\xe2\x6a\x7e\x90\x2e\xcb\xd0\x60\x06\x91";

fn view(bytes: &[u8]) -> Buffer {
    Buffer {
        data: bytes.as_ptr().cast_mut(),
        len: bytes.len(),
        capacity: bytes.len(),
    }
}

#[test]
fn aead_grant_uses_and_refusals_record_and_replay() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("key");
    std::fs::write(&key_path, KEY).unwrap();
    let policy = "aead(chacha20-poly1305)";
    let grant = SecretGrant::new("key", &format!("file:{}", key_path.display()), policy).unwrap();
    let mut h = header(REVISION);
    h.kind = KIND_RUN;
    h.secret_grants.push(grant.clone());
    let mut store = Store::default();

    for mode in [Mode::Record, Mode::Replay] {
        in_task(mode == Mode::Record, &mut |context| {
            // SAFETY: this task pins the tape's store and every input and output
            // carrier for each synchronous call.
            unsafe {
                let (provider, handle) = store.dir();
                let dir = Inner { provider, handle };
                let input = match mode {
                    Mode::Record => TapeInput::Record {
                        dir,
                        name: b"t",
                        header: &h,
                    },
                    Mode::Replay => TapeInput::Replay(ReplayInput::open(dir, b"t").unwrap()),
                };
                let (tape, startup) = Tape::open_with_startup(REVISION, input, &expect()).unwrap();
                let (_, handle) = tape
                    .on_tape(crate::entry::SECRET, (crate::secret::MAC_PROVIDER, 0))
                    .unwrap();
                let files = &mut *(*context).host_files;
                files.secret_mac_tape = Some(tape.secret_mac());
                let mut key = if mode == Mode::Record {
                    Granted::read(grant.clone(), 0).unwrap().secret
                } else {
                    assert!(!key_path.exists());
                    Secret::new(
                        Origin::root(Made::Granted(0)),
                        SecretPolicy::parse(&startup.secret_grants[0].policy).unwrap(),
                        None,
                        None,
                    )
                };
                key.origin = Origin::root(Made::Granted(ordinal_of(handle) as u64));
                let mut registry = files.registry.run_grants().secrets.lock().unwrap();
                let allowed = registry.register(key.clone());
                let denied = registry.register(Secret {
                    policy: SecretPolicy::parse("mac").unwrap(),
                    ..key.clone()
                });
                let open_only = registry.register(Secret {
                    policy: SecretPolicy::parse("open(chacha20-poly1305)").unwrap(),
                    ..key
                });
                let wrong_len = registry.register(Secret::new(
                    Origin::root(Made::Sealed(0)),
                    SecretPolicy::parse(policy).unwrap(),
                    (mode == Mode::Record).then(|| {
                        std::sync::Arc::new(hirsel_core_net::secret_aead::SecretBytes::new(
                            zeroize::Zeroizing::new(vec![7; 16]),
                        ))
                    }),
                    None,
                ));
                drop(registry);

                let nonce = view(NONCE);
                let short_nonce = view(&NONCE[..11]);
                let long_nonce = view(b"nonce13bytes!");
                let aad = view(AAD);
                let plain = view(PLAIN);
                let sealed = view(SEALED);
                let mut output = [0xa5; 131];
                output[0] = b'x';
                let mut out = Buffer {
                    data: output.as_mut_ptr(),
                    len: 1,
                    capacity: 130,
                };
                let seal = |key, nonce, out| {
                    hirsel_core_secret_aead_seal(context, key, 0, nonce, &aad, &plain, out, 10)
                };
                assert_eq!(seal(denied, &short_nonce, &mut out).error.tag, 1);
                let narrowed = crate::secret::hirsel_core_secret_narrow(
                    context,
                    allowed,
                    &view(b"open(chacha20-poly1305)"),
                    12,
                );
                assert_eq!(narrowed.error.tag, 0);
                assert_eq!(seal(narrowed.value, &nonce, &mut out).error.tag, 1);
                assert_eq!(seal(open_only, &nonce, &mut out).error.tag, 1);
                for (key, alg) in [(allowed, 2), (denied, 0)] {
                    let outcome = hirsel_core_secret_aead_open(
                        context, key, alg, &nonce, &aad, &sealed, &mut out, 11,
                    );
                    assert_eq!(outcome.error.tag, 1);
                }
                for nonce in [&short_nonce, &long_nonce] {
                    assert_eq!(seal(allowed, nonce, &mut out).error.tag, 2);
                }
                let capacity = seal(wrong_len, &nonce, &mut out);
                assert_eq!(
                    (capacity.error.tag, capacity.value, capacity.written),
                    (4, 130, 129)
                );
                assert_eq!(out.len, 1);
                assert!(output[1..].iter().all(|&b| b == 0xa5));

                out.capacity = output.len();
                assert_eq!(seal(wrong_len, &nonce, &mut out).error.tag, 2);
                assert_eq!(out.len, 1);
                let done = seal(allowed, &nonce, &mut out);
                assert_eq!((done.error.tag, done.value), (0, 130));
                assert_eq!(out.len, 131);
                assert_eq!(output[0], b'x');
                assert_eq!(&output[1..], SEALED);

                out.len = 1;
                let mut tampered = SEALED.to_vec();
                tampered[SEALED.len() - 1] ^= 1;
                let open = |sealed, out| {
                    hirsel_core_secret_aead_open(context, allowed, 0, &nonce, &aad, sealed, out, 11)
                };
                assert_eq!(open(&view(&tampered), &mut out).error.tag, 3);
                assert_eq!(out.len, 1);
                if mode == Mode::Record {
                    assert!(output[1..1 + PLAIN.len()].iter().all(|&b| b == 0));
                }
                let done = open(&sealed, &mut out);
                assert_eq!((done.error.tag, done.value), (0, PLAIN.len() as u64));
                assert_eq!(out.len, 1 + PLAIN.len());
                assert_eq!(output[0], b'x');
                assert_eq!(&output[1..out.len], PLAIN);
                tape.end(&Ending::returned(&[])).unwrap();
            }
        });
        if mode == Mode::Record {
            std::fs::remove_file(&key_path).unwrap();
            let hex = hirsel_core_ids::hex::encode(KEY);
            // RFC key in RFC 4648 base64.
            let base64 = b"gIGCg4SFhoeIiYqLjI2Oj5CRkpOUlZaXmJmam5ydnp8=";
            for key in [KEY, hex.as_bytes(), base64] {
                assert!(!store.file().windows(key.len()).any(|window| window == key));
            }
            let delivery: Vec<_> = tape_frames(store.file())
                .into_iter()
                .filter_map(|frame| match frame {
                    Frame::Op {
                        op: SECRET_MAC,
                        out,
                        ..
                    } => Some((out.error.0, out.payload)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                delivery,
                [
                    (2, vec![]),
                    (0, SEALED.to_vec()),
                    (3, vec![]),
                    (0, PLAIN.to_vec()),
                ]
            );
        }
    }
}
