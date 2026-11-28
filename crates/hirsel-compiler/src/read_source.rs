//! Supervisor protocol for reading one member of a canonical C archive.
use crate::{Caps, CheckLibraryCaps, Limit, Refusal, Service, worker};
use std::io::{self, Read, Write};
use std::sync::atomic::AtomicBool;

pub const WORKER_ARG: &str = "__read-source";
pub const MAX_PATH: usize = 1024;
pub const MAX_CONTENT: u64 = CheckLibraryCaps::REGISTRY.max_raw_content_bytes;
pub const INVALID_PATH: i32 = 74;
pub const MISSING_PATH: i32 = 75;
pub const MALFORMED_ARCHIVE: i32 = 76;
pub const OUTPUT_LIMIT: i32 = 77;

pub(crate) fn run(
    service: &Service,
    content: &[u8],
    path: &[u8],
    limit: u64,
    cancelled: &AtomicBool,
) -> Result<worker::Reply, Refusal> {
    worker::run(
        service,
        WORKER_ARG,
        Caps {
            input_bytes: MAX_CONTENT,
            output_bytes: limit.min(content.len() as u64),
            ..Caps::HOST
        },
        4096,
        cancelled,
        |writer| {
            writer.write_all(&limit.to_le_bytes())?;
            writer.write_all(&(path.len() as u64).to_le_bytes())?;
            writer.write_all(&(content.len() as u64).to_le_bytes())?;
            writer.write_all(path)?;
            writer.write_all(content)
        },
    )
}

/// Read the single bounded worker request. These frames are transport only.
/// The core driver validates the path and decodes the archive.
pub fn read_request(mut input: impl Read) -> io::Result<(Vec<u8>, Vec<u8>, u64)> {
    fn word(input: &mut impl Read) -> io::Result<u64> {
        let mut bytes = [0; 8];
        input.read_exact(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    let limit = word(&mut input)?;
    let path_len = word(&mut input)?;
    let content_len = word(&mut input)?;
    if path_len > MAX_PATH as u64 || content_len > MAX_CONTENT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized source request",
        ));
    }
    let mut path = vec![0; path_len as usize];
    input.read_exact(&mut path)?;
    let mut content = vec![0; content_len as usize];
    input.read_exact(&mut content)?;
    if input.read(&mut [0])? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing source request data",
        ));
    }
    Ok((content, path, limit))
}

pub(crate) fn answer(reply: worker::Reply) -> Result<(u64, Vec<u8>), Refusal> {
    let tag = match reply.status.code() {
        Some(0) => 0,
        Some(INVALID_PATH) => 1,
        Some(MISSING_PATH) => 2,
        Some(MALFORMED_ARCHIVE) => 10,
        Some(OUTPUT_LIMIT) => return Err(Refusal::Limit(Limit::Output)),
        _ => {
            return Err(Refusal::Unavailable(format!(
                "the source worker ended with {}: {}",
                reply.status,
                String::from_utf8_lossy(&reply.diagnostics)
            )));
        }
    };
    Ok((tag, reply.bytes))
}
