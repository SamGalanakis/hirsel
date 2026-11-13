// Review pending: langs-text-921. No matched ratio before independent review.
// Matches core/langarena/base64.hirsel: scalar loops, String/Vec<u8>, checked sizes.
#[allow(dead_code)]
mod support;
use support::Config;

fn encode(data: &str) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = data.as_bytes();
    let n = input.len();
    let mut result = String::with_capacity(((n + 2) / 3) * 4);
    let mut i = 0;
    while i < n {
        let a = u32::from(input[i]);
        let b = if i + 1 < n { u32::from(input[i + 1]) } else { 0 };
        let c = if i + 2 < n { u32::from(input[i + 2]) } else { 0 };
        let word = (a << 16) | (b << 8) | c;
        for shift in 0..4 {
            let digit = (word >> (18 - shift * 6)) & 63;
            let code = if shift == 2 && i + 1 >= n || shift == 3 && i + 2 >= n {
                b'='
            } else { alphabet[usize::try_from(digit).unwrap()] };
            result.push(char::from(code));
        }
        i += 3;
    }
    result
}
fn digit(b: u8) -> u32 {
    match b {
        b'A'..=b'Z' => u32::from(b - b'A'),
        b'a'..=b'z' => u32::from(b - b'a') + 26,
        b'0'..=b'9' => u32::from(b - b'0') + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("invalid base64"),
    }
}
fn decode(data: &str) -> Vec<u8> {
    let b = data.as_bytes();
    let n = b.len();
    assert_eq!(n % 4, 0);
    let mut out = Vec::with_capacity(n / 4 * 3);
    let mut at = 0;
    while at < n {
        let a = digit(b[at]);
        let c = digit(b[at + 1]);
        let d = if b[at + 2] == b'=' { 0 } else { digit(b[at + 2]) };
        let e = if b[at + 3] == b'=' { 0 } else { digit(b[at + 3]) };
        assert!(!(b[at + 2] == b'=' && (b[at + 3] != b'=' || at + 4 != n || c & 15 != 0)));
        assert!(!(b[at + 3] == b'=' && (at + 4 != n || d & 3 != 0)));
        let word = (a << 18) | (c << 12) | (d << 6) | e;
        out.push((word >> 16) as u8);
        if b[at + 2] != b'=' { out.push((word >> 8) as u8); }
        if b[at + 3] != b'=' { out.push(word as u8); }
        at += 4;
    }
    out
}
fn prefix(message: &mut String, data: &str) {
    if data.len() > 4 { message.push_str(&data[..4]); message.push_str("..."); }
    else { message.push_str(data); }
}
fn run(cfg: &Config) -> u32 {
    let decoding = cfg.name == "Base64::Decode";
    assert!(decoding || cfg.name == "Base64::Encode");
    let n = cfg.number("size");
    let mut data = String::with_capacity(n);
    for _ in 0..n { data.push_str("a"); }
    let mut encoded = encode(&data);
    let mut decoded = if decoding { decode(&encoded) } else { Vec::new() };
    let mut total = 0u32;
    for _ in 0..cfg.warmup + cfg.iterations {
        if decoding { decoded = decode(&encoded); total = total.wrapping_add(decoded.len() as u32); }
        else { encoded = encode(&data); total = total.wrapping_add(encoded.len() as u32); }
    }
    let mut message = String::from(if decoding { "decode " } else { "encode " });
    prefix(&mut message, if decoding { &encoded } else { &data });
    message.push_str(" to ");
    if decoding {
        assert!(decoded.len() >= 4);
        for b in &decoded[..4] { message.push(char::from(*b)); }
        message.push_str("...");
    } else { prefix(&mut message, &encoded); }
    message.push_str(": ");
    message.push_str(&u64::from(total).to_string());
    support::checksum(&message)
}
fn main() { support::main(run); }
