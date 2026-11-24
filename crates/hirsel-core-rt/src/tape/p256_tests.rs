use super::*;
use crate::{
    host_ops::hirsel_core_host_io,
    provide::{hirsel_core_provide_random, random::hirsel_core_provide_random_tape},
    secret::{
        self, hirsel_core_secret_drop, hirsel_core_secret_seal, key::*,
        p256_generation::hirsel_core_secret_p256_key,
    },
};

fn view(bytes: &[u8]) -> Buffer {
    Buffer {
        data: bytes.as_ptr().cast_mut(),
        len: bytes.len(),
        capacity: bytes.len(),
    }
}

fn empty(bytes: &mut [u8]) -> Buffer {
    Buffer {
        data: bytes.as_mut_ptr(),
        len: 0,
        capacity: bytes.len(),
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn dark_key_sign_replays() {
    const P256_SCALAR: &str = "c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721";
    const P256_SIG: &str = "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8";
    const ED25519_DOC: &str = "302e020100300506032b6570042204209d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const ED25519_SIG: &str = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";
    let mut store = Store::default();
    let mut h = header(REVISION);
    h.kind = KIND_RUN;
    let mut host_point = [0; 65];
    let mut host_sig = [0; 64];
    for mode in [Mode::Record, Mode::Replay] {
        in_task(mode == Mode::Record, &mut |context| {
            // SAFETY: the root task and all carriers are pinned through each call.
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
                let (tape, _) = Tape::open_with_startup(REVISION, input, &expect()).unwrap();
                let files = &mut *(*context).host_files;
                files.secret_mac_tape = Some(tape.secret_mac());
                let host = files.registry.grant(secret::MAC_PROVIDER, 0);
                let mut point = [0; 65];
                let mut public = empty(&mut point);
                let p256_use = view(b"sign(p256)");
                let host_key =
                    hirsel_core_secret_p256_key(context, host, &p256_use, &mut public, 7);
                assert_eq!((host_key.error, public.len), (IoError::OK, 65));
                let key = secret::resolve(context, host_key.value);
                assert!(key.origin.opaque());
                assert_eq!(key.bytes.is_some(), mode == Mode::Record);
                let msg = view(b"sample");
                let mut out = [0; 64];
                let mut sig = empty(&mut out);
                let host_signed =
                    hirsel_core_secret_sign(context, host_key.value, 1, host, &msg, &mut sig, 8);
                assert_eq!((host_signed.error, sig.len), (IoError::OK, 64));
                assert!(hirsel_core_net::verify_p256_fixed(&point, b"sample", &out));
                if mode == Mode::Record {
                    host_point = point;
                    host_sig = out;
                } else {
                    assert_eq!(point, host_point);
                    assert_eq!(out, host_sig);
                }
                let random = hirsel_core_provide_random(context, 7, 9).value;
                let control = hirsel_core_provide_random(context, 7, 9).value;
                sig.len = 0;
                let mixed =
                    hirsel_core_secret_sign(context, host_key.value, 1, random, &msg, &mut sig, 10);
                assert_eq!((mixed.error.tag, sig.len), (1, 0));

                let mut scalar = hex(P256_SCALAR);
                let source = view(&scalar);
                let random_tape = hirsel_core_provide_random_tape(context, &source, 11);
                assert_eq!(random_tape.error, IoError::OK);
                scalar.fill(0);
                public.len = 0;
                let virtual_key = hirsel_core_secret_p256_key(
                    context,
                    random_tape.value,
                    &p256_use,
                    &mut public,
                    12,
                );
                assert_eq!(virtual_key.error, IoError::OK);
                let mut document = hex(ED25519_DOC);
                let ed_use = view(b"sign(ed25519)");
                let ed_key = hirsel_core_secret_seal(context, &view(&mut document), &ed_use, 13);
                assert_eq!(ed_key.error, IoError::OK);
                let mut kek = [0x9e; 32];
                let kek = hirsel_core_secret_seal(context, &view(&mut kek), &view(b"wrap"), 14);
                assert_eq!(kek.error, IoError::OK);
                for (key, policy, alg, msg, want) in [
                    (
                        virtual_key.value,
                        p256_use,
                        1,
                        b"sample".as_slice(),
                        P256_SIG,
                    ),
                    (ed_key.value, ed_use, 0, b"".as_slice(), ED25519_SIG),
                ] {
                    let mut wrapped = [0; 256];
                    let mut blob = empty(&mut wrapped);
                    assert_eq!(
                        hirsel_core_secret_wrap(context, kek.value, key, &mut blob, 15).error,
                        IoError::OK
                    );
                    let unwrap = hirsel_core_secret_unwrap(context, kek.value, &blob, &policy, 16);
                    assert_eq!(unwrap.error, IoError::OK);
                    let key = secret::resolve(context, unwrap.value);
                    assert!(!key.origin.opaque());
                    assert_eq!(key.bytes.is_some(), mode == Mode::Record);
                    sig.len = 0;
                    let signed = hirsel_core_secret_sign(
                        context,
                        unwrap.value,
                        alg,
                        random,
                        &view(msg),
                        &mut sig,
                        17,
                    );
                    assert_eq!((signed.error, sig.len), (IoError::OK, 64));
                    assert_eq!(out.as_slice(), hex(want));
                    hirsel_core_secret_drop(context, unwrap.value);
                }
                let mut next = [0; 8];
                let mut independent = [0; 8];
                assert_eq!(
                    hirsel_core_host_io(context, RANDOM, random, &mut empty(&mut next), 0, 18)
                        .error,
                    IoError::OK
                );
                assert_eq!(
                    hirsel_core_host_io(
                        context,
                        RANDOM,
                        control,
                        &mut empty(&mut independent),
                        0,
                        19
                    )
                    .error,
                    IoError::OK
                );
                assert_eq!(next, independent);
                for key in [host_key, virtual_key, ed_key, kek] {
                    hirsel_core_secret_drop(context, key.value);
                }
                tape.end(&Ending::returned(&[])).unwrap();
            }
        });
    }
    let frames = tape_frames(store.file());
    let generated: Vec<_> = frames
        .iter()
        .filter_map(|frame| match frame {
            Frame::Op {
                op: MEDIA_P256_KEY,
                out,
                ..
            } => Some(out.payload.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(generated, &[host_point.as_slice()]);
    let scalar = hex(P256_SCALAR);
    assert!(!store.file().windows(scalar.len()).any(|s| s == scalar));
}
