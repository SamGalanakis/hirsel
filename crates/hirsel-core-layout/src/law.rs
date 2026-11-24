//! Native observation descriptors for the recording runtime.

use crate::{Encoding, Tag};

pub const ENTER: u64 = 0;
pub const OUTCOME: u64 = 1;
pub const SETTLED_CHECK: u64 = 2;
pub const LEAVE: u64 = 3;
pub const CORPUS_FAILURE: u64 = 4;
pub const DELEGATE: u64 = 5;
pub const WITNESS: u64 = 6;

pub const ALL: u64 = 0;
pub const SEARCH: u64 = 1;
pub const CHECK: u64 = 2;
pub const CHECK_TASKS: u64 = 3;
pub const CHECK_CORPUS: u64 = 4;
pub const CHECK_TASKS_CORPUS: u64 = 5;
pub const MODEL_EXPLORE: u64 = 6;
pub const MODEL_CHECK: u64 = 7;
pub const CALLEES: [&str; 8] = [
    "all",
    "search",
    "check",
    "check_tasks",
    "check_corpus",
    "check_tasks_corpus",
    "model.explore",
    "model.check",
];

pub fn model_engine(callee: u64) -> Option<u64> {
    match callee {
        MODEL_EXPLORE | MODEL_CHECK => Some(MODEL_EXPLORE),
        _ => None,
    }
}

pub const TAG_WORDS: usize = 5;
pub const ABSENT: u64 = u64::MAX;

pub fn tag_words(tag: Tag) -> [u64; TAG_WORDS] {
    let (dataful, range, start) = match tag.encoding {
        Encoding::Direct => (ABSENT, 0, 0),
        Encoding::Niche {
            dataful,
            first,
            last,
            start,
        } => (
            u64::from(dataful),
            u64::from(first) << 32 | u64::from(last),
            start,
        ),
    };
    [tag.offset, tag.width, dataful, range, start]
}

pub fn tag_from_words(words: &[u64]) -> Tag {
    let encoding = if words[2] == ABSENT {
        Encoding::Direct
    } else {
        Encoding::Niche {
            dataful: words[2] as u32,
            first: (words[3] >> 32) as u32,
            last: words[3] as u32,
            start: words[4],
        }
    };
    Tag {
        offset: words[0],
        width: words[1],
        encoding,
    }
}
