//! One sequential discovery pass, followed by positioned reads of each stream.
use std::collections::{BTreeMap, BTreeSet};

use super::blocks::{
    check_digest, check_head, header_block, header_payload, read_block, sequence, stream,
};
use super::{Cursor, Decoder, END, Header, ReadError, Stop, TRUNCATED};

#[derive(Clone, Copy)]
struct Location {
    number: u64,
    offset: u64,
}

struct Stream {
    header: Location,
    parent: Option<(u32, u64)>,
    blocks: Vec<Location>,
    flags: u8,
}

pub struct Index {
    tape_id: [u8; 16],
    streams: BTreeMap<u32, Stream>,
    children: BTreeMap<(u32, u64), u32>,
    partial: bool,
}

impl Index {
    /// Check every block in file order before exposing any stream.
    pub fn open<E>(
        mut read: impl FnMut(u64, &mut [u8]) -> Result<usize, E>,
    ) -> Result<(Self, Header), ReadError<E>> {
        let mut offset = 0;
        let mut next = |current: &mut u64, buf: &mut [u8]| {
            let n = read(*current, buf)?;
            *current += n as u64;
            Ok(n)
        };
        let mut bytes = header_block(&mut |buf| next(&mut offset, buf), Some(super::FORMAT))?;
        let header = Header::decode(header_payload(&bytes)).map_err(Stop::from)?;
        let mut index = Self {
            tape_id: header.tape_id,
            streams: BTreeMap::new(),
            children: BTreeMap::new(),
            partial: false,
        };
        index.streams.insert(
            0,
            Stream {
                header: Location {
                    number: 0,
                    offset: 0,
                },
                parent: None,
                blocks: Vec::new(),
                flags: 0,
            },
        );
        for number in 1.. {
            let location = Location { number, offset };
            match read_block(&mut bytes, number, &mut |buf| next(&mut offset, buf)) {
                Ok(()) => {}
                Err(ReadError::Format(Stop::Missing)) => break,
                Err(ReadError::Format(Stop::Truncated)) if index.streams[&0].flags != END => {
                    index.partial = true;
                    break;
                }
                Err(ReadError::Format(Stop::Truncated)) => {
                    return Err(Stop::Damaged(number).into());
                }
                Err(other) => return Err(other),
            }
            check_head(&bytes, number)?;
            check_digest(&bytes, &index.tape_id, number)?;
            if index.streams[&0].flags == END {
                return Err(Stop::Damaged(number).into());
            }
            let (id, seq, flags) = (stream(&bytes), sequence(&bytes), bytes[0]);
            let damaged = Stop::Damaged(number);
            if let Some(stream) = index.streams.get_mut(&id) {
                if stream.flags != 0 || seq != stream.blocks.len() as u64 + 1 {
                    return Err(damaged.into());
                }
                stream.blocks.push(location);
                stream.flags = flags;
            } else {
                if seq != 0 || flags != 0 {
                    return Err(damaged.into());
                }
                let mut c = Cursor::new(header_payload(&bytes));
                let parent = (c.u32().map_err(|_| damaged)?, c.u64().map_err(|_| damaged)?);
                if parent.0 == id || index.children.insert(parent, id).is_some() {
                    return Err(damaged.into());
                }
                index.streams.insert(
                    id,
                    Stream {
                        header: location,
                        parent: Some(parent),
                        blocks: Vec::new(),
                        flags: 0,
                    },
                );
            }
        }
        index.check_parents()?;
        Ok((index, header))
    }

    fn check_parents(&self) -> Result<(), Stop> {
        let mut checked = BTreeSet::new();
        let mut damaged = None;
        for &id in self.streams.keys() {
            let mut positions = BTreeMap::new();
            let mut chain = Vec::new();
            let mut next = id;
            while let Some(parent) = self.streams.get(&next).and_then(|s| s.parent) {
                if checked.contains(&next) {
                    break;
                }
                if let Some(first) = positions.insert(next, chain.len()) {
                    let smallest = chain[first..].iter().min().unwrap();
                    let block = self.streams[smallest].header.number;
                    damaged = Some(damaged.map_or(block, |earliest: u64| earliest.min(block)));
                    break;
                }
                chain.push(next);
                next = parent.0;
            }
            checked.extend(chain);
        }
        damaged.map_or(Ok(()), |block| Err(Stop::Damaged(block)))
    }

    /// Walk the parent's children by numeric spawn index, visiting each parent first.
    pub fn paths(&self) -> impl Iterator<Item = (u32, String)> + '_ {
        let mut stack: Vec<_> = self
            .streams
            .iter()
            .rev()
            .filter_map(|(&id, stream)| {
                let (parent, _) = stream.parent?;
                (!self.streams.contains_key(&parent)).then(|| (id, format!("?.{id}")))
            })
            .collect();
        stack.push((0, String::from("root")));
        std::iter::from_fn(move || {
            let (id, name) = stack.pop()?;
            stack.extend(
                self.children
                    .range((id, 0)..=(id, u64::MAX))
                    .rev()
                    .map(|((_, spawn), child)| (*child, format!("{name}.{spawn}"))),
            );
            Some((id, name))
        })
    }

    pub fn take(&mut self, id: u32) -> Option<StreamBlocks> {
        let stream = self.streams.remove(&id)?;
        Some(StreamBlocks {
            tape_id: self.tape_id,
            id,
            header: stream.header,
            blocks: stream.blocks.into_iter(),
            seq: 0,
            end: stream.flags == END,
            partial: self.partial,
            bytes: Vec::new(),
        })
    }

    pub fn root<E>(
        read: impl FnMut(u64, &mut [u8]) -> Result<usize, E>,
    ) -> Result<(StreamBlocks, Header), ReadError<E>> {
        let (mut index, header) = Self::open(read)?;
        Ok((index.take(0).unwrap(), header))
    }
}

pub struct StreamBlocks {
    tape_id: [u8; 16],
    id: u32,
    header: Location,
    blocks: std::vec::IntoIter<Location>,
    seq: u64,
    end: bool,
    partial: bool,
    bytes: Vec<u8>,
}

impl StreamBlocks {
    pub fn buffer_capacity(&self) -> usize {
        self.bytes.capacity()
    }

    pub fn discard_buffer(&mut self) {
        self.bytes = Vec::new();
    }

    pub fn decoder<E>(
        &mut self,
        read: impl FnMut(u64, &mut [u8]) -> Result<usize, E>,
    ) -> Result<Decoder, ReadError<E>> {
        if self.id == 0 {
            return Ok(Decoder::default());
        }
        let location = self.header;
        let (_, payload) = self.read(location, 0, read)?;
        Decoder::helper(payload).map_err(|err| ReadError::Format(err.into()))
    }

    pub fn next<E>(
        &mut self,
        read: impl FnMut(u64, &mut [u8]) -> Result<usize, E>,
    ) -> Result<Option<(u8, &[u8])>, ReadError<E>> {
        let Some(location) = self.blocks.next() else {
            return if self.end {
                Ok(None)
            } else {
                Err(if self.partial {
                    Stop::Truncated
                } else {
                    Stop::Missing
                }
                .into())
            };
        };
        self.seq += 1;
        let (flags, payload) = self.read(location, self.seq, read)?;
        if flags == TRUNCATED {
            return Err(Stop::Truncated.into());
        }
        Ok(Some((flags, payload)))
    }

    fn read<E>(
        &mut self,
        location: Location,
        seq: u64,
        mut read: impl FnMut(u64, &mut [u8]) -> Result<usize, E>,
    ) -> Result<(u8, &[u8]), ReadError<E>> {
        let Location { number, mut offset } = location;
        read_block(&mut self.bytes, number, &mut |buf| {
            let n = read(offset, buf)?;
            offset += n as u64;
            Ok(n)
        })?;
        check_head(&self.bytes, number)?;
        check_digest(&self.bytes, &self.tape_id, number)?;
        if stream(&self.bytes) != self.id || sequence(&self.bytes) != seq {
            return Err(Stop::Damaged(number).into());
        }
        Ok((self.bytes[0], header_payload(&self.bytes)))
    }
}
