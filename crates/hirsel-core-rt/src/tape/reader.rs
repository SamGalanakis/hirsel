//! A stream's frames, read block by block from the tape file (docs/recording.md,
//! "Replay"): a helper stream's start from its header block, which carries
//! its entry.
use super::*;

pub(super) struct FrameReader {
    pub(super) source: ProviderFile,
    blocks: StreamBlocks,
    retained_block: Option<Retained<'static>>,
    decoder: Decoder,
    frames: std::vec::IntoIter<Frame>,
}

impl FrameReader {
    /// A reader of `blocks`: a helper stream's decoder starts from its
    /// header block, which carries its entry.
    pub(super) fn open(source: ProviderFile, mut blocks: StreamBlocks) -> Result<Self, Fault> {
        Self::take_block_storage(&mut blocks);
        let decoder = match blocks.decoder(|offset, bytes| source.read_at(offset, bytes)) {
            Ok(decoder) => decoder,
            Err(ReadError::Format(s)) => return Err(Fault::stopped(s, 0)),
            Err(ReadError::Source(e)) => return Err(Fault::Io(e)),
        };
        let mut reader = Self {
            source,
            blocks,
            retained_block: None,
            decoder,
            frames: Default::default(),
        };
        reader.keep_block_storage();
        Ok(reader)
    }

    fn take_block_storage(blocks: &mut StreamBlocks) {
        if let Some(memory) = crate::units::memory() {
            blocks.swap_buffer(&mut memory.take_tape_buffer());
        }
    }

    fn keep_block_storage(&mut self) {
        if let Some(memory) = crate::units::memory() {
            let mut bytes = Vec::new();
            self.blocks.swap_buffer(&mut bytes);
            memory.put_tape_buffer(bytes);
            return;
        }
        self.retained_block = RETENTION.retain(self.blocks.buffer_capacity());
        if self.retained_block.is_none() {
            self.blocks.discard_buffer();
        }
    }

    pub(super) fn next(&mut self, at: u64) -> Result<Frame, Fault> {
        loop {
            if let Some(frame) = self.frames.next() {
                if self.frames.as_slice().is_empty() {
                    self.frames = Default::default();
                }
                return Ok(frame);
            }
            self.retained_block = None;
            Self::take_block_storage(&mut self.blocks);
            let decoded = match self
                .blocks
                .next(|offset, bytes| self.source.read_at(offset, bytes))
            {
                Err(ReadError::Format(s)) => Err(Fault::stopped(s, at)),
                Err(ReadError::Source(e)) => Err(Fault::Io(e)),
                Ok(None) => Err(Fault::Diverged(Divergence {
                    at,
                    kind: Mismatch::Extra,
                })),
                Ok(Some((flags, payload))) => self
                    .decoder
                    .block(payload, flags & END != 0)
                    .map_err(Fault::from),
            };
            self.keep_block_storage();
            self.frames = decoded?.into_iter();
        }
    }

    /// A helper stream's entry, decoded from the blocks before its first
    /// frame: the entry may span blocks.
    pub(super) fn entry(&mut self) -> Result<Entry, Fault> {
        loop {
            if let Some(entry) = self.decoder.take_entry() {
                return Ok(entry);
            }
            if self.frames.as_slice().first().is_some() {
                return Err(Fault::Corrupt);
            }
            self.retained_block = None;
            Self::take_block_storage(&mut self.blocks);
            let decoded = match self
                .blocks
                .next(|offset, bytes| self.source.read_at(offset, bytes))
            {
                Err(ReadError::Format(s)) => Err(Fault::stopped(s, 0)),
                Err(ReadError::Source(e)) => Err(Fault::Io(e)),
                Ok(None) => Err(Fault::Truncated(0)),
                Ok(Some((flags, payload))) => self
                    .decoder
                    .block(payload, flags & END != 0)
                    .map_err(Fault::from),
            };
            self.keep_block_storage();
            self.frames = decoded?.into_iter();
        }
    }
}
