//! Standalone tape inspection uses the same format decoder as replay. Sites
//! are diagnostic source bindings, not stored fields, so show has no site column.
use hirsel_tape_format::{self as format, Blocks, Frame, Header, Index, ReadError, Stop};
use std::os::unix::fs::FileExt;
use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
};
fn damaged(error: impl std::fmt::Debug) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("tape: {error:?}"))
}
fn block_error(error: ReadError<io::Error>) -> io::Error {
    match error {
        ReadError::Source(e) => e,
        ReadError::Format(stop) => damaged(stop),
    }
}

#[derive(Debug)]
pub(crate) struct ReplayDamage(u64);

impl std::fmt::Display for ReplayDamage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "replay: Damaged at block {}", self.0)
    }
}

impl std::error::Error for ReplayDamage {}

fn replay_header_error(error: ReadError<io::Error>) -> io::Error {
    match error {
        ReadError::Format(Stop::Damaged(block)) => {
            io::Error::new(io::ErrorKind::InvalidData, ReplayDamage(block))
        }
        error => block_error(error),
    }
}

fn read(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match file.read(buf) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            result => return result,
        }
    }
}

pub(crate) fn header(path: &Path) -> io::Result<Header> {
    let mut file = File::open(path)?;
    Blocks::open(|buf| read(&mut file, buf))
        .map(|(_, h)| h)
        .map_err(replay_header_error)
}
pub(crate) fn run_route(path: &Path) -> io::Result<([u8; 16], Vec<u8>)> {
    let mut file = File::open(path)?;
    format::run_route(|buf| read(&mut file, buf)).map_err(replay_header_error)
}
enum Record<'a> {
    Header(&'a Header),
    Stream(u32, &'a str),
    Entry(format::Entry),
    Frame(u64, Frame),
    Truncated,
}

#[path = "replay_outputs.rs"]
pub(crate) mod outputs;

pub(crate) fn export(path: &Path, destination: &Path) -> io::Result<()> {
    let parent = destination.parent().filter(|p| !p.as_os_str().is_empty());
    let mut export = tempfile::NamedTempFile::new_in(parent.unwrap_or(Path::new(".")))?;
    {
        let mut writer = io::BufWriter::new(export.as_file_mut());
        inspect(path, |record| write_text(&mut writer, record))?;
        writer.flush()?;
    }
    export.as_file().sync_all()?;
    export.persist_noclobber(destination)?;
    Ok(())
}

fn inspect(path: &Path, mut emit: impl FnMut(Record<'_>) -> io::Result<()>) -> io::Result<()> {
    let file = File::open(path)?;
    let read_at = |offset, buf: &mut [u8]| loop {
        match file.read_at(buf, offset) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            result => break result,
        }
    };
    let (mut index, h) = Index::open(read_at).map_err(block_error)?;
    emit(Record::Header(&h))?;
    let paths: Vec<_> = index.paths().collect();
    for (id, path) in paths {
        emit(Record::Stream(id, &path))?;
        let mut blocks = index.take(id).expect("indexed stream");
        let mut decoder = blocks.decoder(read_at).map_err(block_error)?;
        if let Some(entry) = decoder.take_entry() {
            emit(Record::Entry(entry))?;
        }
        decoder.physical = true;
        // Count REPEAT positions as replay does, so the line starts at its first CHOOSE.
        let mut at = 0u64;
        loop {
            let (flags, payload) = match blocks.next(read_at) {
                Ok(Some(block)) => block,
                Ok(None) => break,
                Err(ReadError::Format(Stop::Missing | Stop::Truncated)) => {
                    emit(Record::Truncated)?;
                    break;
                }
                Err(error) => return Err(block_error(error)),
            };
            let frames = decoder
                .block(payload, flags & format::END != 0)
                .map_err(damaged)?;
            if let Some(entry) = decoder.take_entry() {
                emit(Record::Entry(entry))?;
            }
            for frame in frames {
                at = at
                    .checked_add(1)
                    .ok_or_else(|| damaged("frame position overflow"))?;
                let count = match &frame {
                    Frame::Repeat { count } => *count,
                    _ => 1,
                };
                emit(Record::Frame(at, frame))?;
                at = at
                    .checked_add(count - 1)
                    .ok_or_else(|| damaged("frame position overflow"))?;
            }
        }
    }
    Ok(())
}

fn write_text(writer: &mut impl Write, record: Record) -> io::Result<()> {
    match record {
        Record::Header(h) => {
            writeln!(writer, "format {}  kind {}", format::FORMAT, h.kind)?;
            writeln!(writer, "identity {}", String::from_utf8_lossy(&h.identity))?;
            writeln!(
                writer,
                "revision {}",
                hirsel_core_ids::hex::encode(&h.revision)
            )?;
            writeln!(
                writer,
                "tape_id {}",
                hirsel_core_ids::hex::encode(&h.tape_id)
            )?;
            writeln!(
                writer,
                "limit {}  charge_policy {}  bound {}  chooser {}  arg_bytes {}",
                h.limit,
                h.charge_policy,
                h.bound,
                h.chooser,
                h.arg.len()
            )?;
            writeln!(writer, "arg {:?}  grants {:?}", h.arg, h.net_rules)?;
            for grant in &h.secret_grants {
                writeln!(
                    writer,
                    "grant `{}` [secret] for {}",
                    grant.parameter, grant.policy
                )?;
            }
        }
        Record::Stream(_, path) => writeln!(writer, "{path}")?,
        Record::Entry(entry) => writeln!(writer, "ENTRY {:?}", entry.body())?,
        Record::Truncated => writeln!(writer, "TRUNCATED")?,
        Record::Frame(at, frame) => {
            write!(writer, "{at} ")?;
            match frame {
                Frame::Wait { gap, task } => writeln!(writer, "WAIT gap={gap} task={task}")?,
                Frame::Retired {
                    gap,
                    task,
                    stream,
                    pool,
                } => {
                    writeln!(
                        writer,
                        "RETIRED gap={gap} task={task} stream={stream} ordinal={} intake={}",
                        pool >> 1,
                        pool & 1 != 0
                    )?;
                }
                Frame::Wake { gap, task } => writeln!(writer, "WAKE gap={gap} task={task}")?,
                Frame::Turn { gap, task } => writeln!(writer, "TURN gap={gap} task={task}")?,
                Frame::Cancel { gap, task } => writeln!(writer, "CANCEL gap={gap} task={task}")?,
                Frame::Choose { gap, index } => writeln!(writer, "CHOOSE gap={gap} index={index}")?,
                Frame::Grant { gap, kind, args } => {
                    writeln!(writer, "GRANT gap={gap} kind={kind} args={args:?}")?
                }
                Frame::Issue {
                    task,
                    op,
                    object,
                    args,
                } => writeln!(
                    writer,
                    "ISSUE task={task} op={op} object={object} args={args:?}"
                )?,
                Frame::Done { gap, task, out } => writeln!(
                    writer,
                    "DONE gap={gap} task={task} status={} value={} error={:?} written={} payload={:?}",
                    out.status, out.value, out.error, out.written, out.payload
                )?,
                Frame::Op {
                    task,
                    op,
                    object,
                    args,
                    out,
                } => writeln!(
                    writer,
                    "OP task={task} op={op} object={object} args={args:?} status={} value={} error={:?} written={} payload={:?}",
                    out.status, out.value, out.error, out.written, out.payload
                )?,
                Frame::Repeat { count } => {
                    writeln!(writer, "REPEAT count={count}")?;
                }
                Frame::Take {
                    gap,
                    point,
                    count,
                    body,
                } => writeln!(
                    writer,
                    "TAKE gap={gap} point={point:?} count={count} arrival={:?}",
                    body.arrival()
                )?,
                Frame::HelperEnd { gap, ending } => writeln!(
                    writer,
                    "END gap={gap} kind={} outcome={:?} residual={}",
                    ending.kind(),
                    ending.outcome(),
                    ending.residual()
                )?,
                Frame::End { gap, ending } => writeln!(
                    writer,
                    "END gap={gap} kind={} outcome={:?}",
                    ending.kind, ending.outcome
                )?,
            }
        }
    }
    Ok(())
}
