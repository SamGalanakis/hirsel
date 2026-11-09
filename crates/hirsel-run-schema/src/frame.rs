//! The append-file envelope of every Hirsel recording and journal: the ordinary tape, the
//! durable run journal and the remote call journal (determinism.md, Frames and recovery).
//!
//! A file starts with a checksummed file header (format magic, identity, generation), then
//! frames. A frame is a fixed-size header (its own SHA-256, the body's SHA-256, sequence
//! number, body length, kind) followed by the body. The structure is TigerBeetle's
//! independent header and body checksums (`src/vsr/message_header.zig:17-41,108-122`, the
//! header checksum covers the size and the body checksum; `src/vsr/journal.zig:1345-1358`,
//! a header is validated before its size is trusted). Deviations: a sequential append file
//! instead of circular fixed slots and replica repair, since these files have no repair
//! service; SHA-256 for both digests instead of its checksum width; so recovery claims only
//! the prefix-write crash model and refuses any other damage.
//!
//! [`recover`] classifies a file's frames: every valid frame, then either the end of the
//! file or an incomplete tail (a physically short final header, or a valid final header
//! whose body runs past the end), or [`Corrupt`]. A complete header is validated before its
//! length is trusted, so a damaged length is Corrupt wherever it is. Body bytes are never
//! scanned for a plausible next header. The caller decodes each body; a body that does not
//! decode is Corrupt too, including the last.
use sha2::{Digest, Sha256};
use std::ops::Range;

const DIGEST: usize = 32;
/// Frame header: SHA-256 of the rest of the header, SHA-256 of the body, u64 sequence
/// number, u32 body length, kind byte (integers little-endian).
pub const HEADER: usize = DIGEST + DIGEST + 8 + 4 + 1;
const SEQ: usize = 2 * DIGEST;
const LEN: usize = SEQ + 8;
const KIND: usize = LEN + 4;

/// Reserve a frame header at the end of `out`; the body is then encoded in place after it
/// and [`finish`] seals the frame.
pub fn begin(out: &mut Vec<u8>) -> usize {
    let start = out.len();
    out.resize(start + HEADER, 0);
    start
}
/// Seal the frame begun at `start`, whose body is everything after its header.
pub fn finish(out: &mut [u8], start: usize, seq: u64, kind: u8) -> Result<(), String> {
    let frame = &mut out[start..];
    let len = u32::try_from(frame.len() - HEADER).map_err(|_| "frame body too large")?;
    let body: [u8; DIGEST] = Sha256::digest(&frame[HEADER..]).into();
    frame[DIGEST..SEQ].copy_from_slice(&body);
    frame[SEQ..LEN].copy_from_slice(&seq.to_le_bytes());
    frame[LEN..KIND].copy_from_slice(&len.to_le_bytes());
    frame[KIND] = kind;
    let header: [u8; DIGEST] = Sha256::digest(&frame[DIGEST..HEADER]).into();
    frame[..DIGEST].copy_from_slice(&header);
    Ok(())
}
/// Append one frame of `body`.
pub fn append(out: &mut Vec<u8>, seq: u64, kind: u8, body: &[u8]) -> Result<(), String> {
    let start = begin(out);
    out.extend_from_slice(body);
    finish(out, start, seq, kind)
}

/// A validated frame, borrowing the file's bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame<'a> {
    pub seq: u64,
    pub kind: u8,
    pub body: &'a [u8],
    /// The whole frame, header included.
    pub range: Range<usize>,
}

/// Damage the crash model cannot explain: recovery refuses the file and leaves it as it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Corrupt {
    /// The byte offset of the damaged frame (or 0 for the file header).
    pub offset: usize,
    pub detail: String,
}
impl std::fmt::Display for Corrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "corrupt at byte {}: {}", self.offset, self.detail)
    }
}

/// What a file's frames prove: every valid frame in order, and the end of the valid prefix.
/// An incomplete tail lies between `valid` and the end of the file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frames<'a> {
    pub frames: Vec<Frame<'a>>,
    pub valid: usize,
}

/// Classify the frames of `bytes` from `start` (the end of the file header), numbered from
/// sequence 0, whose kinds `known` accepts.
pub fn recover(
    bytes: &[u8],
    start: usize,
    known: impl Fn(u8) -> bool,
) -> Result<Frames<'_>, Corrupt> {
    let mut frames = vec![];
    let mut at = start;
    while at < bytes.len() {
        let rest = &bytes[at..];
        let corrupt = |detail: &str| Corrupt {
            offset: at,
            detail: detail.into(),
        };
        // A physically short final header is a torn tail.
        let Some(header) = rest.get(..HEADER) else {
            break;
        };
        if Sha256::digest(&header[DIGEST..])[..] != header[..DIGEST] {
            return Err(corrupt("frame header checksum mismatch"));
        }
        let seq = u64::from_le_bytes(header[SEQ..LEN].try_into().unwrap());
        if seq != frames.len() as u64 {
            return Err(corrupt("sequence break"));
        }
        let kind = header[KIND];
        if !known(kind) {
            return Err(corrupt("unknown record kind"));
        }
        let len = u32::from_le_bytes(header[LEN..KIND].try_into().unwrap()) as usize;
        // A valid final header whose body runs past the end is a torn tail.
        let Some(body) = rest.get(HEADER..HEADER + len) else {
            break;
        };
        if Sha256::digest(body)[..] != header[DIGEST..SEQ] {
            return Err(corrupt("frame body checksum mismatch"));
        }
        frames.push(Frame {
            seq,
            kind,
            body,
            range: at..at + HEADER + len,
        });
        at += HEADER + len;
    }
    Ok(Frames { frames, valid: at })
}

/// A file format: its magic (with the version) and the length of its identity.
#[derive(Clone, Copy, Debug)]
pub struct Format {
    pub magic: &'static [u8],
    pub identity: usize,
}
/// A validated file header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileHeader<'a> {
    pub identity: &'a [u8],
    pub generation: u64,
    /// Where the frames start.
    pub len: usize,
}
/// Why a file header does not open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Opening<'a> {
    Header(FileHeader<'a>),
    /// A proper prefix of this format's header: the file's creation never became durable.
    Short,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileError {
    /// Another format or another version of this one.
    Foreign,
    /// A complete header of this format that fails its checksum.
    Corrupt(Corrupt),
}
impl Format {
    /// Bytes in the file header: magic, identity, u64 generation, SHA-256 of all three.
    pub fn len(&self) -> usize {
        self.magic.len() + self.identity + 8 + DIGEST
    }
    /// The file header of a new file.
    pub fn header(&self, identity: &[u8], generation: u64) -> Vec<u8> {
        assert_eq!(identity.len(), self.identity, "file identity length");
        let mut out = self.magic.to_vec();
        out.extend_from_slice(identity);
        out.extend_from_slice(&generation.to_le_bytes());
        let digest = Sha256::digest(&out);
        out.extend_from_slice(&digest);
        out
    }
    /// Open the file header at the start of `bytes`.
    pub fn open<'a>(&self, bytes: &'a [u8]) -> Result<Opening<'a>, FileError> {
        let magic = self.magic.len().min(bytes.len());
        if bytes[..magic] != self.magic[..magic] {
            return Err(FileError::Foreign);
        }
        let Some(header) = bytes.get(..self.len()) else {
            return Ok(Opening::Short);
        };
        let (fields, digest) = header.split_at(self.len() - DIGEST);
        if Sha256::digest(fields)[..] != *digest {
            return Err(FileError::Corrupt(Corrupt {
                offset: 0,
                detail: "file header checksum mismatch".into(),
            }));
        }
        let identity = &fields[self.magic.len()..self.magic.len() + self.identity];
        let generation = u64::from_le_bytes(fields[fields.len() - 8..].try_into().unwrap());
        Ok(Opening::Header(FileHeader {
            identity,
            generation,
            len: self.len(),
        }))
    }
    /// Whether `bytes` are this format's file, or a proper prefix of its magic.
    pub fn is(&self, bytes: &[u8]) -> bool {
        !bytes.is_empty() && !matches!(self.open(bytes), Err(FileError::Foreign))
    }
}

/// The damage table every framed file's recovery is tested against (determinism.md, Frames
/// and recovery), each case with the classification the prefix-write crash model requires.
#[doc(hidden)]
pub mod damage {
    use super::*;

    /// What recovery must report: refusal, or a valid prefix ending at this byte.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Expect {
        Corrupt,
        Valid(usize),
    }
    /// A named damaged file and its required classification.
    pub struct Case {
        pub name: String,
        pub bytes: Vec<u8>,
        pub expect: Expect,
    }

    /// The cases for a valid file `bytes` of at least three frames from `start`, whose kinds
    /// `known` accepts and which `unknown` is not.
    pub fn cases(bytes: &[u8], start: usize, known: impl Fn(u8) -> bool, unknown: u8) -> Vec<Case> {
        let frames: Vec<Range<usize>> = recover(bytes, start, &known)
            .expect("an undamaged file")
            .frames
            .into_iter()
            .map(|f| f.range)
            .collect();
        assert!(frames.len() >= 3 && frames.last().unwrap().end == bytes.len());
        assert!(!known(unknown));
        let interior = frames[1].clone();
        let last = frames.last().unwrap().clone();
        let mut out = vec![];
        let mut case = |name: &str, bytes: Vec<u8>, expect| {
            out.push(Case {
                name: name.into(),
                bytes,
                expect,
            })
        };
        let set_len = |at: usize, len: u32| {
            let mut b = bytes.to_vec();
            b[at + LEN..at + KIND].copy_from_slice(&len.to_le_bytes());
            b
        };
        let flip = |at: usize| {
            let mut b = bytes.to_vec();
            b[at] ^= 1;
            b
        };
        // Re-frame the last frame with valid checksums but a changed field.
        let reframe = |seq: Option<u64>, kind: Option<u8>, body: Option<&[u8]>| {
            let parsed = &bytes[last.clone()];
            let mut b = bytes[..last.start].to_vec();
            let old_seq = u64::from_le_bytes(parsed[SEQ..LEN].try_into().unwrap());
            append(
                &mut b,
                seq.unwrap_or(old_seq),
                kind.unwrap_or(parsed[KIND]),
                body.unwrap_or(&parsed[HEADER..]),
            )
            .unwrap();
            b
        };
        let to_eof = (bytes.len() - interior.start - HEADER) as u32;
        case(
            "interior length enlarged to EOF",
            set_len(interior.start, to_eof),
            Expect::Corrupt,
        );
        case(
            "interior length enlarged past EOF",
            set_len(interior.start, to_eof + 1),
            Expect::Corrupt,
        );
        case(
            "interior length at its maximum",
            set_len(interior.start, u32::MAX),
            Expect::Corrupt,
        );
        case(
            "interior header digest",
            flip(interior.start),
            Expect::Corrupt,
        );
        case(
            "interior sequence",
            flip(interior.start + SEQ),
            Expect::Corrupt,
        );
        case("interior body", flip(interior.end - 1), Expect::Corrupt);
        let mut lost = bytes.to_vec();
        lost.drain(interior.clone());
        case("lost interior frame", lost, Expect::Corrupt);
        case(
            "final header kind byte",
            flip(last.start + KIND),
            Expect::Corrupt,
        );
        case(
            "final length enlarged past EOF",
            set_len(last.start, u32::MAX),
            Expect::Corrupt,
        );
        case("final body digest", flip(last.end - 1), Expect::Corrupt);
        case(
            "checksum-valid final sequence",
            reframe(Some(frames.len() as u64), None, None),
            Expect::Corrupt,
        );
        case(
            "checksum-valid final kind",
            reframe(None, Some(unknown), None),
            Expect::Corrupt,
        );
        case(
            "checksum-valid undecodable final body",
            reframe(None, None, Some(&[0xff; 3])),
            Expect::Corrupt,
        );
        for cut in last.start..last.end {
            case(
                &format!("final frame written to byte {}", cut - last.start),
                bytes[..cut].to_vec(),
                Expect::Valid(last.start),
            );
        }
        case("undamaged", bytes.to_vec(), Expect::Valid(bytes.len()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMAT: Format = Format {
        magic: b"test 1\n",
        identity: 4,
    };
    fn file() -> Vec<u8> {
        let mut bytes = FORMAT.header(b"abcd", 7);
        for (seq, body) in [&b"one"[..], b"", b"three"].into_iter().enumerate() {
            append(&mut bytes, seq as u64, 1, body).unwrap();
        }
        bytes
    }

    #[test]
    fn the_damage_table_classifies_by_the_prefix_write_model() {
        let bytes = file();
        for case in damage::cases(&bytes, FORMAT.len(), |k| k == 1, 9) {
            // The codec's own decode accepts any body, so an undecodable body is the caller's.
            if case.name == "checksum-valid undecodable final body" {
                continue;
            }
            let got = match recover(&case.bytes, FORMAT.len(), |k| k == 1) {
                Ok(frames) => damage::Expect::Valid(frames.valid),
                Err(_) => damage::Expect::Corrupt,
            };
            assert_eq!(got, case.expect, "{}", case.name);
        }
    }

    #[test]
    fn the_file_header_protects_its_identity_and_generation() {
        let bytes = file();
        let Ok(Opening::Header(header)) = FORMAT.open(&bytes) else {
            panic!()
        };
        assert_eq!((header.identity, header.generation), (&b"abcd"[..], 7));
        for at in 0..FORMAT.len() {
            let expected = if at < FORMAT.magic.len() {
                FileError::Foreign
            } else {
                FileError::Corrupt(Corrupt {
                    offset: 0,
                    detail: "file header checksum mismatch".into(),
                })
            };
            let mut bad = bytes.clone();
            bad[at] ^= 1;
            assert_eq!(FORMAT.open(&bad), Err(expected), "{at}");
            assert_eq!(FORMAT.open(&bytes[..at]), Ok(Opening::Short), "{at}");
        }
        assert_eq!(FORMAT.open(b"test 2\n"), Err(FileError::Foreign));
    }
}
