use std::fmt::{self, Write};

/// A basic block label: a prefix, an optional number and an optional edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::core) struct Label {
    pub name: &'static str,
    pub n: u32,
    pub edge: u32,
}
impl Label {
    const NONE: u32 = u32::MAX;
    pub const fn plain(name: &'static str) -> Self {
        Self {
            name,
            n: Self::NONE,
            edge: Self::NONE,
        }
    }
    pub fn new(name: &'static str, n: usize) -> Self {
        Self {
            name,
            n: n as u32,
            edge: Self::NONE,
        }
    }
    pub fn edge(n: usize, edge: u32) -> Self {
        Self {
            name: "e",
            n: n as u32,
            edge,
        }
    }
    pub fn block(i: impl TryInto<u32>) -> Self {
        Self {
            name: "b",
            n: i.try_into().ok().unwrap(),
            edge: Self::NONE,
        }
    }
    pub fn write_name(self, out: &mut impl Write) -> fmt::Result {
        out.write_str(self.name)?;
        if self.n != Self::NONE {
            write!(out, "{}", self.n)?;
        }
        if self.edge != Self::NONE {
            write!(out, "_{}", self.edge)?;
        }
        Ok(())
    }
}
impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_name(f)
    }
}
