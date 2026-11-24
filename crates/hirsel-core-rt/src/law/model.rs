use hirsel_core_layout::{case_of, law::*};
use std::io::{self, Write};

pub(super) struct Verdict {
    class: &'static str,
    numbers: Vec<u64>,
    bound: Option<(&'static str, u64)>,
    witnessed: Vec<(String, u64)>,
}

fn variant_at(read: &impl Fn(u64, u64) -> u64, base: u64, words: &[u64]) -> usize {
    let tag = tag_from_words(words);
    case_of(&tag, read(base + tag.offset, tag.width)) as usize
}

fn model_bound(index: usize, bounds: [u64; 4]) -> (&'static str, u64) {
    (["depth", "states"][index], bounds[index])
}

impl Verdict {
    /// `value` and `words` must describe the initialized return of `model.explore`.
    pub(super) unsafe fn read(
        value: *const u8,
        words: &[u64],
        bounds: [u64; 4],
        read: &impl Fn(u64, u64) -> u64,
    ) -> Self {
        let case = |base, words: &[u64]| variant_at(read, base, words);
        let variant = case(0, words);
        let offsets = &words[TAG_WORDS..];
        let base = offsets[variant];
        let (stats, rest) = offsets[3..].split_at(7);
        let (limit, failure) = rest.split_at(1 + TAG_WORDS);
        if variant == 2 {
            return Self {
                class: "failed",
                numbers: vec![
                    read(base + failure[0], 8),
                    case(base + failure[1], &failure[2..]) as u64,
                ],
                bound: None,
                witnessed: Vec::new(),
            };
        }

        let &[states, edges, depth, list, stride, label, count] = stats else {
            unreachable!()
        };
        let depth = read(base + depth, 8);
        // SAFETY: this active Stats owns the initialized witness list of String/u64 pairs.
        let witnessed =
            unsafe { witnesses(value.add((base + list) as usize), stride, label, count) };
        if variant == 0 {
            Self {
                class: "closed",
                numbers: vec![read(base + states, 8), read(base + edges, 8), depth],
                bound: None,
                witnessed,
            }
        } else {
            let index = case(limit[0], &limit[1..]);
            Self {
                class: "to depth",
                numbers: vec![depth],
                bound: Some(model_bound(index, bounds)),
                witnessed,
            }
        }
    }

    pub(super) fn conformance(
        words: &[u64],
        bounds: [u64; 4],
        read: &impl Fn(u64, u64) -> u64,
    ) -> Self {
        let variant = variant_at(read, 0, words);
        let offsets = &words[TAG_WORDS..];
        let entry = read(offsets[variant], 8);
        let (limit, divergence) = offsets[3..].split_at(1 + TAG_WORDS);
        let (class, numbers, bound) = match variant {
            0 => ("conforms", vec![entry], None),
            1 => {
                let reason = variant_at(read, divergence[0], &divergence[1..]);
                ("diverged", vec![entry, reason as u64], None)
            }
            2 => {
                let index = variant_at(read, limit[0], &limit[1..]);
                ("incomplete", vec![entry], Some(model_bound(index, bounds)))
            }
            _ => unreachable!(),
        };
        Self {
            class,
            numbers,
            bound,
            witnessed: Vec::new(),
        }
    }

    pub(super) fn write(&self, output: &mut impl Write) -> io::Result<()> {
        write!(output, "\t{}\t", self.class)?;
        for (index, number) in self.numbers.iter().enumerate() {
            if index != 0 {
                write!(output, ",")?;
            }
            write!(output, "{number}")?;
        }
        write!(output, "\t")?;
        if let Some((name, number)) = self.bound {
            write!(output, "{name}={number}")?;
        }
        for (label, count) in &self.witnessed {
            write!(output, "\t{count}=")?;
            super::write_hex(output, label)?;
        }
        writeln!(output)
    }
}

/// The list and its elements must be live native carriers at the given offsets.
unsafe fn witnesses(list: *const u8, stride: u64, label: u64, count: u64) -> Vec<(String, u64)> {
    // SAFETY: native sequence carriers hold a data pointer followed by their length.
    let len = unsafe { list.add(8).cast::<usize>().read_unaligned() };
    if len == 0 {
        return Vec::new();
    }
    // SAFETY: a nonempty initialized list pins its elements during this hook.
    let data = unsafe { list.cast::<*const u8>().read_unaligned() };
    (0..len)
        .map(|i| {
            // SAFETY: i is in the live list, and the recipe names the tuple's two fields.
            unsafe {
                let pair = data.add(i * stride as usize);
                let label = super::string_bytes(pair.add(label as usize)).to_owned();
                let count = pair.add(count as usize).cast::<u64>().read_unaligned();
                (label, count)
            }
        })
        .collect()
}
