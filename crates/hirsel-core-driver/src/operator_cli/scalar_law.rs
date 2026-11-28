use super::{pitr, wait};

const WAIT_EXAMPLES: &[(&str, Option<u64>)] = &[
    ("000ms", Some(0)),
    ("1ms", Some(1)),
    ("002s", Some(2_000)),
    ("3m", Some(180_000)),
    ("4h", Some(14_400_000)),
    ("18446744073709551615ms", Some(u64::MAX)),
    ("5124095576030h", Some(18_446_744_073_708_000_000)),
    ("5124095576031h", None),
    ("18446744073709551616ms", None),
    ("18446744073709552s", None),
    ("", None),
    ("200", None),
    ("ms", None),
    ("1.5s", None),
    ("-1s", None),
    ("+1s", None),
    (" 1m", None),
    ("1ms\n", None),
    ("1MS", None),
    ("１s", None),
];

type SelectorExample = (&'static str, Option<&'static [&'static str]>);
const SELECTOR_EXAMPLES: &[SelectorExample] = &[
    ("latest", Some(&["latest"])),
    ("boundary:Ab00fF", Some(&["boundary", "Ab00fF"])),
    ("at:main,000", Some(&["at", "0", "0"])),
    ("version:+0002,+0003", Some(&["version", "2", "3"])),
    (
        "version:4294967295,18446744073709551615",
        Some(&["version", "4294967295", "18446744073709551615"]),
    ),
    ("version:4294967296,0", None),
    ("at:main,18446744073709551616", None),
    ("", None),
    ("Latest", None),
    ("latest:", None),
    ("boundary:", None),
    ("boundary:a", None),
    ("boundary:0g", None),
    ("at:MAIN,1", None),
    ("at:-1,2", None),
    ("at:main, 1", None),
    ("at:main,1,2", None),
    ("version:,1", None),
    ("version:main,", None),
    ("version:main,+", None),
    ("version:main,１", None),
];

fn decimal(digits: &str) -> Option<u128> {
    if digits.is_empty() {
        return None;
    }
    digits.chars().try_fold(0u128, |n, c| {
        n.checked_mul(10)?.checked_add(u128::from(c.to_digit(10)?))
    })
}

fn wait_model(text: &str) -> Option<u64> {
    let boundary = text.find(|c: char| !c.is_ascii_digit())?;
    let (digits, units) = text.split_at(boundary);
    let ms = match units {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60 * 1_000,
        "h" => 60 * 60 * 1_000,
        _ => return None,
    };
    decimal(digits)?.checked_mul(ms)?.try_into().ok()
}

fn selector_model(text: &str) -> Option<Vec<String>> {
    if text == "latest" {
        return Some(vec![text.into()]);
    }
    if let Some(token) = text.strip_prefix("boundary:") {
        let mut bytes = token.as_bytes().chunks_exact(2);
        return (!token.is_empty()
            && bytes.remainder().is_empty()
            && bytes.all(|p| p.iter().all(u8::is_ascii_hexdigit)))
        .then(|| vec!["boundary".into(), token.into()]);
    }
    for kind in ["version", "at"] {
        if let Some(fields) = text.strip_prefix(kind).and_then(|s| s.strip_prefix(':')) {
            let mut fields = fields.split(',');
            let branch = fields.next()?;
            let number = fields.next()?;
            if fields.next().is_some() {
                return None;
            }
            let branch: u32 = if branch == "main" {
                0
            } else {
                decimal(branch.strip_prefix('+').unwrap_or(branch))?
                    .try_into()
                    .ok()?
            };
            let number: u64 = decimal(number.strip_prefix('+').unwrap_or(number))?
                .try_into()
                .ok()?;
            return Some(vec![kind.into(), branch.to_string(), number.to_string()]);
        }
    }
    None
}

struct Rng(u64);

impl Rng {
    fn index(&mut self, limit: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % limit as u64) as usize
    }
}

fn check_generated(seed: u64, examples: impl Iterator<Item = &'static str>, check: impl Fn(&str)) {
    let mut rng = Rng(seed);
    let examples: Vec<Vec<char>> = examples.map(|s| s.chars().collect()).collect();
    let example_chars: Vec<_> = examples.iter().flatten().copied().collect();
    let wider: Vec<_> = (0..=127)
        .map(char::from)
        .chain(['é', '中', '１', '−', '\u{00a0}', '🧪'])
        .collect();
    for _ in 0..4096 {
        let base = &examples[rng.index(examples.len())];
        let mut input = if rng.index(4) == 0 {
            vec![]
        } else {
            base.clone()
        };
        for _ in 0..=rng.index(4) {
            let start = rng.index(input.len() + 1);
            let end = start + rng.index((input.len() - start).min(5) + 1);
            let replacement = match rng.index(3) {
                0 => vec![example_chars[rng.index(example_chars.len())]],
                1 => {
                    let sample = &examples[rng.index(examples.len())];
                    let first = rng.index(sample.len() + 1);
                    let last = first + rng.index(sample.len() - first + 1);
                    sample[first..last].to_vec()
                }
                _ => {
                    let len = rng.index(9);
                    (0..len).map(|_| wider[rng.index(wider.len())]).collect()
                }
            };
            drop(input.splice(start..end, replacement));
        }
        check(&input.into_iter().collect::<String>());
    }
}

#[test]
fn wait_durations_match_example_seeded_model() {
    let parsed = |text: &str| wait::duration(text).ok().map(|d| d.as_millis());
    for &(text, expected) in WAIT_EXAMPLES {
        assert_eq!(wait_model(text), expected, "{text:?}");
        assert_eq!(parsed(text), expected.map(u128::from), "{text:?}");
    }
    check_generated(
        0x7e99_a2e4_7aea_118b,
        WAIT_EXAMPLES.iter().map(|e| e.0),
        |text| {
            assert_eq!(parsed(text), wait_model(text).map(u128::from), "{text:?}");
        },
    );
}

#[test]
fn pitr_selectors_match_example_seeded_model() {
    for &(text, expected) in SELECTOR_EXAMPLES {
        let expected = expected.map(|args| args.iter().map(|s| (*s).to_owned()).collect());
        assert_eq!(selector_model(text), expected, "{text:?}");
        assert_eq!(pitr::selector(text).ok(), expected, "{text:?}");
    }
    check_generated(
        0x1914_5718_8e8c_8819,
        SELECTOR_EXAMPLES.iter().map(|e| e.0),
        |text| {
            assert_eq!(pitr::selector(text).ok(), selector_model(text), "{text:?}");
        },
    );
}
