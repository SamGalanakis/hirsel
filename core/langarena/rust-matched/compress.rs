// Matched twin of compress.hirsel. Review pending langs-numeric-920.
// kostya/LangArena3c9f84d benchmarks/compress.rs. Same prefix-doubling BWT,
// stable rank sort, arithmetic bitstreams/tables, and owned LZW dictionary bytes.
mod text_support;
use std::collections::HashMap;
struct BWTResult {
    transformed: Vec<u8>,
    original_idx: i32,
}

impl BWTResult {
    fn new(transformed: Vec<u8>, original_idx: i32) -> Self {
        Self {
            transformed,
            original_idx,
        }
    }
}

pub struct ArithEncodedResult {
    data: Vec<u8>,
    frequencies: Vec<i32>,
}

impl ArithEncodedResult {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            frequencies: Vec::new(),
        }
    }
}

struct ArithFreqTable {
    total: i32,
    low: Vec<i32>,
    high: Vec<i32>,
}

impl ArithFreqTable {
    fn new(frequencies: &[i32]) -> Self {
        let total: i32 = frequencies.iter().sum();
        let mut low = vec![0; 256];
        let mut high = vec![0; 256];

        let mut cum = 0;
        for i in 0..256 {
            low[i] = cum;
            cum += frequencies[i];
            high[i] = cum;
        }

        Self { total, low, high }
    }
}

struct BitOutputStream {
    buffer: u8,
    bit_pos: u8,
    bytes: Vec<u8>,
    bits_written: i32,
}

impl BitOutputStream {
    fn new() -> Self {
        Self {
            buffer: 0,
            bit_pos: 0,
            bytes: Vec::new(),
            bits_written: 0,
        }
    }

    fn write_bit(&mut self, bit: i32) {
        self.buffer = (self.buffer << 1) | (bit as u8 & 1);
        self.bit_pos += 1;
        self.bits_written += 1;

        if self.bit_pos == 8 {
            self.bytes.push(self.buffer);
            self.buffer = 0;
            self.bit_pos = 0;
        }
    }

    fn flush(&mut self) -> Vec<u8> {
        if self.bit_pos > 0 {
            self.buffer <<= 8 - self.bit_pos;
            self.bytes.push(self.buffer);
        }
        std::mem::take(&mut self.bytes)
    }
}

pub struct LZWResult {
    data: Vec<u8>,
}

impl LZWResult {
    fn new() -> Self {
        Self { data: Vec::new() }
    }
}

fn generate_test_data(size: i64) -> Vec<u8> {
    let pattern = b"ABRACADABRA";
    let pattern_len = pattern.len();
    let mut data = Vec::with_capacity(size as usize);

    for i in 0..size {
        data.push(pattern[(i as usize) % pattern_len]);
    }

    data
}

fn bwt_transform(input: &[u8]) -> BWTResult {
    let n = input.len();
    if n == 0 {
        return BWTResult::new(Vec::new(), 0);
    }

    let mut sa = vec![0usize; n];

    let mut counts = [0usize; 256];
    for &byte in input {
        counts[byte as usize] += 1;
    }

    let mut positions = [0usize; 256];
    let mut total = 0usize;
    for i in 0..256 {
        positions[i] = total;
        total += counts[i];
    }

    let mut temp_counts = [0usize; 256];
    for i in 0..n {
        let byte = input[i] as usize;
        let pos = positions[byte] + temp_counts[byte];
        sa[pos] = i;
        temp_counts[byte] += 1;
    }

    if n > 1 {
        let mut rank = vec![0usize; n];
        let mut current_rank = 0usize;
        let mut prev_char = input[sa[0]];

        for &idx in sa.iter() {
            if input[idx] != prev_char {
                current_rank += 1;
                prev_char = input[idx];
            }
            rank[idx] = current_rank;
        }

        let mut k = 1usize;
        while k < n {
            let mut pairs: Vec<(usize, usize)> = Vec::with_capacity(n);
            for i in 0..n {
                pairs.push((rank[i], rank[(i + k) % n]));
            }

            sa.sort_by(|&a, &b| {
                let pa = pairs[a];
                let pb = pairs[b];
                if pa.0 != pb.0 {
                    pa.0.cmp(&pb.0)
                } else {
                    pa.1.cmp(&pb.1)
                }
            });

            let mut new_rank = vec![0usize; n];
            new_rank[sa[0]] = 0;
            for i in 1..n {
                let prev = sa[i - 1];
                let curr = sa[i];
                let same = pairs[prev] == pairs[curr];
                new_rank[curr] = new_rank[prev] + if same { 0 } else { 1 };
            }

            rank = new_rank;
            k <<= 1;
        }
    }

    let mut transformed = vec![0u8; n];
    let mut original_idx: i32 = 0;

    for (i, &suffix) in sa.iter().enumerate() {
        if suffix == 0 {
            transformed[i] = input[n - 1];
            original_idx = i as i32;
        } else {
            transformed[i] = input[suffix - 1];
        }
    }

    BWTResult::new(transformed, original_idx)
}

fn bwt_inverse(bwt_result: &BWTResult) -> Vec<u8> {
    let bwt = &bwt_result.transformed;
    let n = bwt.len();
    if n == 0 {
        return Vec::new();
    }

    let mut counts = [0i32; 256];
    for &byte in bwt {
        counts[byte as usize] += 1;
    }

    let mut positions = [0i32; 256];
    let mut total = 0;
    for i in 0..256 {
        positions[i] = total;
        total += counts[i];
    }

    let mut next = vec![0usize; n];
    let mut temp_counts = [0i32; 256];

    for (i, &byte) in bwt.iter().enumerate() {
        let byte_idx = byte as usize;
        let pos = (positions[byte_idx] + temp_counts[byte_idx]) as usize;
        next[pos] = i;
        temp_counts[byte_idx] += 1;
    }

    let mut result = vec![0u8; n];
    let mut idx = bwt_result.original_idx as usize;

    for i in 0..n {
        idx = next[idx];
        result[i] = bwt[idx];
    }

    result
}

fn arith_encode(data: &[u8]) -> ArithEncodedResult {
    let mut frequencies = vec![0; 256];
    for &byte in data {
        frequencies[byte as usize] += 1;
    }

    let freq_table = ArithFreqTable::new(&frequencies);

    let mut low = 0u64;
    let mut high = 0xFFFFFFFFu64;
    let mut pending = 0;
    let mut output = BitOutputStream::new();

    for &byte in data {
        let idx = byte as usize;
        let range = high - low + 1;

        high = low + (range * freq_table.high[idx] as u64 / freq_table.total as u64) - 1;
        low = low + (range * freq_table.low[idx] as u64 / freq_table.total as u64);

        loop {
            if high < 0x80000000 {
                output.write_bit(0);
                for _ in 0..pending {
                    output.write_bit(1);
                }
                pending = 0;
            } else if low >= 0x80000000 {
                output.write_bit(1);
                for _ in 0..pending {
                    output.write_bit(0);
                }
                pending = 0;
                low -= 0x80000000;
                high -= 0x80000000;
            } else if low >= 0x40000000 && high < 0xC0000000 {
                pending += 1;
                low -= 0x40000000;
                high -= 0x40000000;
            } else {
                break;
            }

            low <<= 1;
            high = (high << 1) | 1;
            high &= 0xFFFFFFFF;
        }
    }

    pending += 1;
    if low < 0x40000000 {
        output.write_bit(0);
        for _ in 0..pending {
            output.write_bit(1);
        }
    } else {
        output.write_bit(1);
        for _ in 0..pending {
            output.write_bit(0);
        }
    }

    ArithEncodedResult {
        data: output.flush(),
        frequencies,
    }
}

fn arith_decode(encoded: &ArithEncodedResult) -> Vec<u8> {
    let frequencies = &encoded.frequencies;
    let total: i32 = frequencies.iter().sum();
    let data_size = total as usize;

    let mut low_table = vec![0i32; 256];
    let mut high_table = vec![0i32; 256];
    let mut cum = 0;
    for i in 0..256 {
        low_table[i] = cum;
        cum += frequencies[i];
        high_table[i] = cum;
    }

    let mut result = vec![0u8; data_size];
    let mut position = 0u64;

    let mut value = 0u64;
    for _ in 0..32 {
        value = (value << 1) | read_bit(&encoded.data, &mut position);
    }

    let mut low = 0u64;
    let mut high = 0xFFFFFFFFu64;

    for j in 0..data_size {
        let range = high - low + 1;
        let scaled = ((value - low + 1) * total as u64 - 1) / range;

        let symbol = high_table.partition_point(|&x| x as u64 <= scaled) as u8;
        result[j] = symbol as u8;

        high = low + (range * high_table[symbol as usize] as u64 / total as u64) - 1;
        low = low + (range * low_table[symbol as usize] as u64 / total as u64);

        loop {
            if high >= 0x80000000 && low < 0x80000000 && (low < 0x40000000 || high >= 0xC0000000) {
                break;
            }

            if high < 0x80000000 {
            } else if low >= 0x80000000 {
                value -= 0x80000000;
                low -= 0x80000000;
                high -= 0x80000000;
            } else if low >= 0x40000000 && high < 0xC0000000 {
                value -= 0x40000000;
                low -= 0x40000000;
                high -= 0x40000000;
            }

            low <<= 1;
            high = (high << 1) | 1;
            value = (value << 1) | read_bit(&encoded.data, &mut position);
        }
    }

    result
}

fn lzw_decode(encoded: &LZWResult) -> Vec<u8> {
    if encoded.data.is_empty() {
        return Vec::new();
    }

    assert_eq!(encoded.data.len() % 2, 0, "Decode error: incomplete code");

    let mut dict: Vec<Vec<u8>> = Vec::with_capacity(4096);
    for i in 0..256 {
        dict.push(vec![i as u8]);
    }

    let mut result = Vec::with_capacity(encoded.data.len() * 2);
    let mut codes = encoded.data.chunks_exact(2);
    let first = codes.next().unwrap();
    let mut old_code = u16::from_be_bytes([first[0], first[1]]) as usize;
    result.extend_from_slice(&dict[old_code]);

    for bytes in codes {
        let new_code = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        let mut entry = dict[old_code].clone();
        if new_code < dict.len() {
            entry.push(dict[new_code][0]);
            result.extend_from_slice(&dict[new_code]);
        } else if new_code == dict.len() {
            entry.push(entry[0]);
            result.extend_from_slice(&entry);
        } else {
            return Vec::new();
        };

        dict.push(entry);
        old_code = new_code;
    }

    result
}

fn lzw_encode(input: &[u8]) -> LZWResult {
    if input.is_empty() {
        return LZWResult::new();
    }

    let mut dict = HashMap::new();
    for i in 0..256 {
        dict.insert(vec![i as u8], i);
    }
    let mut next_code = 256;
    let mut result = Vec::with_capacity(input.len() * 2);
    let mut current = vec![input[0]];

    for &byte in &input[1..] {
        current.push(byte);
        if !dict.contains_key(&current) {
            let code = dict[&current[..current.len() - 1]];
            result.push(((code >> 8) & 0xFF) as u8);
            result.push((code & 0xFF) as u8);

            dict.insert(current, next_code);
            next_code += 1;
            current = vec![byte];
        }
    }

    let code = dict[&current];
    result.push(((code >> 8) & 0xFF) as u8);
    result.push((code & 0xFF) as u8);
    LZWResult { data: result }
}
fn read_bit(data: &[u8], position: &mut u64) -> u64 {
    let byte = usize::try_from(*position / 8).unwrap();
    let shift = u8::try_from(7 - *position % 8).unwrap();
    let b = if byte < data.len() { data[byte] } else { 0 };
    *position = position.checked_add(1).unwrap();
    u64::from((b >> shift) & 1)
}
fn main() {
    let a = text_support::arguments(&["size"], true);
    let task = &a[1];
    let size: i64 = a[2].parse().unwrap();
    let warmup: u64 = a[3].parse().unwrap();
    let iterations: u64 = a[4].parse().unwrap();
    let input = generate_test_data(size);
    let mut bwt = BWTResult::new(Vec::new(), 0);
    let mut encoded = LZWResult::new();
    let mut arith = ArithEncodedResult::new();
    if task == "Compress::BWTDecode" {
        bwt = bwt_transform(&input);
    }
    if task == "Compress::LZWDecode" {
        encoded = lzw_encode(&input);
    }
    if task == "Compress::ArithDecode" {
        arith = arith_encode(&input);
    }
    let mut decoded = Vec::new();
    let mut checksum = 0u32;
    for _ in 0..warmup.checked_add(iterations).unwrap() {
        let n = match task.as_str() {
            "Compress::BWTEncode" => {
                bwt = bwt_transform(&input);
                bwt.transformed.len()
            }
            "Compress::BWTDecode" => {
                decoded = bwt_inverse(&bwt);
                decoded.len()
            }
            "Compress::LZWEncode" => {
                encoded = lzw_encode(&input);
                encoded.data.len()
            }
            "Compress::LZWDecode" => {
                decoded = lzw_decode(&encoded);
                decoded.len()
            }
            "Compress::ArithEncode" => {
                arith = arith_encode(&input);
                arith.data.len()
            }
            "Compress::ArithDecode" => {
                decoded = arith_decode(&arith);
                decoded.len()
            }
            _ => panic!("unknown task"),
        };
        checksum = checksum.wrapping_add(u32::try_from(n).unwrap());
    }
    if [
        "Compress::BWTDecode",
        "Compress::LZWDecode",
        "Compress::ArithDecode",
    ]
    .contains(&task.as_str())
        && decoded == input
    {
        checksum = checksum.wrapping_add(100000);
    }
    text_support::output(checksum);
}
