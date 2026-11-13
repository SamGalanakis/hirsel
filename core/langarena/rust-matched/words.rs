// Reviewed-by: langs-numeric-920 2026-11-13; one owned key per hash entry, checked u32 counts; ordinal metadata reproduces insertion-order ties without duplicate owned Strings. Overflow checks required.
// Matched twin of words.hirsel. Reviewed below.
// kostya/LangArena 3c9f84d benchmarks/words.rs:32-73.
// Owned keys, hash lookup plus insertion-order iteration, explicit LCG.
mod text_support;
use std::collections::HashMap;
fn next(g: &mut i32, max: i32) -> i32 {
    *g = g.wrapping_mul(3877).wrapping_add(29573) % 139968;
    (*g as f64 / 139968.0 * f64::from(max)) as i32
}
fn tally(s: &str) -> u32 {
    // One owned key per entry. The ordinal gives Hirsel Map's insertion-order ties.
    let mut frequencies: HashMap<String, (u32, u64)> = HashMap::new();
    let mut ordinal = 0u64;
    for word in s.split(' ').filter(|s| !s.is_empty()) {
        if let Some((count, _)) = frequencies.get_mut(word) {
            *count = count.checked_add(1).unwrap();
        } else {
            frequencies.insert(word.to_owned(), (1, ordinal));
            ordinal = ordinal.checked_add(1).unwrap();
        }
    }
    let mut best = String::new();
    let mut maximum = 0u32;
    let mut first = u64::MAX;
    for (word, &(count, order)) in &frequencies {
        if count > maximum || count == maximum && order < first {
            best = word.clone();
            maximum = count;
            first = order;
        }
    }
    let hash = best.bytes().fold(5381u32, |h, b| {
        (h << 5).wrapping_add(h).wrapping_add(u32::from(b))
    });
    maximum
        .wrapping_add(hash)
        .wrapping_add(u32::try_from(frequencies.len()).unwrap())
}
fn main() {
    let a: Vec<String> = text_support::arguments(&["words", "word_len"], false);
    let words: u64 = a[1].parse().unwrap();
    let max: i32 = a[2].parse().unwrap();
    let warmup: u64 = a[3].parse().unwrap();
    let iterations: u64 = a[4].parse().unwrap();
    let mut source = String::with_capacity(
        usize::try_from(words.checked_mul(u64::try_from(max).unwrap() + 1).unwrap()).unwrap(),
    );
    let mut g = 42;
    for i in 0..words {
        let len = next(&mut g, max)
            .checked_add(next(&mut g, 3))
            .unwrap()
            .checked_add(3)
            .unwrap();
        for _ in 0..len {
            source.push(char::from_u32(97 + u32::try_from(next(&mut g, 26)).unwrap()).unwrap());
        }
        if i + 1 < words {
            source.push(' ');
        }
    }
    let mut checksum = 0u32;
    for _ in 0..warmup.checked_add(iterations).unwrap() {
        checksum = checksum.wrapping_add(tally(&source));
    }
    text_support::output(checksum);
}
