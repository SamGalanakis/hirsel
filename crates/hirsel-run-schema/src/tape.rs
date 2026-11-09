//! The ordinary tape's file format: a [`crate::frame`] file whose identity is the run
//! identity. Its frame kinds and payloads belong to the runtime's tape codec.
use crate::frame::Format;

pub const VERSION: u32 = 13;
pub const MAGIC: &[u8] = b"hirsel-io-tape 13\0";
pub const FORMAT: Format = Format {
    magic: MAGIC,
    identity: 32,
};
