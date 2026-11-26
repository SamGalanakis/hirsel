//! Launcher-control session framing.
use super::Wire;
use std::io::{self, Write};

/// The largest session frame either side accepts, derived beside the launcher's other
/// protocol limits.
pub(crate) use crate::launcher::admit::protocol::MAX_FRAME;

pub(crate) fn write_frame(out: &mut impl Write, value: &impl Wire) -> io::Result<()> {
    let body = value.encode();
    if body.len() > MAX_FRAME {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let size = u32::try_from(body.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&size.to_le_bytes());
    frame.extend_from_slice(&body);
    out.write_all(&frame)
}
