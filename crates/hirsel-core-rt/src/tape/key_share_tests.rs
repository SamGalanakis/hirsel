use super::*;
use crate::{media::hirsel_core_media_drop, secret::key_share::hirsel_core_secret_key_share};

#[test]
fn host_key_share_public_points_replay() {
    let mut store = Store::default();
    let mut h = header(REVISION);
    h.kind = KIND_RUN;
    let mut points = [Vec::new(), Vec::new()];
    for mode in [Mode::Record, Mode::Replay] {
        in_task(mode == Mode::Record, &mut |context| {
            // SAFETY: the root task and both public carriers remain pinned.
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
                let random = files.registry.grant(crate::secret::MAC_PROVIDER, 0);
                for (group, len) in [32, 65].into_iter().enumerate() {
                    let mut bytes = [0xa5; 66];
                    let mut public = Buffer {
                        data: bytes.as_mut_ptr(),
                        len: 1,
                        capacity: len + 1,
                    };
                    let payer = (*context).current.available();
                    let key =
                        hirsel_core_secret_key_share(context, random, group as u64, &mut public, 5);
                    assert_eq!(
                        (key.status, key.error, public.len),
                        (0, IoError::OK, len + 1)
                    );
                    assert_eq!(bytes[0], 0xa5);
                    assert_eq!((*context).current.available(), payer - 64);
                    if mode == Mode::Record {
                        points[group] = bytes[1..public.len].to_vec();
                    } else {
                        assert_eq!(&bytes[1..public.len], points[group]);
                    }
                    hirsel_core_media_drop(context, key.value);
                    assert_eq!((*context).current.available(), payer);
                }
                tape.end(&Ending::returned(&[])).unwrap();
            }
        });
    }
    let payloads: Vec<_> = tape_frames(store.file())
        .into_iter()
        .filter_map(|frame| match frame {
            Frame::Op {
                op: MEDIA_KEY_SHARE,
                out,
                ..
            } => Some(out.payload),
            _ => None,
        })
        .collect();
    assert_eq!(payloads, points);
}
