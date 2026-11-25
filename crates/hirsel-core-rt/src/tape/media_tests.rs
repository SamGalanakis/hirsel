use super::*;
use crate::media::capture::{Delivery, arguments, issue};
use hirsel_core_net::{
    secret_aead::AeadAlg,
    srtp_cipher::{SrtpProfile, SrtpSpace},
    tls_kdf::{Side, dtls12::Dtls12Keys},
};

#[test]
fn recorded_srtp_child_supplies_packet_on_replay() {
    let mut store = Store::default();
    let mut header = header(REVISION);
    header.kind = KIND_RUN;
    let expected = [
        0xa9, 0x69, 0x10, 0xa3, 0xd7, 0xe6, 0x6d, 0x0f, 0x6c, 0x03, 0x29, 0x06, 0x97, 0xac, 0xbb,
        0x07, 0x44, 0x3b, 0x55, 0x79, 0x7e, 0xa9, 0x69, 0xd2, 0xe5, 0x20,
    ];
    let shared = [
        0x8b, 0xd4, 0x05, 0x4f, 0xb5, 0x5b, 0x9d, 0x63, 0xfd, 0xfb, 0xac, 0xf9, 0xf0, 0x4b, 0x9f,
        0x0d, 0x35, 0xe6, 0xd6, 0x3f, 0x53, 0x75, 0x63, 0xef, 0xd4, 0x62, 0x72, 0x90, 0x0f, 0x89,
        0x49, 0x2d,
    ];
    let aad = [
        0x80, 0x0f, 0x12, 0x34, 0xde, 0xca, 0xfb, 0xad, 0xca, 0xfe, 0xba, 0xbe,
    ];
    let roc = [0; 4];
    let mut recorded_origin = 0;
    for mode in [Mode::Record, Mode::Replay] {
        in_task(false, &mut |context| {
            // SAFETY: this root pins its operands, destination, callbacks and store
            // through delivery. Neither callback escapes the synchronous issue.
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
                let (tape, _) = Tape::open_with_startup(REVISION, input, &expect()).unwrap();
                (*(*context).host_files).secret_mac_tape = Some(tape.secret_mac());

                let mut live_child = None;
                let mut no_output = Buffer {
                    data: ptr::null_mut(),
                    len: 0,
                    capacity: 0,
                };
                let received = issue(
                    context,
                    0,
                    arguments(0, 0, &[b"dtls12-srtp fixture", &[0]]),
                    Delivery::Child { refusals: &[] },
                    &mut no_output,
                    |_| {
                        assert_eq!(mode, Mode::Record);
                        let client = std::array::from_fn(|i| i as u8);
                        let server = std::array::from_fn(|i| 0xa0 + i as u8);
                        let mut parent =
                            Dtls12Keys::new(AeadAlg::Aes128Gcm, &shared, client, server, None)
                                .unwrap();
                        live_child =
                            parent.take_srtp(SrtpProfile::Aes128CmHmacSha1_80, Side::Client);
                        Outcome::default()
                    },
                    27,
                );
                assert_eq!(received.error, IoError::OK);
                if mode == Mode::Record {
                    recorded_origin = received.value;
                }
                assert_ne!(recorded_origin, 0);
                assert_eq!(received.value, recorded_origin);
                assert_eq!(live_child.is_some(), mode == Mode::Record);

                let mut output = [0; 26];
                let mut carrier = Buffer {
                    data: output.as_mut_ptr(),
                    len: 0,
                    capacity: output.len(),
                };
                let plain = [0xab; 16];
                let received = issue(
                    context,
                    1,
                    arguments(1, 26, &[&received.value.to_le_bytes(), &aad, &roc, &plain]),
                    Delivery::Packet {
                        value: 26,
                        bytes: 26,
                        tampered: None,
                    },
                    &mut carrier,
                    |dst| {
                        let [send, _] = live_child.take().unwrap();
                        let key = send.keys(SrtpSpace::Rtp, true).unwrap();
                        let nonce = key.cm_nonce(0xcafebabe, 0x1234).unwrap();
                        let mut body = plain;
                        let tag = key
                            .expand_cm()
                            .unwrap()
                            .seal(nonce, &aad, &roc, &mut body)
                            .unwrap();
                        crate::secret::key_share::append(dst, &body);
                        crate::secret::key_share::append(dst, &tag);
                        Outcome {
                            value: 26,
                            ..Outcome::default()
                        }
                    },
                    28,
                );
                assert_eq!(received.error, IoError::OK);
                assert_eq!(received.value, 26);
                assert_eq!(carrier.len, output.len());
                assert_eq!(output, expected);
                tape.end(&ending()).unwrap();
                (*(*context).host_files).secret_mac_tape = None;
            }
        });
    }
}
