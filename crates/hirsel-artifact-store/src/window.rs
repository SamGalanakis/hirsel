//! Immutable checked windows of a content table. A window owns only its bytes;
//! integrity reads stream the complete record under the caller's reservation.
use super::*;

const SCRATCH: u64 = 64 << 10;

/// Maximum simultaneously allocated byte storage for this read. Control and
/// admission owners remain charged by the request table that calls the reader.
pub fn window_read_bytes(reference: Reference, range: Range<u64>) -> Option<u64> {
    if reference.len > MAX_RECORD as u64 || range.start > range.end || range.end > reference.len {
        return None;
    }
    let length = range.end.checked_sub(range.start)?;
    Bytes::owned_with_owner_bytes(length)?.checked_add(if length == reference.len {
        0
    } else {
        reference.len.min(SCRATCH)
    })
}

/// Capture the demanded bytes during the same integrity pass that checks the
/// complete content reference. Mutating a provider cannot change a returned
/// window or substitute different bytes between checking and copying.
///
/// The caller reserves `window_read_bytes` before calling this function. Reads
/// use std FileExt's exact positioned-I/O contract, supplied by the table owner.
pub fn read_verified_window(
    reference: Reference,
    range: Range<u64>,
    mut read_at: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> Result<Bytes, ContentReadError> {
    window_read_bytes(reference, range.clone()).ok_or(ContentReadError::InvalidContent)?;
    let length =
        usize::try_from(range.end - range.start).map_err(|_| ContentReadError::InvalidContent)?;
    let mut result = vec![0; length];
    if range.start == 0 && range.end == reference.len {
        read_at(0, &mut result).map_err(ContentReadError::Unreadable)?;
        return Bytes::from(result)
            .checked(reference)
            .ok_or(ContentReadError::InvalidContent);
    }
    let mut scratch = vec![0; reference.len.min(SCRATCH) as usize];
    let mut hash = reference.domain.digest(reference.len);
    let mut offset = 0;
    while offset < reference.len {
        let length = (reference.len - offset).min(scratch.len() as u64) as usize;
        let chunk = &mut scratch[..length];
        read_at(offset, chunk).map_err(ContentReadError::Unreadable)?;
        hash.update(&*chunk);
        let start = offset.max(range.start);
        let end = (offset + length as u64).min(range.end);
        if start < end {
            result[(start - range.start) as usize..(end - range.start) as usize]
                .copy_from_slice(&chunk[(start - offset) as usize..(end - offset) as usize]);
        }
        offset += length as u64;
    }
    if Output(ContentHash(hash.finalize().into())) != reference.output {
        return Err(ContentReadError::InvalidContent);
    }
    Ok(Bytes::from(result))
}
