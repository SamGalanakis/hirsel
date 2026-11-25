//! TLS-8 PC9's SFrame case: a send import of granted bytes, a frame sealed one ratchet
//! step on, and the same bytes imported again, recorded and replayed without the key.
use super::*;
use crate::media::{
    hirsel_core_media_drop, hirsel_core_packet_seal_into, hirsel_core_schedule_import_sframe,
    hirsel_core_schedule_sframe_sealer,
};
use crate::secret::{Granted, MAC_PROVIDER, Origin, Secret};
use hirsel_core_net::{SecretGrant, SecretPolicy};

const BASE: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
// Python hmac and cryptography over RFC 9605 sections 4.4.2 and 5.1 for the base key
// 00..0f: base_key[1], then KID 5's sframe_key and sframe_salt under it, and "hirsel"
// sealed at CTR 0 under header 50 (TLS-8 PC12's step-1 frame).
const STEP_ONE: &str = "fb75d8d5782da6c6cbf18ac43eca5da9e47f7e6ac7926a78e486226bd2af0f87";
const KID5_KEY: &str = "1a765b572a4d410c6e91d0a59721cb29";
const KID5_SALT: &str = "9cb0edc9894e388a4588bbfe";
const FRAME: &str = "501779b9607a5841322c5952a56cb455d5803b545e1d1b";

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
fn sframe_send_import_replays_without_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base");
    std::fs::write(&path, BASE).unwrap();
    // Two grants of one file: equal bytes, so the second send import is the registry's Spent.
    let policy = "sframe_import(send,4,2)";
    let grants = ["first", "again"]
        .map(|name| SecretGrant::new(name, &format!("file:{}", path.display()), policy).unwrap());
    let mut header = header(REVISION);
    header.kind = KIND_RUN;
    header.secret_grants.extend(grants.clone());
    let mut store = Store::default();
    let mut outcomes = Vec::new();

    for mode in [Mode::Record, Mode::Replay] {
        let mut seen = Vec::new();
        in_task(mode == Mode::Record, &mut |context| {
            // SAFETY: this task pins the store and every carrier through each synchronous call.
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
                (*(*context).host_files).secret_mac_tape = Some(tape.secret_mac());
                let [first, again] = grants.each_ref().map(|grant| {
                    let (_, handle) = tape
                        .on_tape(crate::entry::SECRET, (MAC_PROVIDER, 0))
                        .unwrap();
                    let ordinal = ordinal_of(handle) as u64;
                    let secret = if mode == Mode::Record {
                        let mut secret = Granted::read(grant.clone(), 0).unwrap().secret;
                        secret.origin = Origin::Granted(ordinal);
                        secret
                    } else {
                        let saved = startup
                            .secret_grants
                            .iter()
                            .find(|saved| saved.parameter == grant.parameter)
                            .unwrap();
                        Secret::new(
                            Origin::Granted(ordinal),
                            SecretPolicy::parse(&saved.policy).unwrap(),
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
                let schedule = hirsel_core_schedule_import_sframe(context, 0, first, 10);
                assert_eq!(schedule.error, IoError::OK);
                let sealer =
                    hirsel_core_schedule_sframe_sealer(context, schedule.value, 1, 5, 0, 11);
                assert_eq!(sealer.error, IoError::OK);
                let mut frame = [0; 23];
                frame[0] = 0x50;
                let mut dst = Buffer {
                    data: frame.as_mut_ptr(),
                    len: 1,
                    capacity: frame.len(),
                };
                let sealed = hirsel_core_packet_seal_into(
                    context,
                    sealer.value,
                    0,
                    &view(b"hirsel"),
                    0,
                    &mut dst,
                    0,
                    &view(b""),
                    12,
                );
                assert_eq!((sealed.error, dst.len), (IoError::OK, 23));
                assert_eq!(frame.as_slice(), unhex(FRAME));
                // The registry's Spent: the import consumed the secret (code 0), unlike a
                // spent mark's Spent, which returns custody before issue (code 1).
                let spent = hirsel_core_schedule_import_sframe(context, 0, again, 13);
                assert_eq!((spent.error.tag, spent.error.code), (4, 0));
                seen.push((sealed.value, spent.value, spent.written));
                hirsel_core_media_drop(context, sealer.value);
                hirsel_core_media_drop(context, schedule.value);
                tape.end(&Ending::returned(&[])).unwrap();
                (*(*context).host_files).secret_mac_tape = None;
            }
        });
        outcomes.push(seen);
    }
    assert_eq!(outcomes[0], outcomes[1]);
    let file = store.file();
    for secret in [
        BASE.to_vec(),
        unhex(STEP_ONE),
        unhex(KID5_KEY),
        unhex(KID5_SALT),
    ] {
        assert!(!file.windows(secret.len()).any(|w| w == secret));
    }
    // The seal's delivery is what it appended: the ciphertext and tag after the header.
    assert!(file.windows(22).any(|w| w == &unhex(FRAME)[1..]));
}
