use super::{Blocks, ReadError, Stop};
use std::io::Read;

#[test]
fn truncation_marker_requires_empty_payload_and_no_end_flag() {
    // Block 1 after a header with a zero tape id. The final eight bytes of
    // each fixture are the little-endian XXH3-64 checksum from libxxhash.
    let empty = b"\x04HTB\x01\0\0\0\0\0\0\0\0\0\0\0\x9a\x63\x37\x17\xc4\x6e\xad\x8c";
    let with_payload = b"\x04HTB\x01\0\0\0\0\0\0\0\x01\0\0\0\0\x93\x7a\xe7\x3f\x1b\x62\x8a\x41";
    let with_end = b"\x06HTB\x01\0\0\0\0\0\0\0\0\0\0\0\xa2\xf2\xb9\xef\x9f\xf4\x0f\x66";

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
