//! Standalone tape inspection uses the same format decoder as replay. Sites
//! are diagnostic source bindings, not stored fields, so show has no site column.
use hirsel_tape_format::{self as format, Blocks, Decoder, Frame, Header, ReadError};
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
        .map_err(block_error)
}
pub(crate) fn run_route(path: &Path) -> io::Result<([u8; 16], Vec<u8>)> {
    let mut file = File::open(path)?;
    format::run_route(|buf| read(&mut file, buf)).map_err(block_error)
}
pub(crate) fn show(path: &Path) -> io::Result<()> {
    let mut file = File::open(path)?;
    let (mut blocks, h) = Blocks::open(|buf| read(&mut file, buf)).map_err(block_error)?;
    println!("format {}  kind {}", format::FORMAT, h.kind);
    println!("identity {}", String::from_utf8_lossy(&h.identity));
    println!(
        "revision {}",
        h.revision
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    println!(
        "tape_id {}",
        h.tape_id
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
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
    let mut decoder = Decoder::default();
    decoder.physical = true;
    // Count REPEAT positions as replay does, so the line starts at its first CHOOSE.
    let mut at = 0;
    while let Some((flags, payload)) = blocks
        .next(|buf| read(&mut file, buf))
        .map_err(block_error)?
    {
        for frame in decoder
            .block(payload, flags & format::END != 0)
            .map_err(damaged)?
        {
            at += 1;
            print!("{at} ");
            match frame {
                Frame::Wait { gap, task } => println!("WAIT gap={gap} task={task}"),
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
                Frame::End { gap, ending } => println!(
                    "END gap={gap} kind={} outcome={:?}",
                    ending.kind, ending.outcome
                ),
            }
        }
    }
    Ok(())
}
