//! STAGE commits and fork observations the store crash laws report on stdout.
use super::{Bridge, Op};

/// A writer COMMIT the live read law stages through stdout ("STAGE").
/// It lands as the reader takes the terminal segment's fence: a writer
/// that completed and released the fence, its bytes not yet durable (BL13).
pub(super) struct StagedCommit {
    pub(super) segment: String,
    pub(super) at: usize,
    pub(super) frame: Vec<u8>,
    /// With an END image: write and sync both. Without: write the frame
    /// unsynced.
    pub(super) end: Option<Vec<u8>>,
    pub(super) takes_until_commit: u8,
}

impl StagedCommit {
    pub(super) fn parse(raw: &[u8]) -> Self {
        let boundary = match raw[0] {
            0 | 2 => false,
            1 => true,
            _ => panic!("unknown STAGE scenario"),
        };
        let at = u64::from_le_bytes(raw[1..9].try_into().unwrap()) as usize;
        let name_len = raw[9] as usize;
        let segment = std::str::from_utf8(&raw[10..10 + name_len])
            .unwrap()
            .to_owned();
        let rest = &raw[10 + name_len..];
        let frame_len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        let frame = rest[4..4 + frame_len].to_vec();
        let end = rest[4 + frame_len..].to_vec();
        assert_eq!(boundary, !end.is_empty(), "STAGE END image");
        StagedCommit {
            segment,
            at,
            frame,
            end: boundary.then_some(end),
            takes_until_commit: 1,
        }
    }
}

#[derive(Default)]
pub(super) struct ForkObservation {
    pub(super) image: u8,
    pub(super) synced: bool,
    pub(super) child_returned: bool,
}

// Walk the segment's wire envelopes independently of the store reader.
pub(super) fn fork_frame_start(frames: &[u8]) -> Option<usize> {
    let mut offset = 0;
    while frames.len() - offset >= 16 {
        let frames = &frames[offset..];
        let kind = u16::from_le_bytes(frames[..2].try_into().unwrap());
        let len = u32::from_le_bytes(frames[4..8].try_into().unwrap()) as usize;
        let payload = frames.get(16..16 + len)?;
        if kind == 0x0013 {
            assert_eq!(frames[2..4], [1, 0]);
            // AP19 and §13.1: id 1, parent 0, version 2, name "beta",
            // then the LE watermark. The fixture uses 2 schema stamps,
            // reserves 7 for the pack-list edit and 14 for the first put.
            assert_eq!(
                payload,
                b"\x01\0\0\0\0\0\0\0\x02\0\0\0\0\0\0\0\x04\0beta\x18\0\0\0\0\0\0\0"
            );
            return Some(offset);
        }
        offset += 16 + len;
    }
    None
}

impl Bridge {
    /// Lands the staged COMMIT at the first fence take on its segment. The
    /// frame overwrites the segment from the recovered end: a direct
    /// segment is padded past its frames.
    pub(super) fn land_staged(&mut self, name: &str) {
        let Some(staged) = self.staged.as_mut() else {
            return;
        };
        if staged.segment != name || staged.takes_until_commit == 0 {
            return;
        }
        staged.takes_until_commit -= 1;
        if staged.takes_until_commit != 0 {
            return;
        }
        let (at, frame, end) = (staged.at, staged.frame.clone(), staged.end.clone());
        let mut bytes = self.fs.read(name).unwrap().to_vec();
        assert!(bytes.len() >= at, "staged COMMIT starts within the segment");
        bytes.resize(bytes.len().max(at + frame.len()), 0);
        bytes[at..at + frame.len()].copy_from_slice(&frame);
        assert!(self.apply(Op::Write(name, &bytes)));
        if let Some(end) = end {
            let end_name = format!("{name}.end");
            assert!(self.apply(Op::Sync(name)));
            assert!(self.apply(Op::Write(&end_name, &end)));
            assert!(self.apply(Op::Sync(&end_name)));
        }
    }
}
