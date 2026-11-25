pub(super) fn crc_zero_byte(mut remainder: u64, poly: u64) -> u64 {
    for _ in 0..8 {
        remainder = (remainder >> 1) ^ (poly & (remainder & 1).wrapping_neg());
    }
    remainder
}

pub(super) fn crc_combine<const WIDTH: usize>(
    mut a: u64,
    b: u64,
    mut len_b: u64,
    poly: u64,
) -> u64 {
    // Each column describes what a zero byte does to one bit of the CRC.
    fn apply<const WIDTH: usize>(columns: &[u64; WIDTH], mut value: u64) -> u64 {
        let mut result = 0;
        while value != 0 {
            result ^= columns[value.trailing_zeros() as usize];
            value &= value - 1;
        }
        result
    }

    let mut power: [u64; WIDTH] = std::array::from_fn(|bit| crc_zero_byte(1 << bit, poly));
    while len_b != 0 {
        if len_b & 1 != 0 {
            a = apply(&power, a);
        }
        len_b >>= 1;
        if len_b != 0 {
            power = power.map(|column| apply(&power, column));
        }
    }
    a ^ b
}
