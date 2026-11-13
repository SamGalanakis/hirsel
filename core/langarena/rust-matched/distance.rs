// Reviewed-by: langs-numeric-920 2026-11-13; Jaro/NGram: same ASCII pairs, byte bool flags, u64 counts and gram maps, score evaluation and checked arithmetic. Overflow checks required.
// Matched twin of distance.hirsel. Reviewed below.
// Source: kostya/LangArena3c9f84d benchmarks/distance.rs:31-243.
// Same ASCII bytes, boolean match lists, owned pair Strings and gram frequency maps.
mod text_support;
use std::collections::HashMap;
fn jaro(s1: &str, s2: &str) -> f64 {
    let bytes1 = s1.as_bytes();
    let bytes2 = s2.as_bytes();

    let len1 = bytes1.len();
    let len2 = bytes2.len();

    if len1 == 0 || len2 == 0 {
        return 0.0;
    }

    let match_dist = if len1 == 0 || len2 == 0 {
        0
    } else {
        (len1.max(len2) / 2).saturating_sub(1)
    };

    let mut s1_matches = vec![false; len1];
    let mut s2_matches = vec![false; len2];

    let mut matches = 0u64;
    for i in 0..len1 {
        let start = i.saturating_sub(match_dist);
        let end = (len2 - 1).min(i + match_dist);

        for j in start..=end {
            if !s2_matches[j] && bytes1[i] == bytes2[j] {
                s1_matches[i] = true;
                s2_matches[j] = true;
                matches += 1;
                break;
            }
        }
    }

    if matches == 0 {
        return 0.0;
    }

    let mut transpositions = 0u64;
    let mut k = 0;
    for i in 0..len1 {
        if s1_matches[i] {
            while k < len2 && !s2_matches[k] {
                k += 1;
            }
            if k < len2 {
                if bytes1[i] != bytes2[k] {
                    transpositions += 1;
                }
                k += 1;
            }
        }
    }
    transpositions /= 2;

    let m = matches as f64;
    (m / len1 as f64 + m / len2 as f64 + (m - transpositions as f64) / m) / 3.0
}
fn ngram(s1: &str, s2: &str) -> f64 {
    let s1_bytes = s1.as_bytes();
    let s2_bytes = s2.as_bytes();

    let len1 = s1_bytes.len();
    let len2 = s2_bytes.len();

    if len1 < 4 || len2 < 4 {
        return 0.0;
    }

    let mut grams1: HashMap<u32, u64> = HashMap::new();

    for i in 0..=len1 - 4 {
        let gram = ((s1_bytes[i] as u32) << 24)
            | ((s1_bytes[i + 1] as u32) << 16)
            | ((s1_bytes[i + 2] as u32) << 8)
            | (s1_bytes[i + 3] as u32);

        let _ = *grams1.entry(gram).and_modify(|e| *e += 1).or_insert(1);
    }

    let mut grams2: HashMap<u32, u64> = HashMap::new();
    let mut intersection = 0u64;

    for i in 0..=len2 - 4 {
        let gram = ((s2_bytes[i] as u32) << 24)
            | ((s2_bytes[i + 1] as u32) << 16)
            | ((s2_bytes[i + 2] as u32) << 8)
            | (s2_bytes[i + 3] as u32);

        let count2 = grams2.entry(gram).and_modify(|e| *e += 1).or_insert(1);

        if let Some(&count1) = grams1.get(&gram) {
            if *count2 <= count1 {
                intersection += 1;
            }
        }
    }

    let total = grams1.len() + grams2.len();
    if total > 0 {
        intersection as f64 / total as f64
    } else {
        0.0
    }
}
fn next(g: &mut i32, max: i32) -> i32 {
    *g = g.wrapping_mul(3877).wrapping_add(29573) % 139968;
    (*g as f64 / 139968.0 * f64::from(max)) as i32
}
fn random_string(g: &mut i32, n: i32) -> String {
    let mut s = String::with_capacity(usize::try_from(n).unwrap());
    for _ in 0..n {
        s.push(char::from_u32(97 + u32::try_from(next(g, 10)).unwrap()).unwrap());
    }
    s
}
fn main() {
    let a: Vec<String> = text_support::arguments(&["count", "size"], true);
    let task = &a[1];
    assert!(task == "Distance::Jaro" || task == "Distance::NGram");
    let count: usize = a[2].parse().unwrap();
    let size: i32 = a[3].parse().unwrap();
    let warmup: u64 = a[4].parse().unwrap();
    let iterations: u64 = a[5].parse().unwrap();
    let mut g = 42;
    let mut pairs = Vec::with_capacity(count);
    for _ in 0..count {
        let n = next(&mut g, size).checked_add(4).unwrap();
        let m = next(&mut g, size).checked_add(4).unwrap();
        let x = random_string(&mut g, n);
        let y = random_string(&mut g, m);
        pairs.push((x, y));
    }
    let mut result = 0u32;
    for _ in 0..warmup.checked_add(iterations).unwrap() {
        for (x, y) in &pairs {
            let score = if task == "Distance::Jaro" {
                jaro(x, y)
            } else {
                ngram(x, y)
            };
            result = result.wrapping_add((score * 1000.0) as u32);
        }
    }
    text_support::output(result);
}
