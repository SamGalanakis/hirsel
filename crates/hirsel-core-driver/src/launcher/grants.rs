//! The grant diff a deploy's admission reports (launch.md "Deploy preview").
use super::session::{Reader, Wire, Writer};
use std::io;

/// A grant diff: the sorted grant lines a new recipe holds that the old one lacks, and the
/// sorted lines of the old one it drops (`recipe_grants::grant_units`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Grants {
    pub(crate) added: Vec<String>,
    pub(crate) removed: Vec<String>,
}

impl Wire for Grants {
    fn put(&self, w: &mut Writer) {
        w.strings(&self.added);
        w.strings(&self.removed);
    }
    fn get(r: &mut Reader<'_>) -> io::Result<Self> {
        Ok(Self {
            added: r.strings()?,
            removed: r.strings()?,
        })
    }
}
