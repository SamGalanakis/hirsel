use super::*;

pub(super) fn tape_frames(mut bytes: &[u8]) -> Vec<Frame> {
    let (mut blocks, _) = Blocks::open(|buf| bytes.read(buf)).unwrap();
    let mut decoder = Decoder::default();
    let mut frames = Vec::new();
    while let Some((flags, payload)) = blocks.next(|buf| bytes.read(buf)).unwrap() {
        frames.extend(decoder.block(payload, flags & format::END != 0).unwrap());
    }
    frames
}
