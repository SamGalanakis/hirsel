//! Synchronous packet operations over already expanded keys.
use crate::{
    Context, media,
    provider::{Buffer, CANCELLED, Outcome},
};
use hirsel_core_net::{
    packet_aead::{self, PacketError, PacketLengths},
    packet_cipher::TAG_LEN,
};
use std::{mem::MaybeUninit, slice};

unsafe fn fault(context: &mut Context, site: u64) -> Outcome {
    use crate::causes::{self, Cause};
    // SAFETY: the adapter supplies the running context and its call site.
    unsafe { causes::record(context, Cause::new(causes::BOUNDS, site)) };
    CANCELLED
}

fn result(answer: Result<usize, PacketError>) -> Outcome {
    let (value, tag, written) = packet_aead::outcome(answer);
    Outcome {
        value,
        error: crate::provider::IoError { tag, code: 0 },
        written,
        ..Outcome::default()
    }
}

fn range(start: u64, end: u64, len: usize) -> Option<std::ops::Range<usize>> {
    (start <= end && end <= len as u64).then(|| start as usize..end as usize)
}

/// The buffers are pinned by lowering. Only the destination is exclusively borrowed.
/// # Safety
/// `buf` is an exclusive valid carrier and `bytes` is at most its spare.
unsafe fn spare(buf: &mut Buffer, bytes: usize) -> &mut [MaybeUninit<u8>] {
    if bytes == 0 {
        return &mut [];
    }
    unsafe { slice::from_raw_parts_mut(buf.data.add(buf.len).cast(), bytes) }
}

/// # Safety
/// Context is running. The sealer and buffer are exclusive and the extra span is shared.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_packet_seal(
    context: *mut Context,
    sealer: u64,
    seq: u64,
    buf: *mut Buffer,
    aad_at: u64,
    at: u64,
    extra: *const Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: lowering supplies the exclusive sealer and buffer and a pinned extra span.
    unsafe {
        let buf = &mut *buf;
        let Some(aad) = range(aad_at, at, buf.len) else {
            return fault(&mut *context, site);
        };
        let sealer = media::loan_mut::<super::schedule::Sealer>(sealer);
        let extra = crate::tls::bytes(&*extra);
        let nonce = match sealer.counter.check(seq).map_err(PacketError::from) {
            Ok(nonce) => nonce,
            Err(error) => return result(Err(error)),
        };
        let lengths = PacketLengths::new(buf.len - aad.end, aad.len(), extra.len());
        if let Err(error) = lengths
            .check(sealer.algorithm)
            .and_then(|()| lengths.spare(TAG_LEN, buf.capacity - buf.len))
        {
            return result(Err(error));
        }
        let initialized = if buf.len == 0 {
            &mut []
        } else {
            slice::from_raw_parts_mut(buf.data, buf.len)
        };
        let (header, body) = initialized.split_at_mut(aad.end);
        let sealed = with_aad(&*context, &header[aad], extra, |aad| {
            sealer.counter.commit(seq);
            sealer
                .cipher
                .seal(nonce, aad, body)
                .expect("validated packet sizes")
        });
        match sealed {
            Ok(tag) => {
                spare(buf, TAG_LEN).write_copy_of_slice(&tag);
                buf.len += TAG_LEN;
                result(Ok(TAG_LEN))
            }
            Err(error) => result(Err(error)),
        }
    }
}

/// # Safety
/// The sealer and destination are exclusive. Inputs and Context remain live through return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_packet_seal_into(
    context: *mut Context,
    sealer: u64,
    seq: u64,
    src: *const Buffer,
    from: u64,
    dst: *mut Buffer,
    aad_at: u64,
    extra: *const Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: lowering pins the borrowed carriers and the exclusive state.
    unsafe {
        let src = crate::tls::bytes(&*src);
        let dst = &mut *dst;
        let extra = crate::tls::bytes(&*extra);
        let Some(plain) = usize::try_from(from).ok().and_then(|from| src.get(from..)) else {
            return fault(&mut *context, site);
        };
        let Some(aad) = range(aad_at, dst.len as u64, dst.len) else {
            return fault(&mut *context, site);
        };
        let sealer = media::loan_mut::<super::schedule::Sealer>(sealer);
        let nonce = match sealer.counter.check(seq).map_err(PacketError::from) {
            Ok(nonce) => nonce,
            Err(error) => return result(Err(error)),
        };
        let available = dst.capacity - dst.len;
        let lengths = PacketLengths::new(plain.len(), aad.len(), extra.len());
        let append = match lengths
            .check(sealer.algorithm)
            .and_then(|()| packet_aead::sealed_len(plain.len(), available))
        {
            Ok(append) => append,
            Err(error) => return result(Err(error)),
        };
        let header = if aad.is_empty() {
            &[]
        } else {
            slice::from_raw_parts(dst.data.add(aad.start), aad.len())
        };
        let tail = spare(dst, append);
        let sealed = with_aad(&*context, header, extra, |aad| {
            sealer.counter.commit(seq);
            sealer
                .cipher
                .seal_into(nonce, aad, plain, tail)
                .expect("validated packet sizes");
            append
        });
        if sealed.is_ok() {
            dst.len += append;
        }
        result(sealed)
    }
}

/// # Safety
/// The opener and extra span are borrowed, and the buffer is exclusive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_packet_open(
    context: *mut Context,
    opener: u64,
    domain: u64,
    seq: u64,
    buf: *mut Buffer,
    aad_at: u64,
    at: u64,
    extra: *const Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: lowering pins the borrowed carriers and opener.
    unsafe {
        let buf = &mut *buf;
        let Some(aad) = range(aad_at, at, buf.len) else {
            return fault(&mut *context, site);
        };
        let opener = media::loan::<super::schedule::Opener>(opener);
        let extra = crate::tls::bytes(&*extra);
        let nonce = match opener.nonce.nonce(domain, seq).map_err(PacketError::from) {
            Ok(nonce) => nonce,
            Err(error) => return result(Err(error)),
        };
        let lengths = PacketLengths::new(
            (buf.len - aad.end).saturating_sub(TAG_LEN),
            aad.len(),
            extra.len(),
        );
        if let Err(error) = lengths.check(opener.algorithm) {
            return result(Err(error));
        }
        let initialized = if buf.len == 0 {
            &mut []
        } else {
            slice::from_raw_parts_mut(buf.data, buf.len)
        };
        let (header, body) = initialized.split_at_mut(aad.end);
        let opened = with_aad(&*context, &header[aad], extra, |aad| {
            opener
                .cipher
                .open(nonce, aad, body)
                .map(|plain| plain.len())
                .map_err(|_| PacketError::Tampered)
        })
        .and_then(std::convert::identity);
        if opened.is_ok() {
            buf.len -= TAG_LEN;
        }
        result(opened)
    }
}

/// # Safety
/// Context and packet inputs are live and the destination is exclusive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_packet_open_into(
    context: *mut Context,
    opener: u64,
    domain: u64,
    seq: u64,
    src: *const Buffer,
    aad_at: u64,
    at: u64,
    extra: *const Buffer,
    dst: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: all buffers are pinned and the destination is disjoint from inputs.
    unsafe {
        let src = crate::tls::bytes(&*src);
        let Some(aad) = range(aad_at, at, src.len()) else {
            return fault(&mut *context, site);
        };
        let opener = media::loan::<super::schedule::Opener>(opener);
        let extra = crate::tls::bytes(&*extra);
        let dst = &mut *dst;
        let nonce = match opener.nonce.nonce(domain, seq).map_err(PacketError::from) {
            Ok(nonce) => nonce,
            Err(error) => return result(Err(error)),
        };
        let sealed = &src[aad.end..];
        let bytes = sealed.len().saturating_sub(TAG_LEN);
        let lengths = PacketLengths::new(bytes, aad.len(), extra.len());
        if let Err(error) = lengths
            .check(opener.algorithm)
            .and_then(|()| lengths.spare(bytes, dst.capacity - dst.len))
        {
            return result(Err(error));
        }
        let tail = spare(dst, bytes);
        let opened = with_aad(&*context, &src[aad], extra, |aad| {
            opener
                .cipher
                .open_into(nonce, aad, sealed, tail)
                .map(|()| bytes)
                .map_err(|_| PacketError::Tampered)
        })
        .and_then(std::convert::identity);
        if opened.is_ok() {
            dst.len += bytes;
        }
        result(opened)
    }
}

fn with_aad<T>(
    context: &Context,
    header: &[u8],
    extra: &[u8],
    use_aad: impl FnOnce(&[u8]) -> T,
) -> Result<T, PacketError> {
    if header.is_empty() {
        return Ok(use_aad(extra));
    }
    if extra.is_empty() {
        return Ok(use_aad(header));
    }
    let bytes = header.len() + extra.len();
    let _charge = super::schedule::admit(context, bytes as u64).map_err(|available| {
        PacketError::Capacity {
            requested: bytes as u64,
            available,
        }
    })?;
    let mut joined = Vec::with_capacity(bytes);
    joined.extend_from_slice(header);
    joined.extend_from_slice(extra);
    Ok(use_aad(&joined))
}

/// # Safety
/// The mask and sample stay borrowed. Output points to five writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_packet_mask(
    context: *mut Context,
    mask: u64,
    sample: *const Buffer,
    at: u64,
    out: *mut [u8; 5],
    site: u64,
) -> Outcome {
    // SAFETY: lowering pins the inputs and provides the inline result slot.
    unsafe {
        let sample = crate::tls::bytes(&*sample);
        let Some(sample) = packet_aead::sample_at(sample, at) else {
            return fault(&mut *context, site);
        };
        let mask = media::loan::<super::schedule::Mask>(mask);
        out.write(mask.key.mask(sample));
        Outcome::default()
    }
}
