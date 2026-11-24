use super::{Blocks, ReadError, Stop};
use std::io::Read;

#[test]
fn truncation_marker_requires_empty_payload_and_no_end_flag() {
    // Block 1 after a header with a zero tape id. The final eight bytes of
    // each fixture are the little-endian XXH3-64 checksum from libxxhash.
    let empty = b"\x04\x48\x54\x42\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x3a\x52\xec\x6a\x91\xdd\x11\x4c";
    let with_payload = b"\x04\x48\x54\x42\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x67\x4c\x94\xb2\x86\x07\x6c\x49";
    let with_end = b"\x06\x48\x54\x42\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x19\x67\xa9\x7f\x40\x26\x2d\x60";

    let stops = [empty.as_slice(), with_payload, with_end].map(|mut bytes| {
        let mut blocks = Blocks {
            bytes: Vec::new(),
            seq: 1,
            tape_id: [0; 16],
            ended: false,
        };
        match blocks.next(|buf| bytes.read(buf)) {
            Err(ReadError::Format(stop)) => stop,
            result => panic!("expected a format stop, got {result:?}"),
        }
    });
    assert_eq!(stops, [Stop::Truncated, Stop::Damaged(1), Stop::Damaged(1)]);
}
