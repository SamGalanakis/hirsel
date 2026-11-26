use super::{
    Args, CLOSE, CLOSE_ENTRIES, Divergence, Fault, Frame, FrameReader, Inner, Live, Mismatch,
    OpenError, Out, ReplayInput, Request, Step, done, replay_out,
};
use std::collections::BTreeMap;

pub(super) fn is_close(op: u8) -> bool {
    matches!(op, CLOSE | CLOSE_ENTRIES)
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct CloseKey {
    object: u64,
    op: u8,
    part: Option<u8>,
}

impl CloseKey {
    pub(super) fn new(op: u8, object: u64, args: &Args) -> Result<Self, Fault> {
        let Args::Literal(bytes) = args else {
            return Err(Fault::Corrupt);
        };
        let part = match bytes.as_slice() {
            [] => None,
            [part @ 0..=2] if op == CLOSE => Some(*part),
            _ => return Err(Fault::Corrupt),
        };
        if object == 0 {
            return Err(Fault::Corrupt);
        }
        Ok(Self { object, op, part })
    }

    pub(super) fn record(frame: &Frame) -> Result<Option<Self>, Fault> {
        match frame {
            Frame::Op {
                task,
                op,
                object,
                args,
                ..
            } if is_close(*op) => {
                if *op == CLOSE
                    && matches!(args, Args::Literal(bytes) if bytes.as_slice() == crate::udp_net::record::CLOSE_ARGS)
                {
                    return Ok(None);
                }
                if *task != 0 {
                    return Err(Fault::Corrupt);
                }
                Self::new(*op, *object, args).map(Some)
            }
            Frame::Issue { op, .. } if is_close(*op) => Err(Fault::Corrupt),
            _ => Ok(None),
        }
    }
}

struct Entry {
    at: u64,
    out: Out,
    seen: bool,
    used: bool,
}

enum Scan {
    Unopened(Inner, Vec<u8>),
    Reading(FrameReader, u64),
    Ended,
}

pub(super) struct Closes {
    tape_id: [u8; 16],
    /// The replayed unit's stream, which the scan reads ahead in.
    stream: u32,
    scan: Scan,
    entries: BTreeMap<CloseKey, Entry>,
}

impl Closes {
    pub(super) fn new((dir, name): (Inner, Vec<u8>), tape_id: [u8; 16], stream: u32) -> Self {
        Self {
            tape_id,
            stream,
            scan: Scan::Unopened(dir, name),
            entries: BTreeMap::new(),
        }
    }

    /// A `CLOSE` of an uncreated object or one of `objects`, read before
    /// position `at`, that no drop has matched yet: a unit's drops keep their
    /// order against its takes, including drops of objects not yet created at
    /// this point in the replay.
    pub(super) fn unmatched_before(&self, at: u64, created: u64, objects: &[u64]) -> bool {
        self.entries.iter().any(|(key, entry)| {
            !entry.used
                && entry.at < at
                && key.op == CLOSE
                && (key.object > created || objects.contains(&key.object))
        })
    }

    pub(super) fn contains(&self, key: CloseKey) -> bool {
        self.entries.contains_key(&key)
    }

    pub(super) fn observe(
        &mut self,
        key: CloseKey,
        at: u64,
        frame: Frame,
        seen: bool,
    ) -> Result<(), Fault> {
        let Frame::Op { out, .. } = frame else {
            return Err(Fault::Corrupt);
        };
        if out.value != 0 || out.written != 0 || !out.payload.is_empty() {
            return Err(Fault::Corrupt);
        }
        match self.entries.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Entry {
                    at,
                    out,
                    seen,
                    used: false,
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let recorded = entry.get_mut();
                // A CLOSE seen ahead must agree with the original reader.
                if recorded.at != at || recorded.out != out {
                    return Err(Fault::Corrupt);
                }
                recorded.seen |= seen;
            }
        }
        Ok(())
    }

    pub(super) fn take(&mut self, key: CloseKey) -> Result<Option<Out>, Fault> {
        while !self.contains(key) {
            let Some((at, frame)) = self.scan_next()? else {
                return Ok(None);
            };
            if let Some(recorded) = CloseKey::record(&frame)? {
                self.observe(recorded, at, frame, false)?;
            }
        }
        let entry = self.entries.get_mut(&key).unwrap();
        if entry.used {
            return Ok(None);
        }
        entry.used = true;
        Ok(Some(entry.out.clone()))
    }

    // A drop moved earlier needs its outcome before the sequential reader
    // reaches the recorded CLOSE. Scan ahead with an independent cursor and
    // retain only CLOSE records. The sequential reader still checks the tape
    // and must see the same CLOSE at the same position before sealing.
    fn scan_next(&mut self) -> Result<Option<(u64, Frame)>, Fault> {
        if let Scan::Unopened(dir, name) = &self.scan {
            // SAFETY: ReplayInput's original directory provider remains pinned
            // throughout the replay, including all of its resource drops.
            let mut replay =
                unsafe { ReplayInput::open(*dir, name) }.map_err(|error| match error {
                    OpenError::Io(error) => Fault::Io(error),
                    OpenError::Unreadable(error) => error,
                    OpenError::Refused(_) => Fault::Corrupt,
                })?;
            if replay.stored.tape_id != self.tape_id {
                return Err(Fault::Corrupt);
            }
            let blocks = replay.index.take(self.stream).ok_or(Fault::Corrupt)?;
            let mut reader = FrameReader::open(replay.source, blocks)?;
            if self.stream != 0 {
                reader.entry()?;
            }
            self.scan = Scan::Reading(reader, 0);
        }
        let Scan::Reading(reader, at) = &mut self.scan else {
            return Ok(None);
        };
        let frame = reader.next(*at + 1)?;
        *at += 1;
        if matches!(frame, Frame::End { .. } | Frame::HelperEnd { .. }) {
            self.scan = Scan::Ended;
            Ok(None)
        } else {
            Ok(Some((*at, frame)))
        }
    }

    pub(super) fn finish(&self) -> Result<(), Fault> {
        if self.entries.values().any(|entry| !entry.seen) {
            return Err(Fault::Corrupt);
        }
        if let Some(at) = self
            .entries
            .values()
            .filter(|entry| !entry.used)
            .map(|entry| entry.at)
            .min()
        {
            return Err(Fault::Diverged(Divergence {
                at,
                kind: Mismatch::Extra,
            }));
        }
        Ok(())
    }
}

pub(super) fn replay_close(
    live: &mut Live,
    generation: u32,
    object: u64,
    request: &mut Request,
    args: &Args,
) -> Result<Step, Fault> {
    let op = request.operation;
    let key = CloseKey::new(op, object, args)?;
    let out = live.replayer.as_mut().unwrap().close(key)?;
    // SAFETY: a CLOSE has no output carrier and mutates only taped ownership.
    unsafe { replay_out(live, generation, op, object, request, out, None).map(done) }
}
