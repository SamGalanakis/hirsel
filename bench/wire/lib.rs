pub mod client;
pub mod codec;
pub mod load;
pub mod reference;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

pub const BITS: u32 = 1_000_000;
pub const IMAGE_BYTES: usize = 125_000;
pub const CHUNKS: u32 = 1954;
pub const MAX_FRAME: usize = 256 * 1024;
pub const DONE_MAX: usize = 8 * 1024;
pub const REPLY_MAX: usize = DONE_MAX - 20;
pub const Q: usize = 4;
pub const V1_REFUSAL_MAX: usize = 36;
pub const OUTCOME_RESERVE: usize = (1 + Q) * (DONE_MAX + V1_REFUSAL_MAX);

pub fn flip(image: &mut [u8], bit: u32) -> bool {
    image[(bit >> 3) as usize] ^= 1 << (bit & 7);
    image[(bit >> 3) as usize] & (1 << (bit & 7)) != 0
}
