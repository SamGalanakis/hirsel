//! Standalone tape inspection uses the same format decoder as replay. Sites
//! are diagnostic source bindings, not stored fields, so show has no site column.
use hirsel_tape_format::{self as format, Blocks, Frame, Header, Index, ReadError, Stop};
use std::os::unix::fs::FileExt;
use std::{
    fs::File,
    io::{self, Read},
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
pub(crate) fn show(path: &Path) -> io::Result<()> {
    let file = File::open(path)?;
    let read_at = |offset, buf: &mut [u8]| loop {
        match file.read_at(buf, offset) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            result => break result,
        }
    };
    let (mut index, h) = Index::open(read_at).map_err(block_error)?;
    println!("format {}  kind {}", format::FORMAT, h.kind);
    println!("identity {}", String::from_utf8_lossy(&h.identity));
    println!("revision {}", hirsel_core_ids::hex::encode(&h.revision));
    println!("tape_id {}", hirsel_core_ids::hex::encode(&h.tape_id));
    println!(
        "limit {}  charge_policy {}  bound {}  chooser {}  arg_bytes {}",
        h.limit,
        h.charge_policy,
        h.bound,
        h.chooser,
        h.arg.len()
    );
    println!("arg {:?}  grants {:?}", h.arg, h.net_rules);
    for grant in &h.secret_grants {
        println!("grant `{}` [secret] for {}", grant.parameter, grant.policy);
    }
    let paths: Vec<_> = index.paths().collect();
    for (id, path) in paths {
        println!("{path}");
        let mut blocks = index.take(id).expect("indexed stream");
        let mut decoder = blocks.decoder(read_at).map_err(block_error)?;
        if let Some(entry) = decoder.take_entry() {
            println!("ENTRY {:?}", entry.body());
        }
        decoder.physical = true;
        // Count REPEAT positions as replay does, so the line starts at its first CHOOSE.
        let mut at = 0;
        loop {
            let (flags, payload) = match blocks.next(read_at) {
                Ok(Some(block)) => block,
                Ok(None) => break,
                Err(ReadError::Format(Stop::Missing | Stop::Truncated)) => {
                    println!("TRUNCATED");
                    break;
                }
                Err(error) => return Err(block_error(error)),
            };
            let frames = decoder
                .block(payload, flags & format::END != 0)
                .map_err(damaged)?;
            if let Some(entry) = decoder.take_entry() {
                println!("ENTRY {:?}", entry.body());
            }
            for frame in frames {
                at += 1;
                print!("{at} ");
                match frame {
                    Frame::Wait { gap, task } => println!("WAIT gap={gap} task={task}"),
                    Frame::Retired {
                        gap,
                        task,
                        stream,
                        pool,
                    } => {
                        println!(
                            "RETIRED gap={gap} task={task} stream={stream} ordinal={} intake={}",
                            pool >> 1,
                            pool & 1 != 0
                        );
                    }
                    Frame::Wake { gap, task } => println!("WAKE gap={gap} task={task}"),
                    Frame::Cancel { gap, task } => println!("CANCEL gap={gap} task={task}"),
                    Frame::Choose { gap, index } => println!("CHOOSE gap={gap} index={index}"),
                    Frame::Grant { gap, kind, args } => {
                        println!("GRANT gap={gap} kind={kind} args={args:?}")
                    }
                    Frame::Issue {
                        task,
                        op,
                        object,
                        args,
                    } => println!("ISSUE task={task} op={op} object={object} args={args:?}"),
                    Frame::Done { gap, task, out } => println!(
                        "DONE gap={gap} task={task} status={} value={} error={:?} written={} payload={:?}",
                        out.status, out.value, out.error, out.written, out.payload
                    ),
                    Frame::Op {
                        task,
                        op,
                        object,
                        args,
                        out,
                    } => println!(
                        "OP task={task} op={op} object={object} args={args:?} status={} value={} error={:?} written={} payload={:?}",
                        out.status, out.value, out.error, out.written, out.payload
                    ),
                    Frame::Repeat { count } => {
                        println!("REPEAT count={count}");
                        at += count - 1;
                    }
                    Frame::Take {
                        gap,
                        point,
                        count,
                        body,
                    } => println!(
                        "TAKE gap={gap} point={point:?} count={count} arrival={:?}",
                        body.arrival()
                    ),
                    Frame::HelperEnd { gap, ending } => println!(
                        "END gap={gap} kind={} outcome={:?}",
                        ending.kind(),
                        ending.outcome()
                    ),
                    Frame::End { gap, ending } => println!(
                        "END gap={gap} kind={} outcome={:?}",
                        ending.kind, ending.outcome
                    ),
                }
            }
        }
    }
    Ok(())
}
