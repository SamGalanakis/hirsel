use hirsel_core_layout::{case_of, law::*};
use std::io::{self, Write};

pub(super) struct Verdict {
    class: &'static str,
    numbers: Vec<u64>,
    bound: Option<(&'static str, u64)>,
    witnessed: Vec<(String, u64)>,
}

impl Verdict {
    /// `value` and `words` must describe the initialized return of `model.explore`.
    pub(super) unsafe fn read(
        value: *const u8,
        words: &[u64],
        bounds: [u64; 4],
        read: &impl Fn(u64, u64) -> u64,
    ) -> Self {
        let case = |base: u64, words: &[u64]| {
            let tag = tag_from_words(words);
            case_of(&tag, read(base + tag.offset, tag.width)) as usize
        };
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
                bound: Some((["depth", "states"][index], bounds[index])),
                witnessed,
            }
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
