//! A stream's recorder: frames encode into the block being filled, and a
//! closed block goes to the writer (or the provider file) under the stream's
//! number.
use super::*;

impl Recorder {
    /// Keep period-two CHOOSE runs in a REPEAT. Seals fall between frames,
    /// including between a frame and its DATA continuations. Frames encode
    /// straight into the block being filled, after its reserved head.
    pub(super) fn frame(&mut self, f: &Frame) -> Result<(), IoError> {
        if self.truncated || self.failed.is_some() {
            return Ok(());
        }
        if let Frame::Choose { gap, index } = *f {
            self.encoder.count();
            let choose = Some((gap, index));
            if self.chooses[(self.repeat % 2) as usize] == choose {
                self.repeat += 1;
            } else {
                self.flush_repeat();
                put_choose(&mut self.buffer, gap, index);
                self.chooses = [self.chooses[1], choose];
            }
        } else {
            self.flush_repeat();
            self.chooses = [None, None];
            let mut left = match f {
                Frame::Done { out, .. } | Frame::Op { out, .. } => out.payload.len(),
                Frame::HelperEnd { ending, .. } => ending.body().len(),
                Frame::Grant {
                    kind: crate::entry::EXEC,
                    args: Some(snapshot),
                    ..
                } => snapshot.len(),
                _ => 0,
            };
            self.room(left);
            let mut rest = Vec::new();
            self.encoder.frame_into(f, &mut self.buffer, &mut rest);
            for data in rest {
                left = left.saturating_sub(PAYLOAD_CHUNK);
                self.frames += 1;
                if self.full() {
                    let _ = self.close(0);
                    if self.truncated || self.failed.is_some() {
                        return Ok(());
                    }
                    self.room(left);
                }
                format::put_data(&mut self.buffer, data);
            }
        }
        self.frames += 1;
        if matches!(f, Frame::End { .. } | Frame::HelperEnd { .. }) {
            let _ = self.close(format::END);
        } else if self.full() {
            let _ = self.close(0);
        }
        Ok(())
    }

    pub(super) fn full(&self) -> bool {
        self.buffer.len() - BLOCK_HEAD >= CLOSE_AT || self.frames >= CLOSE_FRAMES
    }

    pub(super) fn empty(&self) -> bool {
        self.buffer.len() == BLOCK_HEAD
    }

    /// Make room for a frame carrying `payload` bytes, as much of it as the
    /// block can take, in one allocation: a recycled block buffer when the
    /// block is empty and the payload is large, else one reservation. A
    /// recorder holds no buffer between blocks (closing hands it off), so an
    /// idle unit's recording costs no block of memory.
    pub(super) fn room(&mut self, payload: usize) {
        let need = payload.min(CLOSE_AT + PAYLOAD_CHUNK) + 64;
        if self.buffer.capacity() - self.buffer.len() >= need {
            return;
        }
        if self.empty() && need > 4096 {
            let spare = writer::spare();
            if spare.capacity() >= BLOCK_HEAD + need {
                self.buffer = Self::fresh(spare);
                return;
            }
        }
        self.buffer.reserve(need);
    }

    /// A block buffer with its head reserved.
    pub(super) fn fresh(mut b: Vec<u8>) -> Vec<u8> {
        b.clear();
        b.resize(BLOCK_HEAD, 0);
        b
    }

    pub(super) fn flush_repeat(&mut self) {
        if self.repeat != 0 {
            put_repeat(&mut self.buffer, self.repeat);
            self.repeat = 0;
            self.chooses = [None, None];
        }
    }

    pub(super) fn close(&mut self, flags: u8) -> Result<(), IoError> {
        if self.truncated || self.failed.is_some() {
            return Ok(());
        }
        self.flush_repeat();
        self.chooses = [None, None];
        let block_frames = self.frames as u64;
        self.frames = 0;
        let seq = self.seq;
        self.seq += 1;
        self.encoder.new_block();
        format::seal(&self.tape_id, flags, self.stream, seq, &mut self.buffer);
        TAPE_COUNTS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        TAPE_COUNTS[1].fetch_add(
            self.buffer.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        let t0 = std::time::Instant::now();
        let r = match &self.sink {
            Sink::Provider(f) => {
                let r = f.write(&self.buffer);
                self.buffer = Self::fresh(Vec::new());
                r
            }
            Sink::Writer(stream) => {
                let b = std::mem::replace(&mut self.buffer, Self::fresh(Vec::new()));
                if !writer::push(stream, b, block_frames, seq == 0) {
                    self.truncated = true;
                    self.buffer = Vec::new();
                    let mut marker = Self::fresh(Vec::new());
                    format::seal(
                        &self.tape_id,
                        format::TRUNCATED,
                        self.stream,
                        seq,
                        &mut marker,
                    );
                    writer::mark(stream, marker);
                }
                Ok(())
            }
            Sink::Closed => Err(IoError { tag: 5, code: 0 }),
        };
        TAPE_COUNTS[C_WORKER_NS].fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        match r {
            Ok(()) => {
                if matches!(self.sink, Sink::Provider(_)) {
                    self.written_frames += block_frames;
                }
                Ok(())
            }
            Err(e) => {
                self.failed = Some((self.written_frames, e));
                Err(e)
            }
        }
    }
}
