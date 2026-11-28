use super::*;
use std::io::{self, BufRead, BufReader, Read};
use zstd_safe::{CCtx, CParameter, DCtx, DParameter, InBuffer, OutBuffer};

const ARCHIVE_TAG: &str = "hirsel.package-archive.v2";
const WINDOW_LOG: u32 = 23;
const WINDOW_LIMIT: u64 = 1 << WINDOW_LOG;
const COMPRESSION_LEVEL: i32 = 19;
const FRAME_HEADER_MAX: usize = 18;
const NOT_ONE_FRAME: &str = "not one Zstandard frame";
const FRAME_LIMIT: usize = 65 << 20;

pub(crate) fn frame_encoder() -> Result<CCtx<'static>, String> {
    let mut encoder = CCtx::try_create().ok_or("could not allocate a Zstandard encoder")?;
    for parameter in [
        CParameter::CompressionLevel(COMPRESSION_LEVEL),
        CParameter::WindowLog(WINDOW_LOG),
        CParameter::ContentSizeFlag(true),
    ] {
        encoder
            .set_parameter(parameter)
            .map_err(zstd_safe::get_error_name)?;
    }
    Ok(encoder)
}

impl PackageArchive {
    /// Opens the package archive at `path`; `toolchain` admits a toolchain package's own
    /// bare name, as [`Content::decode_as`] does.
    pub(crate) fn open(
        path: &std::path::Path,
        toolchain: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        use std::os::unix::fs::OpenOptionsExt;

        let read = || -> Result<Self, Box<dyn std::error::Error>> {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)?;
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err("expected a regular file".into());
            }
            if meta.len() > ARCHIVE_LIMIT as u64 {
                return Err(format!("over the limit of {ARCHIVE_LIMIT} bytes").into());
            }
            let (archive, _) = Self::read(file, toolchain)?;
            let id = &archive.entries[&archive.head].content.id;
            archive.verify(id, archive.head, "archive")?;
            Ok(archive)
        };
        read().map_err(|error| format!("{}: {error}", path.display()).into())
    }

    pub(crate) fn encode(head: Root, entries: &BTreeMap<Root, Library>) -> Result<Vec<u8>, String> {
        if entries.len() > PACKAGE_LIMIT {
            return Err(format!("over the limit of {PACKAGE_LIMIT} packages"));
        }
        let mut out = Writer::new(ARCHIVE_LIMIT);
        out.text(ARCHIVE_TAG);
        out.number(entries.len());
        let mut encoder = frame_encoder()?;
        let mut compressed = Vec::new();
        let mut total = 0;
        for library in std::iter::once(&entries[&head]).chain(
            entries
                .iter()
                .filter_map(|(root, library)| (*root != head).then_some(library)),
        ) {
            let bytes = &library.encoded;
            total += bytes.len();
            if total > ARCHIVE_LIMIT {
                return Err("entries inflate past 512 MiB".into());
            }
            compressed.clear();
            compressed.reserve(zstd_safe::compress_bound(bytes.len()));
            encoder
                .compress2(&mut compressed, bytes)
                .map_err(zstd_safe::get_error_name)?;
            out.frame(&compressed);
            if out.exceeded {
                break;
            }
        }
        out.finish()
    }

    pub(crate) fn decode_as(bytes: Bytes, toolchain: bool) -> Result<Self, String> {
        if bytes.len() > ARCHIVE_LIMIT {
            return Err(format!("over the limit of {ARCHIVE_LIMIT} bytes"));
        }
        Self::read(io::Cursor::new(bytes), toolchain).map(|(archive, _)| archive)
    }

    /// Decodes a bounded archive from a file or an HTTP response. Each entry's root is
    /// hashed as bytes come out of its decompressor.
    pub(crate) fn read(reader: impl Read, toolchain: bool) -> Result<(Self, usize), String> {
        let mut input = BufReader::new(reader.take(ARCHIVE_LIMIT as u64 + 1));
        let tag_len = number(&mut input, 0)?;
        let mut tag = [0; ARCHIVE_TAG.len()];
        if tag_len != tag.len() {
            return Err("unknown package archive tag".into());
        }
        input.read_exact(&mut tag).map_err(|e| e.to_string())?;
        if tag != ARCHIVE_TAG.as_bytes() {
            return Err("unknown package archive tag".into());
        }
        let count = number(&mut input, 1 + tag.len())?;
        if count == 0 {
            return Err("no packages".into());
        }
        if count > PACKAGE_LIMIT {
            return Err(format!("over the limit of {PACKAGE_LIMIT} packages"));
        }
        let mut offset = 1 + tag.len() + number_len(count);
        let mut head = None;
        let mut last = None;
        let mut entries = BTreeMap::new();
        let mut total = 0;
        let mut decoder = DCtx::try_create().ok_or("could not allocate a Zstandard decoder")?;
        decoder
            .set_parameter(DParameter::WindowLogMax(WINDOW_LOG))
            .map_err(zstd_safe::get_error_name)?;
        for index in 0..count {
            let len = number(&mut input, offset)?;
            offset += number_len(len);
            if len > ARCHIVE_LIMIT.saturating_sub(offset) {
                return Err(format!("over the limit of {ARCHIVE_LIMIT} bytes"));
            }
            let (root, encoded) = entry(
                (&mut input).take(len as u64),
                Some(len),
                &mut total,
                &mut decoder,
                |_, _, _| {},
            )
            .map_err(|error| format!("bad compressed entry {index}: {error}"))?;
            offset += len;
            let content = Content::decode_as(&encoded, toolchain && index == 0)?;
            if let Some(head) = head {
                if root == head {
                    return Err(format!("archive repeats {root}"));
                }
                ordered(last.as_ref(), &root, "packages", &root)?;
                last = Some(root);
            } else {
                head = Some(root);
            }
            entries.insert(root, Library { content, encoded });
        }
        if !input.fill_buf().map_err(|e| e.to_string())?.is_empty() {
            if offset == ARCHIVE_LIMIT {
                return Err(format!("over the limit of {ARCHIVE_LIMIT} bytes"));
            }
            return Err(format!("trailing bytes at {offset}"));
        }
        Ok((
            Self {
                head: head.unwrap(),
                entries,
            },
            offset,
        ))
    }
}

impl Library {
    pub(crate) fn read_frame(reader: impl Read, len: Option<u64>) -> Result<(Root, Self), String> {
        Self::read_frame_observed(reader, len, |_, _, _| {})
    }

    fn read_frame_observed(
        reader: impl Read,
        len: Option<u64>,
        observe: impl FnMut(&Vec<u8>, usize, &DCtx<'_>),
    ) -> Result<(Root, Self), String> {
        if len.is_some_and(|len| len > FRAME_LIMIT as u64) {
            return Err("compressed frame exceeds 65 MiB".into());
        }
        let mut decoder = DCtx::try_create().ok_or("could not allocate a Zstandard decoder")?;
        decoder
            .set_parameter(DParameter::WindowLogMax(WINDOW_LOG))
            .map_err(zstd_safe::get_error_name)?;
        let (root, encoded) = entry(
            reader.take(FRAME_LIMIT as u64 + 1),
            len.map(|len| len as usize),
            &mut 0,
            &mut decoder,
            observe,
        )?;
        let content = Content::decode_as(&encoded, false)?;
        Ok((root, Self { content, encoded }))
    }
}

fn number(input: &mut impl Read, offset: usize) -> Result<usize, String> {
    read_number(offset, usize::MAX as u64, || {
        let mut byte = [0];
        input.read_exact(&mut byte).map_err(|e| e.to_string())?;
        Ok(byte[0])
    })
    .map(|n| n as usize)
}

fn number_len(n: usize) -> usize {
    (usize::BITS - n.leading_zeros()).div_ceil(7).max(1) as usize
}

fn entry(
    mut frame: impl Read,
    len: Option<usize>,
    total: &mut usize,
    decoder: &mut DCtx<'_>,
    mut observe: impl FnMut(&Vec<u8>, usize, &DCtx<'_>),
) -> Result<(Root, Bytes), String> {
    let mut header = [0; FRAME_HEADER_MAX];
    let header = if let Some(len) = len {
        let header = &mut header[..len.min(FRAME_HEADER_MAX)];
        frame.read_exact(header).map_err(frame_read_error)?;
        header
    } else {
        let mut length = 0;
        while length < header.len() {
            let read = frame
                .read(&mut header[length..])
                .map_err(frame_read_error)?;
            if read == 0 {
                break;
            }
            length += read;
        }
        &mut header[..length]
    };
    let (size, normalized) = frame_header(header)?;
    *total += size;
    if *total > ARCHIVE_LIMIT {
        return Err("entries inflate past 512 MiB".into());
    }

    let mut encoded = frame_buffer(size);
    let frame_len = len.map(|len| len - header.len() + normalized.len());
    let normalized_len = normalized.len();
    let mut compressed = BufReader::new(io::Cursor::new(normalized).chain(frame));
    let mut consumed = 0;
    let mut chunk = vec![0; DCtx::out_size()];
    observe(&encoded, chunk.capacity(), decoder);
    let mut hash = Root::hasher(size);
    loop {
        let data = compressed.fill_buf().map_err(frame_read_error)?;
        if data.is_empty() {
            return Err(NOT_ONE_FRAME.into());
        }
        let mut input = InBuffer { src: data, pos: 0 };
        let available = chunk.len().min(size - encoded.len() + 1);
        let mut output = OutBuffer::around(&mut chunk[..available]);
        let status = decoder.decompress_stream(&mut output, &mut input);
        let produced = output.pos();
        let inflated = encoded.len() + produced;
        if inflated > size {
            return Err(format!("inflates to {inflated} bytes, not {size}"));
        }
        match status {
            Err(code) => {
                let reason = match zstd_safe::get_error_name(code) {
                    "Restored data doesn't match checksum" => "checksum mismatch".to_owned(),
                    _ => NOT_ONE_FRAME.to_owned(),
                };
                return Err(reason);
            }
            Ok(remaining) => {
                let read = input.pos;
                consumed += read;
                compressed.consume(read);
                let bytes = &chunk[..produced];
                hash.update(bytes);
                if inflated > encoded.capacity() {
                    // Vec's automatic doubling can cross even the format limit.
                    let capacity = inflated.max(2 * encoded.capacity()).min(size);
                    encoded.reserve_exact(capacity - encoded.len());
                }
                encoded.extend_from_slice(bytes);
                observe(&encoded, chunk.capacity(), decoder);
                if remaining == 0 {
                    if let Some(len) = frame_len {
                        if consumed != len {
                            return Err(NOT_ONE_FRAME.into());
                        }
                    } else {
                        if consumed + header.len() > FRAME_LIMIT + normalized_len {
                            return Err("compressed frame exceeds 65 MiB".into());
                        }
                        if !compressed.fill_buf().map_err(frame_read_error)?.is_empty() {
                            return Err(NOT_ONE_FRAME.into());
                        }
                    }
                    if inflated != size {
                        return Err(format!("inflates to {inflated} bytes, not {size}"));
                    }
                    break;
                }
            }
        }
    }
    Ok((Root::finish(hash), encoded.into()))
}

fn frame_read_error(error: io::Error) -> String {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        NOT_ONE_FRAME.into()
    } else {
        error.to_string()
    }
}

fn frame_header(header: &[u8]) -> Result<(usize, Vec<u8>), String> {
    if !header.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        return Err(NOT_ONE_FRAME.into());
    }
    let size = zstd_safe::get_frame_content_size(header)
        .map_err(|_| NOT_ONE_FRAME)?
        .ok_or("no content size")?;
    if size > CONTENT_LIMIT as u64 {
        return Err(format!("content size {size} over 64 MiB"));
    }
    let descriptor = header[4];
    let single_segment = descriptor & 0x20 != 0;
    let window = if single_segment {
        size
    } else {
        let descriptor = header[5];
        let base = 1u64 << (10 + (descriptor >> 3));
        base + (base / 8) * u64::from(descriptor & 7)
    };
    if window > WINDOW_LIMIT {
        return Err(format!("window {window} over 8 MiB"));
    }
    if zstd_safe::get_dict_id_from_frame(header).is_some() {
        return Err("needs a dictionary".into());
    }
    // The decoder reports size mismatches as generic corruption before yielding the
    // last block. Validate the declared size ourselves while reading the same blocks.
    // A single-segment frame has no window descriptor. Round its window up to an
    // encodable value: the output cap still limits all available history to size.
    let window_descriptor = if single_segment {
        ((window.max(1024).next_power_of_two().ilog2() - 10) << 3) as u8
    } else {
        header[5]
    };
    let dict_start = if single_segment { 5 } else { 6 };
    let dict_end = dict_start + [0, 1, 2, 4][usize::from(descriptor & 3)];
    let size_len = match descriptor >> 6 {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let mut normalized = header[..4].to_vec();
    normalized.extend([descriptor & 0x1f, window_descriptor]);
    normalized.extend_from_slice(&header[dict_start..dict_end]);
    normalized.extend_from_slice(&header[dict_end + size_len..]);
    Ok((size as usize, normalized))
}

fn frame_buffer(size: usize) -> Vec<u8> {
    Vec::with_capacity(size.min(64 << 10))
}

#[test]
fn frame_buffer_reserves_at_most_64_kib_before_decoding() {
    assert!(frame_buffer(CONTENT_LIMIT).capacity() <= 64 << 10);
    assert_eq!(frame_buffer(19).capacity(), 19);
}

#[test]
fn reviewed_short_reads() {
    short_reads::matrix();
}

#[cfg(test)]
mod short_reads {
    use super::*;
    use crate::property_corpus::reviewed;
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Limits {
        initial_output_bytes: usize,
        max_output_bytes: usize,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Setup {
        lengths: Vec<String>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Schedule {
        max_read: Option<usize>,
    }
    #[derive(Deserialize)]
    #[serde(tag = "status", rename_all = "lowercase", deny_unknown_fields)]
    enum Expected {
        Decoded {
            root: String,
            bytes: usize,
            package: String,
        },
        Refused {
            reason: String,
            output: String,
        },
    }

    struct Reads<'a> {
        bytes: &'a [u8],
        max: usize,
        calls: usize,
        forced_short: usize,
    }
    impl Read for Reads<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let available = buf.len().min(self.bytes.len());
            let len = available.min(self.max);
            self.forced_short += usize::from(len < available);
            self.calls += usize::from(len > 0);
            self.bytes.read(&mut buf[..len])
        }
    }

    type FrameCase = reviewed::Case<Limits, Setup, Expected, Schedule>;

    impl<'a> Reads<'a> {
        fn new(bytes: &'a [u8], schedule: &Schedule) -> Self {
            let max = schedule.max_read.unwrap_or(usize::MAX);
            assert!(max > 0);
            Self {
                bytes,
                max,
                calls: 0,
                forced_short: 0,
            }
        }
    }

    fn check_frame(case: &FrameCase, frame: &[u8], schedule: &Schedule, known: bool) -> usize {
        let id = &case.id;
        let mut reader = Reads::new(frame, schedule);
        let mut observations = Vec::new();
        let decoded = Library::read_frame_observed(
            &mut reader,
            known.then_some(frame.len() as u64),
            |buf, chunk, decoder| {
                observations.push((buf.len(), buf.capacity(), chunk + decoder.sizeof()));
            },
        );
        let &(first_len, first_cap, _) = observations.first().expect("reader reached frame buffer");
        assert_eq!(first_len, 0, "{id}");
        assert!(
            first_cap <= case.limits.initial_output_bytes,
            "{id}: {first_cap}"
        );
        for &(len, cap, workspace) in &observations {
            assert!(
                len <= cap && cap <= first_cap.max(2 * len) && cap <= case.limits.max_output_bytes,
                "{id}: {len} output bytes, {cap} reserved, {workspace} codec workspace bytes"
            );
            assert!(workspace >= DCtx::out_size(), "{id}");
        }
        let &(last_len, last_cap, workspace) = observations.last().unwrap();
        assert_eq!(reader.forced_short > 0, schedule.max_read.is_some(), "{id}");
        eprintln!(
            "{id} known={known} max_read={:?} reads={} output={last_len} capacity={first_cap}..{last_cap} codec_workspace={workspace}",
            schedule.max_read, reader.calls
        );
        match &case.expected {
            Expected::Decoded {
                root,
                bytes,
                package,
            } => {
                let (actual, library) = decoded.unwrap_or_else(|e| panic!("{id}: {e}"));
                assert!(reader.bytes.is_empty(), "{id}");
                assert_eq!(&actual.to_string(), root, "{id}");
                assert_eq!(&library.content.id.to_string(), package, "{id}");
                assert_eq!(last_len, *bytes, "{id}");
                assert!(
                    last_cap <= *bytes,
                    "{id}: {last_cap} reserved for {bytes} decoded bytes"
                );
                assert!(last_cap > first_cap, "{id}: growth not exercised");
                let (canonical_root, canonical) =
                    library.content.inventory().unwrap().encode().unwrap();
                assert_eq!(canonical_root, actual, "{id}");
                assert_eq!(canonical.as_slice(), &library.encoded[..], "{id}");
            }
            Expected::Refused { reason, output } => {
                assert_eq!(&decoded.err().expect("frame accepted"), reason, "{id}");
                assert!(matches!(output.as_str(), "some" | "none"), "{id}");
                assert_eq!(last_len > 0, output == "some", "{id}");
            }
        }
        reader.calls
    }

    fn check_archive(case: &FrameCase, frame: &[u8], schedule: &Schedule) {
        let id = &case.id;
        let mut wrapped = Writer::new(ARCHIVE_LIMIT);
        wrapped.text(ARCHIVE_TAG);
        wrapped.number(1);
        wrapped.frame(frame);
        let mut wrapped = wrapped.finish().unwrap();
        let mut reader = Reads::new(&wrapped, schedule);
        let archive = PackageArchive::read(&mut reader, false);
        match &case.expected {
            Expected::Decoded { root, .. } => {
                let (archive, consumed) = archive.unwrap_or_else(|e| panic!("{id}: {e}"));
                assert_eq!(&archive.head.to_string(), root, "{id}");
                assert_eq!(archive.entries.len(), 1, "{id}");
                assert_eq!(consumed, wrapped.len(), "{id}");
                assert!(reader.bytes.is_empty(), "{id}");
                assert_eq!(reader.forced_short > 0, schedule.max_read.is_some(), "{id}");
                wrapped.push(0);
                assert_eq!(
                    PackageArchive::read(&wrapped[..], false).err().unwrap(),
                    format!("trailing bytes at {consumed}"),
                    "{id}"
                );
            }
            Expected::Refused { reason, .. } => {
                assert_eq!(
                    archive.err().unwrap(),
                    format!("bad compressed entry 0: {reason}"),
                    "{id}"
                );
            }
        }
    }

    pub(super) fn matrix() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../core/tests/decoder-corpus/archive.json");
        let cases = reviewed::read::<Limits, Setup, Expected, Schedule>(
            &std::fs::read(path).unwrap(),
            "archive",
            "reviewed_short_reads",
            "zstd-frame",
            "package-content-v3",
        )
        .unwrap();
        let mut success_cells = 0;
        let mut encodings = BTreeSet::new();
        let mut roots = BTreeSet::new();
        let mut truncated = BTreeSet::new();
        for (case, frame) in cases {
            let id = &case.id;
            assert_eq!(case.setup.lengths, ["known", "unknown"], "{id}");
            assert_eq!(case.limits.initial_output_bytes, 64 << 10, "{id}");
            assert_eq!(case.limits.max_output_bytes, CONTENT_LIMIT, "{id}");
            let [contiguous, short] = case.schedules.as_slice() else {
                panic!("{id}: expected two read schedules")
            };
            assert!(
                contiguous.max_read.is_none() && short.max_read.is_some(),
                "{id}"
            );
            for length in &case.setup.lengths {
                let known = length == "known";
                let whole = check_frame(&case, &frame, contiguous, known);
                let split = check_frame(&case, &frame, short, known);
                assert!(split > whole, "{id}: both paths took {whole} reads");
                if matches!(case.expected, Expected::Decoded { .. }) {
                    success_cells += 2;
                }
            }
            for schedule in &case.schedules {
                check_archive(&case, &frame, schedule);
            }
            match case.expected {
                Expected::Decoded { root, .. } => {
                    roots.insert(root);
                    encodings.insert(frame);
                }
                Expected::Refused { output, .. } => {
                    truncated.insert(output);
                }
            }
        }
        assert_eq!(success_cells, 8);
        assert_eq!(encodings.len(), 2);
        assert_eq!(roots.len(), 1);
        assert_eq!(truncated.len(), 2);
    }
}
