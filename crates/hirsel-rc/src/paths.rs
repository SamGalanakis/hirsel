//! A unit's field paths, each interned once, and the `Tri` rows over them
//! that the forward ownership state keeps per slot.
//!
//! A row is two bitsets, `yes` and `maybe`, indexed by `PathId`: a path in
//! neither is `No`. Rows are canonical (no trailing empty words), so equal
//! rows compare equal and an empty row is the default. A unit's first 64
//! paths fit in the inline words; further words are shared between clones
//! until one is written, so cloning a row never allocates.
use std::collections::HashMap;
use std::rc::Rc;

use crate::insert::{Step, Tri};

/// A field path interned in one unit's `Paths`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct PathId(u32);

impl PathId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

/// The field paths of one unit, in intern order.
#[derive(Debug, Default)]
pub(crate) struct Paths {
    steps: Vec<Vec<Step>>,
    ids: HashMap<Vec<Step>, PathId>,
}

impl Paths {
    pub fn intern(&mut self, path: &[Step]) -> PathId {
        if let Some(&id) = self.ids.get(path) {
            return id;
        }
        let id = PathId(u32::try_from(self.steps.len()).expect("paths fit u32"));
        self.steps.push(path.to_vec());
        self.ids.insert(path.to_vec(), id);
        id
    }

    /// The path's id, if the unit has interned it.
    pub fn get(&self, path: &[Step]) -> Option<PathId> {
        self.ids.get(path).copied()
    }

    pub fn steps(&self, id: PathId) -> &[Step] {
        &self.steps[id.index()]
    }
}

/// A `Tri` per interned path, `No` where unset.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub(crate) struct Tris {
    /// Paths 0 to 63.
    yes: u64,
    maybe: u64,
    /// Each further 64 paths as (yes, maybe): absent, or ending in a
    /// nonempty pair.
    more: Option<Rc<Vec<(u64, u64)>>>,
}

impl Tris {
    pub const EMPTY: Self = Self {
        yes: 0,
        maybe: 0,
        more: None,
    };

    fn more(&self) -> &[(u64, u64)] {
        self.more.as_deref().map_or(&[], Vec::as_slice)
    }

    fn words(&self) -> usize {
        1 + self.more().len()
    }

    fn word(&self, w: usize) -> (u64, u64) {
        match w {
            0 => (self.yes, self.maybe),
            _ => self.more().get(w - 1).copied().unwrap_or((0, 0)),
        }
    }

    fn set_word(&mut self, w: usize, (yes, maybe): (u64, u64)) {
        if w == 0 {
            self.yes = yes;
            self.maybe = maybe;
            return;
        }
        if self.word(w) == (yes, maybe) {
            return;
        }
        let more = Rc::make_mut(self.more.get_or_insert_default());
        if more.len() < w {
            more.resize(w, (0, 0));
        }
        more[w - 1] = (yes, maybe);
        while more.last() == Some(&(0, 0)) {
            more.pop();
        }
        if more.is_empty() {
            self.more = None;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.yes | self.maybe == 0 && self.more.is_none()
    }

    pub fn clear(&mut self) {
        *self = Self::EMPTY;
    }

    pub fn get(&self, id: PathId) -> Tri {
        let (yes, maybe) = self.word(id.index() / 64);
        let bit = 1 << (id.index() % 64);
        if yes & bit != 0 {
            Tri::Yes
        } else if maybe & bit != 0 {
            Tri::Maybe
        } else {
            Tri::No
        }
    }

    /// The `Tri` at a path that the unit may never have interned.
    pub fn at(&self, paths: &Paths, path: &[Step]) -> Tri {
        paths.get(path).map_or(Tri::No, |id| self.get(id))
    }

    pub fn set(&mut self, id: PathId, t: Tri) {
        let w = id.index() / 64;
        let bit = 1 << (id.index() % 64);
        let (mut yes, mut maybe) = self.word(w);
        yes &= !bit;
        maybe &= !bit;
        match t {
            Tri::No => {}
            Tri::Yes => yes |= bit,
            Tri::Maybe => maybe |= bit,
        }
        self.set_word(w, (yes, maybe));
    }

    /// Every path that is not `No`, by id.
    pub fn iter(&self) -> impl Iterator<Item = (PathId, Tri)> + '_ {
        (0..self.words()).flat_map(move |w| {
            let (yes, maybe) = self.word(w);
            let mut bits = yes | maybe;
            std::iter::from_fn(move || {
                if bits == 0 {
                    return None;
                }
                let i = bits.trailing_zeros();
                bits &= bits - 1;
                let t = if yes >> i & 1 != 0 {
                    Tri::Yes
                } else {
                    Tri::Maybe
                };
                Some((PathId((w * 64) as u32 + i), t))
            })
        })
    }

    /// Every path that is not `No`, in path order: where the order of the
    /// paths reaches the output, not the order the unit interned them.
    pub fn sorted<'p>(&self, paths: &'p Paths) -> Vec<(&'p [Step], Tri)> {
        let mut out: Vec<_> = self.iter().map(|(id, t)| (paths.steps(id), t)).collect();
        out.sort_unstable_by(|a, b| a.0.cmp(b.0));
        out
    }

    /// Sets to `No` every path for which `keep` is false.
    pub fn retain(&mut self, mut keep: impl FnMut(PathId) -> bool) {
        let dropped: Vec<PathId> = self
            .iter()
            .filter(|&(id, _)| !keep(id))
            .map(|(id, _)| id)
            .collect();
        for id in dropped {
            self.set(id, Tri::No);
        }
    }

    /// The pointwise `Tri::join`, an unset path being `No`.
    pub fn join(&mut self, other: &Self) {
        for w in 0..self.words().max(other.words()) {
            let (y1, m1) = self.word(w);
            let (y2, m2) = other.word(w);
            self.set_word(w, (y1 & y2, m1 | m2 | (y1 ^ y2)));
        }
    }
}
