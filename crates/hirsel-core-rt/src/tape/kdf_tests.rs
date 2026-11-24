use super::*;
use crate::secret::{
    Granted, MAC_PROVIDER, MacTape, Origin, Secret, hirsel_core_secret_drop,
    hirsel_core_secret_hmac_sha256, hirsel_core_secret_seal,
    kdf::{
        hirsel_core_secret_derive_argon2id, hirsel_core_secret_derive_hkdf_sha256,
        hirsel_core_secret_derive_hmac_sha256,
    },
    key::hirsel_core_secret_wrap,
};
use crate::steps::{hirsel_core_step_catch, hirsel_core_step_enter, hirsel_core_step_leave};
use hirsel_core_net::{SecretGrant, SecretPolicy};

const IKM: &[u8] = &[0x0b; 22];
const PASSWORD: &[u8] = b"password";
const SALT: &[u8] = b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c";
const INFO: &[u8] = b"\xf0\xf1\xf2\xf3\xf4\xf5\xf6\xf7\xf8\xf9";
const OKM_HEX: &str =
    "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865";
const ARGON_TAG_HEX: &str = "6d093c501fd5999645e0ea3bf620d7b8be7fd2db59c20d9fff9539da2bf57037";
// Python hmac.digest with RFC 5869 A.1's OKM and the Argon2_id tag in libargon2 src/test.c.
const HKDF_MAC: &str = "f24d157d396fd46cc712da839e284d3daddf384b77873b47c056c590c208f95a";
const ARGON_MAC: &str = "ded0fe8e0ae5d825ccb4f9278d7837ba650705bdd2b02d129c256f916b53b391";
const CHARGE: u64 = 262_208;

fn view(bytes: &[u8]) -> Buffer {
    Buffer {
        data: bytes.as_ptr().cast_mut(),
        len: bytes.len(),
        capacity: bytes.len(),
    }
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn kdf_grants_charge_before_issue_and_replay_without_keys() {
    let dir = tempfile::tempdir().unwrap();
    let ikm_path = dir.path().join("ikm");
    let password_path = dir.path().join("phrase");
    std::fs::write(&ikm_path, IKM).unwrap();
    std::fs::write(&password_path, PASSWORD).unwrap();
    let grants = [
        SecretGrant::new("ikm", &format!("file:{}", ikm_path.display()), "kdf").unwrap(),
        SecretGrant::new(
            "phrase",
            &format!("file:{}", password_path.display()),
            "kdf",
        )
        .unwrap(),
    ];
    let mut header = header(REVISION);
    header.kind = KIND_RUN;
    header.secret_grants.extend(grants.clone());
    let mut store = Store::default();

    for mode in [Mode::Record, Mode::Replay] {
        in_task(mode == Mode::Record, &mut |context| {
            // SAFETY: this task pins the store, request inputs and writable
            // outputs for the synchronous calls.
            unsafe {
                let (provider, handle) = store.dir();
                let dir = Inner { provider, handle };
                let input = match mode {
                    Mode::Record => TapeInput::Record {
                        dir,
                        name: b"t",
                        header: &header,
                    },
                    Mode::Replay => TapeInput::Replay(ReplayInput::open(dir, b"t").unwrap()),
                };
                let (tape, startup) = Tape::open_with_startup(REVISION, input, &expect()).unwrap();
                let [ikm, password] = grants.each_ref().map(|grant| {
                    let (_, handle) = tape
                        .on_tape(crate::entry::SECRET, (MAC_PROVIDER, 0))
                        .unwrap();
                    let ordinal = ordinal_of(handle) as u64;
                    let secret = if mode == Mode::Record {
                        let mut secret = Granted::read(grant.clone(), 0).unwrap().secret;
                        secret.origin = Origin::Granted(ordinal);
                        secret
                    } else {
                        let policy = &startup
                            .secret_grants
                            .iter()
                            .find(|saved| saved.parameter == grant.parameter)
                            .unwrap()
                            .policy;
                        Secret::new(
                            Origin::Granted(ordinal),
                            SecretPolicy::parse(policy).unwrap(),
                            None,
                            None,
                        )
                    };
                    (*(*context).host_files)
                        .registry
                        .run_grants()
                        .secrets
                        .lock()
                        .unwrap()
                        .register(secret)
                });
                let hkdf = move |parent, len, uses| {
                    hirsel_core_secret_derive_hkdf_sha256(
                        context,
                        parent,
                        &view(SALT),
                        &view(INFO),
                        len,
                        &view(uses),
                        10,
                    )
                };
                let argon = move |parent, uses| {
                    hirsel_core_secret_derive_argon2id(
                        context,
                        parent,
                        &view(b"somesalt"),
                        256,
                        2,
                        2,
                        32,
                        &view(uses),
                        11,
                    )
                };
                let hmac = move |parent, uses| {
                    hirsel_core_secret_derive_hmac_sha256(
                        context,
                        parent,
                        &view(b""),
                        &view(b"x"),
                        &view(uses),
                        12,
                    )
                };
                let mac = move |key, expected| {
                    let mut tag = [0; 32];
                    let outcome = hirsel_core_secret_hmac_sha256(
                        context,
                        key,
                        &view(b"x"),
                        tag.as_mut_ptr(),
                        13,
                    );
                    assert_eq!(outcome.error.tag, 0);
                    assert_eq!(tag.as_slice(), unhex(expected));
                };

                let counted = Counted::new(None);
                (*(*context).host_files).secret_mac_tape = Some(MacTape {
                    provider: counted.provider(),
                    handle: 0,
                });
                for uses in [
                    b"tls_send(alpha.test)".as_slice(),
                    b"sign(ed25519)",
                    b"wrap",
                    b"mtls_client_id",
                ] {
                    assert_eq!(hkdf(ikm, 42, uses).error.tag, 1);
                    assert_eq!(argon(password, uses).error.tag, 1);
                    assert_eq!(hmac(ikm, uses).error.tag, 1);
                }
                let narrowed = hirsel_core_secret_seal(context, &view(b""), &view(b"mac"), 14);
                assert_eq!(narrowed.error.tag, 0);
                assert_eq!(hkdf(narrowed.value, 42, b"mac").error.tag, 1);
                assert_eq!(argon(narrowed.value, b"mac").error.tag, 1);
                assert_eq!(hmac(narrowed.value, b"mac").error.tag, 1);
                hirsel_core_secret_drop(context, narrowed.value);
                for len in [0, 8161] {
                    assert_eq!(hkdf(ikm, len, b"mac").error.tag, 1);
                }

                (*context).limit_enter(0).unwrap();
                assert!(hirsel_core_step_enter(context, 512));
                assert!(crate::steps::take_many(context, 1, 11));
                assert_ne!(argon(password, b"mac").status, 0);
                assert!(hirsel_core_step_catch(context));
                (*context).limit_exit();
                (*context).limit_enter(CHARGE - 1).unwrap();
                let refused = argon(password, b"mac");
                assert_eq!(
                    (refused.error.tag, refused.value, refused.written),
                    (2, CHARGE, CHARGE - 1)
                );
                assert_eq!((*context).available(), CHARGE - 1);
                (*context).limit_exit();
                assert_eq!(counted.calls.get(), 0);

                (*(*context).host_files).secret_mac_tape = Some(tape.secret_mac());
                let hkdf_child = hkdf(ikm, 42, b"mac");
                assert_eq!(hkdf_child.error.tag, 0);
                mac(hkdf_child.value, HKDF_MAC);

                let mut kek_bytes = [0x9c; 32];
                let kek = hirsel_core_secret_seal(
                    context,
                    &Buffer {
                        data: kek_bytes.as_mut_ptr(),
                        len: 32,
                        capacity: 32,
                    },
                    &view(b"wrap"),
                    14,
                );
                assert_eq!(kek.error.tag, 0);
                assert_eq!(
                    hirsel_core_secret_wrap(
                        context,
                        kek.value,
                        hkdf_child.value,
                        &mut crate::net::empty_buffer(),
                        15
                    )
                    .error
                    .tag,
                    1
                );
                hirsel_core_secret_drop(context, kek.value);
                hirsel_core_secret_drop(context, hkdf_child.value);

                (*context).limit_enter(CHARGE).unwrap();
                assert!(hirsel_core_step_enter(context, 513));
                assert!(crate::steps::take_many(context, 1, 11));
                let argon_child = argon(password, b"mac");
                assert_eq!(argon_child.error.tag, 0);
                assert_eq!(crate::steps::remaining(&*context), Some(0));
                hirsel_core_step_leave(context);
                assert_eq!((*context).available(), CHARGE - 64);
                mac(argon_child.value, ARGON_MAC);
                hirsel_core_secret_drop(context, argon_child.value);
                assert_eq!((*context).available(), CHARGE);
                (*context).limit_exit();
                tape.end(&Ending::returned(&[])).unwrap();
            }
        });
        if mode == Mode::Record {
            std::fs::remove_file(&ikm_path).unwrap();
            std::fs::remove_file(&password_path).unwrap();
            let frames = tape_frames(store.file());
            let delivered: Vec<_> = frames
                .iter()
                .filter_map(|frame| match frame {
                    Frame::Op {
                        op: SECRET_KDF,
                        out,
                        ..
                    } => Some(out),
                    _ => None,
                })
                .collect();
            assert_eq!(delivered.len(), 2);
            assert!(
                delivered
                    .iter()
                    .all(|out| out.status == OK && out.payload.is_empty())
            );
            for (raw, hex, b64) in [
                (IKM, "0b".repeat(22), "CwsLCwsLCwsLCwsLCwsLCwsLCwsLCw=="),
                (PASSWORD, "70617373776f7264".into(), "cGFzc3dvcmQ="),
                (
                    &unhex(OKM_HEX),
                    OKM_HEX.into(),
                    "PLJfJfqs1XqQQ09k0DYvKi0tCpDPGlpMXbAtVuzExb80AHII1biHGFhl",
                ),
                (
                    &unhex(ARGON_TAG_HEX),
                    ARGON_TAG_HEX.into(),
                    "bQk8UB/VmZZF4Oo79iDXuL5/0ttZwg2f/5U52iv1cDc=",
                ),
            ] {
                for absent in [raw, hex.as_bytes(), b64.as_bytes()] {
                    assert!(
                        !store
                            .file()
                            .windows(absent.len())
                            .any(|window| window == absent)
                    );
                }
            }
        }
    }
}
