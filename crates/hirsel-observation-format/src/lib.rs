//! Constants shared by the runtime observation stream's writer and reader.

#[cfg(test)]
hirsel_guard::install!();

pub const MAGIC: [u8; 8] = *b"HIRSOBS3";
pub const HEADER_BYTES: usize = 20;
/// One whole record, including its header. This is a stream-format limit, not
/// a transport chunk size.
pub const RECORD_BYTES: usize = 64 * 1024;
pub const PAYLOAD_BYTES: usize = RECORD_BYTES - HEADER_BYTES;
pub const END: u16 = u16::MAX - 1;
pub const LOSS: u16 = u16::MAX;

/// Bytes after the header for a record with `words` u64s and `bytes` bytes,
/// when the record fits the stream format.
pub fn payload_bytes(words: usize, bytes: usize) -> Option<usize> {
    words
        .checked_mul(8)?
        .checked_add(bytes)
        .filter(|&size| size <= PAYLOAD_BYTES)
}
