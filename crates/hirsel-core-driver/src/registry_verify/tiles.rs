use super::{Error, Hash};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) const WIDTH: u64 = 256;

pub(super) fn empty_root() -> Hash {
    Sha256::digest([]).into()
}

pub(super) fn leaf(entry: &[u8]) -> Hash {
    Sha256::new()
        .chain_update([0])
        .chain_update(entry)
        .finalize()
        .into()
}

fn branch(left: Hash, right: Hash) -> Hash {
    Sha256::new()
        .chain_update([1])
        .chain_update(left)
        .chain_update(right)
        .finalize()
        .into()
}

fn split(size: u64) -> u64 {
    1 << (size - 1).ilog2()
}

fn tile_path(level: &str, index: u64, width: u64) -> String {
    let mut groups = format!("{:03}", index % 1000);
    let mut higher = index / 1000;
    while higher != 0 {
        groups = format!("x{:03}/{groups}", higher % 1000);
        higher /= 1000;
    }
    let mut path = format!("tile/{level}/{groups}");
    if width < WIDTH {
        path.push_str(&format!(".p/{width}"));
    }
    path
}

pub(super) fn read_bundle(
    size: u64,
    index: u64,
    read: &mut impl FnMut(&str) -> Result<Vec<u8>, Error>,
) -> Result<Vec<u8>, Error> {
    read(&tile_path("entries", index, bundle_width(size, index)))
}

fn bundle_width(size: u64, index: u64) -> u64 {
    (size - index * WIDTH).min(WIDTH)
}

pub(super) fn bundle_entries(bytes: &[u8], size: u64, index: u64) -> Result<Vec<&[u8]>, Error> {
    let width = bundle_width(size, index) as usize;
    let mut remaining = bytes;
    let mut entries = Vec::with_capacity(width);
    let mut count = 0;
    while !remaining.is_empty() && count < WIDTH as usize {
        let (length, rest) = remaining.split_first_chunk::<2>().ok_or(Error::Bundle)?;
        let (entry, tail) = rest
            .split_at_checked(usize::from(u16::from_be_bytes(*length)))
            .ok_or(Error::Bundle)?;
        if count < width {
            entries.push(entry);
        }
        count += 1;
        remaining = tail;
    }
    if !remaining.is_empty() || (count != width && count != WIDTH as usize) {
        return Err(Error::Bundle);
    }
    Ok(entries)
}

fn hash_root(hashes: &[Hash]) -> Hash {
    match hashes {
        [] => empty_root(),
        [hash] => *hash,
        _ => {
            let (left, right) = hashes.split_at(split(hashes.len() as u64) as usize);
            branch(hash_root(left), hash_root(right))
        }
    }
}

struct Tiles<'a, F> {
    size: u64,
    read: &'a mut F,
    loaded: BTreeMap<(u32, u64), Vec<Hash>>,
}

impl<F: FnMut(&str) -> Result<Vec<u8>, Error>> Tiles<'_, F> {
    /// Get an RFC 6962 subtree from the highest tile level within it.
    fn subtree(&mut self, start: u64, size: u64) -> Result<Hash, Error> {
        if !size.is_power_of_two() {
            let k = split(size);
            return Ok(branch(
                self.subtree(start, k)?,
                self.subtree(start + k, size - k)?,
            ));
        }
        let level = size.ilog2() / 8;
        let first = start >> (8 * level);
        let index = first / WIDTH;
        let offset = (first % WIDTH) as usize;
        let width = bundle_width(self.size >> (8 * level), index) as usize;
        let tile = match self.loaded.entry((level, index)) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let path = tile_path(&level.to_string(), index, width as u64);
                let raw = (self.read)(&path)?;
                let (hashes, rest) = raw.as_chunks::<32>();
                if !rest.is_empty() || (hashes.len() != width && hashes.len() != WIDTH as usize) {
                    return Err(Error::Tile);
                }
                entry.insert(hashes[..width].to_vec())
            }
        };
        let count = (size >> (8 * level)) as usize;
        let hashes = tile.get(offset..offset + count).ok_or(Error::Tile)?;
        Ok(hash_root(hashes))
    }

    /// Fold the inclusion path for the whole bundle, treating its root as a subtree.
    fn include(
        &mut self,
        start: u64,
        size: u64,
        bundle_start: u64,
        bundle_size: u64,
        bundle_root: Hash,
    ) -> Result<Hash, Error> {
        if (start, size) == (bundle_start, bundle_size) {
            return Ok(bundle_root);
        }
        let k = split(size);
        let middle = start + k;
        if bundle_start < middle {
            let left = self.include(start, k, bundle_start, bundle_size, bundle_root)?;
            Ok(branch(left, self.subtree(middle, size - k)?))
        } else {
            let right = self.include(middle, size - k, bundle_start, bundle_size, bundle_root)?;
            Ok(branch(self.subtree(start, k)?, right))
        }
    }

    /// Reconstruct both roots in RFC 6962 SUBPROOF order. Until the first right
    /// descent, the old root itself supplies the common subtree.
    fn consistency(
        &mut self,
        start: u64,
        old: u64,
        new: u64,
        common_root: Option<Hash>,
    ) -> Result<(Hash, Hash), Error> {
        if old == new {
            let hash = match common_root {
                Some(hash) => hash,
                None => self.subtree(start, old)?,
            };
            return Ok((hash, hash));
        }
        let k = split(new);
        let middle = start + k;
        if old <= k {
            let (prior, left) = self.consistency(start, old, k, common_root)?;
            Ok((prior, branch(left, self.subtree(middle, new - k)?)))
        } else {
            let (prior_right, right) = self.consistency(middle, old - k, new - k, None)?;
            let left = self.subtree(start, k)?;
            Ok((branch(left, prior_right), branch(left, right)))
        }
    }
}

pub(super) fn consistent(
    old_size: u64,
    old_root: Hash,
    size: u64,
    root: Hash,
    read: &mut impl FnMut(&str) -> Result<Vec<u8>, Error>,
) -> bool {
    if old_size == 0 {
        return true;
    }
    let mut tiles = Tiles {
        size,
        read,
        loaded: BTreeMap::new(),
    };
    tiles.consistency(0, old_size, size, Some(old_root)) == Ok((old_root, root))
}

pub(super) fn included_entry(
    size: u64,
    root: Hash,
    index: u64,
    read: &mut impl FnMut(&str) -> Result<Vec<u8>, Error>,
) -> Result<Vec<u8>, Error> {
    if index >= size {
        return Err(Error::ReleaseClaim);
    }
    let bundle = index / WIDTH;
    let bytes = read_bundle(size, bundle, read)?;
    let entries = bundle_entries(&bytes, size, bundle)?;
    let hashes: Vec<_> = entries.iter().map(|entry| leaf(entry)).collect();
    let mut tiles = Tiles {
        size,
        read,
        loaded: BTreeMap::new(),
    };
    let calculated = tiles.include(
        0,
        size,
        bundle * WIDTH,
        hashes.len() as u64,
        hash_root(&hashes),
    )?;
    if calculated != root {
        return Err(Error::MerkleRoot);
    }
    Ok(entries[(index % WIDTH) as usize].into())
}

/// One complete subtree for each set bit of the prefix size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Frontier {
    size: u64,
    levels: [Option<Hash>; u64::BITS as usize],
}

impl Default for Frontier {
    fn default() -> Self {
        Self {
            size: 0,
            levels: [None; u64::BITS as usize],
        }
    }
}

impl Frontier {
    pub(super) fn levels(&self) -> [Option<Hash>; u64::BITS as usize] {
        self.levels
    }

    pub(super) fn restore(size: u64, levels: Vec<Option<Hash>>) -> Option<Self> {
        let levels: [Option<Hash>; u64::BITS as usize] = levels.try_into().ok()?;
        for (level, hash) in levels.iter().enumerate() {
            if hash.is_some() != ((size >> level) & 1 != 0) {
                return None;
            }
        }
        Some(Self { size, levels })
    }

    pub(super) fn size(&self) -> u64 {
        self.size
    }

    pub(super) fn append(&mut self, mut hash: Hash) -> Result<(), Error> {
        let size = self.size.checked_add(1).ok_or(Error::Bundle)?;
        let level = self.size.trailing_ones() as usize;
        for left in &mut self.levels[..level] {
            hash = branch(left.take().expect("occupied frontier level"), hash);
        }
        self.levels[level] = Some(hash);
        self.size = size;
        Ok(())
    }

    pub(super) fn root(&self) -> Hash {
        self.levels
            .iter()
            .flatten()
            .copied()
            .reduce(|right, left| branch(left, right))
            .unwrap_or_else(empty_root)
    }
}
