//! The frames one scheduling decision selects, in selection order. A decision
//! almost always selects one or two frames, so those stay inline and a
//! retirement allocates nothing; a longer selection continues in `spill`.

const INLINE: usize = 2;

#[derive(Clone, Debug)]
pub(crate) struct Picks<T: Copy> {
    inline: [Option<T>; INLINE],
    spill: Vec<T>,
}

impl<T: Copy> Picks<T> {
    pub(crate) const fn new() -> Self {
        Self {
            inline: [None; INLINE],
            spill: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, value: T) {
        match self.inline.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => *slot = Some(value),
            None => self.spill.push(value),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.inline[0].is_none() && self.spill.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = T> + '_ {
        self.inline
            .iter()
            .flatten()
            .copied()
            .chain(self.spill.iter().copied())
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, value: &T) -> bool
    where
        T: PartialEq,
    {
        self.iter().any(|item| item == *value)
    }
}

impl<T: Copy> Default for Picks<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A selection another policy already collected keeps its storage.
impl<T: Copy> From<Vec<T>> for Picks<T> {
    fn from(spill: Vec<T>) -> Self {
        Self {
            inline: [None; INLINE],
            spill,
        }
    }
}

impl<T: Copy> FromIterator<T> for Picks<T> {
    fn from_iter<I: IntoIterator<Item = T>>(items: I) -> Self {
        let mut picks = Self::new();
        for item in items {
            picks.push(item);
        }
        picks
    }
}

impl<T: Copy> IntoIterator for Picks<T> {
    type Item = T;
    type IntoIter = std::iter::Chain<
        std::iter::Flatten<std::array::IntoIter<Option<T>, INLINE>>,
        std::vec::IntoIter<T>,
    >;

    fn into_iter(self) -> Self::IntoIter {
        self.inline.into_iter().flatten().chain(self.spill)
    }
}

#[cfg(test)]
impl<T: Copy + PartialEq> PartialEq<Vec<T>> for Picks<T> {
    fn eq(&self, other: &Vec<T>) -> bool {
        self.iter().eq(other.iter().copied())
    }
}
