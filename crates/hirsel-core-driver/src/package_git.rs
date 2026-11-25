//! Git sources (packages.md section 7, GIT-1 to GIT-12): a commit fetched over Git's smart
//! HTTP protocol version 2, checked object by object, read as trees in memory and packed into
//! the bundle `hirsel pack` writes for the package at that commit. No program runs and no
//! file of the commit is written.

mod hash;
mod pack;
mod packer;
mod pkt;
mod snapshot;
mod transport;

pub(crate) use hash::Oid;
pub(crate) use packer::{pack_commit, pack_listed};
pub(crate) use transport::{Listed, Remote, list_refs};

use crate::package_manifest::GitSource;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// How a git fetch fails, before the binding's id and source are put in front.
#[derive(Debug)]
pub(crate) enum Failure {
    /// `fetching <source> failed: <cause>`.
    Fetch(String),
    /// `<source> sent a bad pack: <failure>`.
    Pack(String),
    /// A whole refusal, already worded.
    Refusal(String),
}

impl Failure {
    pub(crate) fn explain(self, id: &impl std::fmt::Display, source: &str) -> String {
        self.explain_after(&format!("{id}: "), source)
    }

    /// `explain` after `who`, the binding's `<id>: ` or nothing when `hirsel add` has yet to
    /// read the id.
    pub(crate) fn explain_after(self, who: &str, source: &str) -> String {
        match self {
            Self::Fetch(cause) => format!("{who}fetching {source} failed: {cause}"),
            Self::Pack(failure) => format!("{who}{source} sent a bad pack: {failure}"),
            Self::Refusal(text) => text,
        }
    }
}

pub(crate) const FETCH_LIMIT: u32 = 256;
pub(crate) const RECEIVED_LIMIT: u64 = 1 << 30;

/// What one outermost git source has spent, its nested fetches included (GIT-6).
pub(crate) struct Budget {
    /// The outermost binding's id and source spelling, which the fetch count refusal names;
    /// empty until `hirsel add` has read the id.
    pub(crate) outer: String,
    fetches: u32,
    received: u64,
}

impl Budget {
    pub(crate) fn new(outer: String) -> Self {
        Self {
            outer,
            fetches: 0,
            received: 0,
        }
    }

    /// Counts one more fetch, refused past the 256th.
    pub(crate) fn fetch(&mut self) -> Result<(), Failure> {
        self.fetches += 1;
        if self.fetches > FETCH_LIMIT {
            return Err(Failure::Refusal(format!(
                "{} causes more than {FETCH_LIMIT} fetches",
                self.outer
            )));
        }
        Ok(())
    }

    /// Counts bytes as received, refused past 1 GiB.
    pub(crate) fn receive(&mut self, bytes: usize) -> Result<(), Failure> {
        self.received += bytes as u64;
        if self.received > RECEIVED_LIMIT {
            return Err(Failure::Fetch("over the limit of 1 GiB received".into()));
        }
        Ok(())
    }
}

/// The commits one command has fetched: one (`git`, `rev`) is fetched once, whatever the
/// `dir`, and every later use reads the pack already held.
#[derive(Clone, Default)]
pub(crate) struct Session {
    packs: Arc<Mutex<HashMap<(String, String), Arc<pack::Pack>>>>,
}

impl Session {
    fn get(&self, source: &GitSource) -> Option<Arc<pack::Pack>> {
        let packs = self.packs.lock().unwrap();
        packs
            .get(&(source.url.clone(), source.rev.clone()))
            .cloned()
    }

    fn put(&self, source: &GitSource, pack: Arc<pack::Pack>) {
        let mut packs = self.packs.lock().unwrap();
        packs.insert((source.url.clone(), source.rev.clone()), pack);
    }
}

/// Server or tree text as section 9 writes it: the lock's string escapes without the quotes,
/// cut at 256 bytes.
pub(crate) fn quote_text(text: &[u8]) -> String {
    let text = String::from_utf8_lossy(&text[..text.len().min(256)]);
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0}'..='\u{1f}' | '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", u32::from(ch)));
            }
            _ => out.push(ch),
        }
    }
    out
}
