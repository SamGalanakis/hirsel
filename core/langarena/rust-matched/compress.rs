// Reviewed-by: langs-numeric-920 2026-11-13; all eight generated-input variants including HuffEncode/HuffDecode/LZWDecode; matching stable merge, boxed trees, first-equal insertion, arithmetic tables and byte dictionaries; overflow checks required.
// ArithEncode checked signed-frequency conversions now mirror int.narrow; delta approved by langs-numeric-920 2026-11-13 at b67a67e0; scoped to changed conversion/copy sites.
// Remaining BWT/LZW/Huff/ArithDecode checked-narrowing parity approved by langs-numeric-920 2026-11-13; remaining as sites are lossless widening or checksum truncation.
// Matched twin of compress.hirsel. All eight generated-input variants reviewed.
// Additional review langs-numeric-920: HuffEncode/HuffDecode/LZWDecode use matching boxed trees, first-equal insertion, i32 bit/frequency vectors, deep byte copies and owned dictionaries.
// kostya/LangArena3c9f84d benchmarks/compress.rs. Same prefix-doubling BWT,
// stable rank sort, arithmetic bitstreams/tables, and owned LZW dictionary bytes.
// Delta review langs-numeric-920 2026-11-14 at620b106a: BWTEncode/BWTDecode approved against core7e93579e move-merge and comparator carrier.
// HuffEncode/HuffDecode delta not approved: BoxInput::drop adds a full Option::unwrap scan absent from core's successful sort handoff; optimized IR retains it. See followups.md.
mod text_support;
use std::collections::HashMap;
mod compress_merge;
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
        let mut low = vec![0; 256];
        let mut high = vec![0; 256];

        let mut cum = 0;
        for i in 0..256 {
            low[i] = cum;
            cum += frequencies[i];
            high[i] = cum;
        }

        Self {
            total: cum,
            low,
            high,
        }
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

    fn write_bit(&mut self, bit: u8) {
        self.buffer = (self.buffer << 1) | (bit & 1);
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
    let mut data = Vec::with_capacity(usize::try_from(size).unwrap());

    for i in 0..size {
        data.push(pattern[usize::try_from(i).unwrap() % pattern_len]);
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

            compress_merge::indices(&mut sa, &pairs);

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
            original_idx = i32::try_from(i).unwrap();
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
        let pos = usize::try_from(positions[byte_idx] + temp_counts[byte_idx]).unwrap();
        next[pos] = i;
        temp_counts[byte_idx] += 1;
    }

    let mut result = vec![0u8; n];
    let mut idx = usize::try_from(bwt_result.original_idx).unwrap();

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
    let mut pending = 0u64;
    let mut output = BitOutputStream::new();

    let total = u64::try_from(freq_table.total).unwrap();
    for &byte in data {
        let idx = byte as usize;
        let range = high - low + 1;

        high = low + (range * u64::try_from(freq_table.high[idx]).unwrap() / total) - 1;
        low = low + (range * u64::try_from(freq_table.low[idx]).unwrap() / total);

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
    let table = ArithFreqTable::new(&encoded.frequencies);
    let total = u64::try_from(table.total).unwrap();
    let data_size = usize::try_from(total).unwrap();
    let low_table = &table.low;
    let high_table = &table.high;
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
        let scaled = ((value - low + 1) * total - 1) / range;

        let mut left = 0usize;
        let mut right = 256usize;
        while left < right {
            let middle = left + (right - left) / 2;
            if u64::try_from(high_table[middle]).unwrap() <= scaled {
                left = middle + 1;
            } else {
                right = middle;
            }
        }
        let symbol = left;
        result[j] = u8::try_from(symbol).unwrap();

        high = low + (range * u64::try_from(high_table[symbol]).unwrap() / total) - 1;
        low = low + (range * u64::try_from(low_table[symbol]).unwrap() / total);

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
        dict.push(vec![u8::try_from(i).unwrap()]);
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

    let mut dict: HashMap<Vec<u8>, u32> = HashMap::new();
    for i in 0u32..256u32 {
        dict.insert(vec![u8::try_from(i).unwrap()], i);
    }
    let mut next_code = 256u32;
    let mut result = Vec::with_capacity(input.len() * 2);
    let mut current = vec![input[0]];

    for &byte in &input[1..] {
        current.push(byte);
        if !dict.contains_key(&current) {
            let code = dict[&current[..current.len() - 1]];
            result.push(u8::try_from((code >> 8) & 0xFF).unwrap());
            result.push(u8::try_from(code & 0xFF).unwrap());

            dict.insert(current, next_code);
            next_code += 1;
            current = vec![byte];
        }
    }

    let code = dict[&current];
    result.push(u8::try_from((code >> 8) & 0xFF).unwrap());
    result.push(u8::try_from(code & 0xFF).unwrap());
    LZWResult { data: result }
}
fn read_bit(data: &[u8], position: &mut u64) -> u64 {
    let byte = usize::try_from(*position / 8).unwrap();
    let shift = u8::try_from(7 - *position % 8).unwrap();
    let b = if byte < data.len() { data[byte] } else { 0 };
    *position = position.checked_add(1).unwrap();
    u64::from((b >> shift) & 1)
}
struct HuffmanNode {
    frequency: i32,
    byte_val: u8,
    is_leaf: bool,
    left: Option<Box<HuffmanNode>>,
    right: Option<Box<HuffmanNode>>,
}

impl HuffmanNode {
    fn new(freq: i32, byte: u8, leaf: bool) -> Self {
        Self {
            frequency: freq,
            byte_val: byte,
            is_leaf: leaf,
            left: None,
            right: None,
        }
    }
}

struct HuffmanCodes {
    code_lengths: Vec<i32>,
    codes: Vec<i32>,
}

impl HuffmanCodes {
    fn new() -> Self {
        Self {
            code_lengths: vec![0; 256],
            codes: vec![0; 256],
        }
    }
}

struct EncodedResult {
    data: Vec<u8>,
    bit_count: i32,
    frequencies: Vec<i32>,
}

impl EncodedResult {
    fn new(data: Vec<u8>, bit_count: i32, frequencies: Vec<i32>) -> Self {
        Self {
            data,
            bit_count,
            frequencies,
        }
    }
}

fn compare_huffman(_: &(), a: &Box<HuffmanNode>, b: &Box<HuffmanNode>) -> std::cmp::Ordering {
    a.frequency.cmp(&b.frequency)
}

fn build_huffman_tree(frequencies: &[i32]) -> Option<Box<HuffmanNode>> {
    let mut nodes: Vec<Box<HuffmanNode>> = frequencies
        .iter()
        .enumerate()
        .filter(|&(_, &freq)| freq > 0)
        .map(|(i, &freq)| Box::new(HuffmanNode::new(freq, u8::try_from(i).unwrap(), true)))
        .collect();

    compress_merge::boxes(&mut nodes, &(), compare_huffman);

    if nodes.len() == 1 {
        let node = nodes.remove(0);
        let mut root = Box::new(HuffmanNode::new(node.frequency, 0, false));
        root.left = Some(node);
        root.right = Some(Box::new(HuffmanNode::new(0, 0, true)));
        return Some(root);
    }

    while nodes.len() > 1 {
        let left = nodes.remove(0);
        let right = nodes.remove(0);

        let mut parent = Box::new(HuffmanNode::new(left.frequency + right.frequency, 0, false));
        parent.left = Some(left);
        parent.right = Some(right);

        let mut lo = 0usize;
        let mut hi = nodes.len();
        let mut found = false;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match nodes[mid].frequency.cmp(&parent.frequency) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Equal => {
                    found = true;
                    hi = mid;
                }
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        let _ = found;
        let pos = lo;
        nodes.insert(pos, parent);
    }

    nodes.pop()
}

fn build_huffman_codes(
    node: &HuffmanNode,
    code: i32,
    length: i32,
    huffman_codes: &mut HuffmanCodes,
) {
    if node.is_leaf {
        if length > 0 || node.byte_val != 0 {
            let idx = node.byte_val as usize;
            huffman_codes.code_lengths[idx] = length;
            huffman_codes.codes[idx] = code;
        }
    } else {
        if let Some(ref left) = node.left {
            build_huffman_codes(left, code << 1, length + 1, huffman_codes);
        }
        if let Some(ref right) = node.right {
            build_huffman_codes(right, (code << 1) | 1, length + 1, huffman_codes);
        }
    }
}

fn huffman_encode(data: &[u8], codes: &HuffmanCodes, frequencies: Vec<i32>) -> EncodedResult {
    let mut result = Vec::with_capacity(data.len() * 2);
    let mut current_byte = 0u8;
    let mut bit_pos = 0;
    let mut total_bits = 0;

    for &byte in data {
        let idx = byte as usize;
        let code = codes.codes[idx];
        let length = usize::try_from(codes.code_lengths[idx]).unwrap();

        for i in (0..length).rev() {
            if (code & (1 << i)) != 0 {
                current_byte |= 1 << (7 - bit_pos);
            }
            bit_pos += 1;
            total_bits += 1;

            if bit_pos == 8 {
                result.push(current_byte);
                current_byte = 0;
                bit_pos = 0;
            }
        }
    }

    if bit_pos > 0 {
        result.push(current_byte);
    }

    EncodedResult::new(result, total_bits, frequencies)
}

fn huffman_decode(encoded: &[u8], root: &HuffmanNode, bit_count: i32) -> Vec<u8> {
    let mut result = vec![0u8; usize::try_from(bit_count).unwrap()];
    let mut result_size = 0;

    let mut current_node = root;
    let mut bits_processed = 0;
    let mut byte_index = 0;

    while bits_processed < bit_count && byte_index < encoded.len() {
        let byte_val = encoded[byte_index];
        byte_index += 1;

        for bit_pos in (0..8).rev() {
            if bits_processed >= bit_count {
                break;
            }

            let bit = ((byte_val >> bit_pos) & 1) == 1;
            bits_processed += 1;

            current_node = if bit {
                current_node.right.as_ref().unwrap()
            } else {
                current_node.left.as_ref().unwrap()
            };

            if current_node.is_leaf {
                if current_node.byte_val != 0 {
                    result[result_size] = current_node.byte_val;
                    result_size += 1;
                }
                current_node = root;
            }
        }
    }

    result.truncate(result_size);
    result
}
fn huff_encode(input: &[u8]) -> EncodedResult {
    let mut frequencies = vec![0i32; 256];
    for &byte in input {
        frequencies[usize::from(byte)] += 1;
    }
    let tree = build_huffman_tree(&frequencies);
    let mut codes = HuffmanCodes::new();
    if let Some(root) = &tree {
        build_huffman_codes(root, 0, 0, &mut codes);
    }
    huffman_encode(input, &codes, frequencies)
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
    let mut huff = EncodedResult::new(Vec::new(), 0, Vec::new());
    if task == "Compress::HuffDecode" {
        huff = huff_encode(&input);
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
            "Compress::HuffEncode" => {
                huff = huff_encode(&input);
                huff.data.len()
            }
            "Compress::HuffDecode" => {
                let tree = build_huffman_tree(&huff.frequencies);
                if let Some(root) = &tree {
                    decoded = huffman_decode(&huff.data, root, huff.bit_count);
                }
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
        "Compress::HuffDecode",
    ]
    .contains(&task.as_str())
        && decoded == input
    {
        checksum = checksum.wrapping_add(100000);
    }
    text_support::output(checksum);
}
