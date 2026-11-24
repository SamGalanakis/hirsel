//! Synchronous packet operations over already expanded keys.
use crate::{
    Context, media,
    provider::{Buffer, CANCELLED, Outcome},
};
use hirsel_core_net::{
    packet_aead::{self, PacketError, PacketLengths},
    packet_protection::{Aad, Algorithm},
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
        let tag_len = sealer.algorithm.tag_len();
        let extra = crate::tls::bytes(&*extra);
        let nonce = match sealer.counter.packet_nonce(seq).map_err(PacketError::from) {
            Ok(nonce) => nonce,
            Err(error) => return result(Err(error)),
        };
        let lengths = PacketLengths::new(buf.len - aad.end, aad.len(), extra.len());
        if let Err(error) = lengths
            .check(sealer.algorithm)
            .and_then(|()| lengths.spare(tag_len, buf.capacity - buf.len))
        {
            return result(Err(error));
        }
        #[cfg(feature = "record")]
        if sealer.origin != 0 {
            use super::capture::{self, Delivery};
            let bytes = crate::tls::bytes(buf);
            let header = &bytes[aad.clone()];
            let body = &bytes[aad.end..];
            let body_len = body.len();
            let append = body_len + tag_len;
            let origin = sealer_origin(sealer);
            let args = capture::arguments(
                6,
                (buf.capacity - buf.len) as u64,
                &[
                    &origin[..8 + usize::from(sealer.origin_member != 0)],
                    &seq.to_le_bytes(),
                    &aad_at.to_le_bytes(),
                    &at.to_le_bytes(),
                    header,
                    body,
                    extra,
                ],
            );
            let mut target = Buffer {
                data: buf.data.add(aad.end),
                len: 0,
                capacity: append,
            };
            let answer = with_aad(&*context, sealer.algorithm, header, extra, |aad| {
                capture::issue(
                    context,
                    6,
                    args,
                    Delivery::Packet {
                        value: tag_len as u64,
                        bytes: append,
                        tampered: None,
                    },
                    &mut target,
                    |out| {
                        let body = slice::from_raw_parts_mut(out.data, body_len);
                        let tag = sealer
                            .cipher
                            .as_ref()
                            .expect("live packet key")
                            .seal(nonce, aad, body)
                            .expect("validated packet sizes");
                        out.len = body_len;
                        crate::secret::key_share::append(out, &tag[..tag_len]);
                        result(Ok(tag_len))
                    },
                    site,
                )
            });
            let answer = answer.unwrap_or_else(|error| result(Err(error)));
            if answer.status == 0 && answer.error == crate::provider::IoError::OK {
                sealer.counter.commit(seq);
                buf.len += tag_len;
            }
            return answer;
        }
        let initialized = if buf.len == 0 {
            &mut []
        } else {
            slice::from_raw_parts_mut(buf.data, buf.len)
        };
        let (header, body) = initialized.split_at_mut(aad.end);
        let sealed = with_aad(&*context, sealer.algorithm, &header[aad], extra, |aad| {
            sealer.counter.commit(seq);
            sealer
                .cipher
                .as_ref()
                .expect("live packet key")
                .seal(nonce, aad, body)
                .expect("validated packet sizes")
        });
        match sealed {
            Ok(tag) => {
                spare(buf, tag_len).write_copy_of_slice(&tag[..tag_len]);
                buf.len += tag_len;
                result(Ok(tag_len))
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
        let tag_len = sealer.algorithm.tag_len();
        let nonce = match sealer.counter.packet_nonce(seq).map_err(PacketError::from) {
            Ok(nonce) => nonce,
            Err(error) => return result(Err(error)),
        };
        let available = dst.capacity - dst.len;
        let lengths = PacketLengths::new(plain.len(), aad.len(), extra.len());
        let append = match lengths
            .check(sealer.algorithm)
            .and_then(|()| packet_aead::sealed_len(plain.len(), tag_len, available))
        {
            Ok(append) => append,
            Err(error) => return result(Err(error)),
        };
        let header = if aad.is_empty() {
            &[]
        } else {
            slice::from_raw_parts(dst.data.add(aad.start), aad.len())
        };
        #[cfg(feature = "record")]
        if sealer.origin != 0 {
            use super::capture::{self, Delivery};
            let origin = sealer_origin(sealer);
            let args = capture::arguments(
                7,
                available as u64,
                &[
                    &origin[..8 + usize::from(sealer.origin_member != 0)],
                    &seq.to_le_bytes(),
                    &from.to_le_bytes(),
                    &aad_at.to_le_bytes(),
                    header,
                    plain,
                    extra,
                ],
            );
            let mut target = Buffer {
                data: dst.data.add(dst.len),
                len: 0,
                capacity: append,
            };
            let answer = with_aad(&*context, sealer.algorithm, header, extra, |aad| {
                capture::issue(
                    context,
                    7,
                    args,
                    Delivery::Packet {
                        value: append as u64,
                        bytes: append,
                        tampered: None,
                    },
                    &mut target,
                    |out| {
                        let tail = spare(out, append);
                        sealer
                            .cipher
                            .as_ref()
                            .expect("live packet key")
                            .seal_into(nonce, aad, plain, tail)
                            .expect("validated packet sizes");
                        out.len = append;
                        result(Ok(append))
                    },
                    site,
                )
            });
            let answer = answer.unwrap_or_else(|error| result(Err(error)));
            if answer.status == 0 && answer.error == crate::provider::IoError::OK {
                sealer.counter.commit(seq);
                dst.len += append;
            }
            return answer;
        }
        let tail = spare(dst, append);
        let sealed = with_aad(&*context, sealer.algorithm, header, extra, |aad| {
            sealer.counter.commit(seq);
            sealer
                .cipher
                .as_ref()
                .expect("live packet key")
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
        let tag_len = opener.algorithm.tag_len();
        let extra = crate::tls::bytes(&*extra);
        let nonce = match opener
            .nonce
            .packet_nonce(domain, seq)
            .map_err(PacketError::from)
        {
            Ok(nonce) => nonce,
            Err(error) => return result(Err(error)),
        };
        let lengths = PacketLengths::new(
            (buf.len - aad.end).saturating_sub(tag_len),
            aad.len(),
            extra.len(),
        );
        if let Err(error) = lengths.check(opener.algorithm) {
            return result(Err(error));
        }
        #[cfg(feature = "record")]
        if opener.origin != 0 {
            use super::capture::{self, Delivery};
            let bytes = crate::tls::bytes(buf);
            let header = &bytes[aad.clone()];
            let sealed = &bytes[aad.end..];
            let sealed_len = sealed.len();
            let plain = sealed_len.saturating_sub(tag_len);
            let reason = if sealed_len < tag_len { 1 } else { 2 };
            let args = capture::arguments(
                8,
                0,
                &[
                    &opener.origin.to_le_bytes(),
                    &domain.to_le_bytes(),
                    &seq.to_le_bytes(),
                    &aad_at.to_le_bytes(),
                    &at.to_le_bytes(),
                    header,
                    sealed,
                    extra,
                ],
            );
            let mut target = Buffer {
                data: buf.data.wrapping_add(aad.end),
                len: 0,
                capacity: plain,
            };
            let answer = with_aad(&*context, opener.algorithm, header, extra, |aad| {
                capture::issue(
                    context,
                    8,
                    args,
                    Delivery::Packet {
                        value: plain as u64,
                        bytes: plain,
                        tampered: Some(reason),
                    },
                    &mut target,
                    |out| {
                        if reason == 1 {
                            return tampered(reason);
                        }
                        let body = slice::from_raw_parts_mut(out.data, sealed_len);
                        match opener
                            .cipher
                            .as_ref()
                            .expect("live packet key")
                            .open(nonce, aad, body)
                        {
                            Ok(_) => {
                                out.len = plain;
                                result(Ok(plain))
                            }
                            Err(_) => tampered(reason),
                        }
                    },
                    site,
                )
            });
            let answer = answer.unwrap_or_else(|error| result(Err(error)));
            if answer.status == 0 {
                if answer.error == crate::provider::IoError::OK {
                    buf.len -= tag_len;
                } else if answer.error == tampered(2).error && plain != 0 {
                    slice::from_raw_parts_mut(target.data, plain).fill(0);
                }
            }
            return answer;
        }
        let initialized = if buf.len == 0 {
            &mut []
        } else {
            slice::from_raw_parts_mut(buf.data, buf.len)
        };
        let (header, body) = initialized.split_at_mut(aad.end);
        let opened = with_aad(&*context, opener.algorithm, &header[aad], extra, |aad| {
            opener
                .cipher
                .as_ref()
                .expect("live packet key")
                .open(nonce, aad, body)
                .map_err(|_| PacketError::Tampered)
        })
        .and_then(std::convert::identity);
        if opened.is_ok() {
            buf.len -= tag_len;
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
        let tag_len = opener.algorithm.tag_len();
        let extra = crate::tls::bytes(&*extra);
        let dst = &mut *dst;
        let nonce = match opener
            .nonce
            .packet_nonce(domain, seq)
            .map_err(PacketError::from)
        {
            Ok(nonce) => nonce,
            Err(error) => return result(Err(error)),
        };
        let sealed = &src[aad.end..];
        let bytes = sealed.len().saturating_sub(tag_len);
        let lengths = PacketLengths::new(bytes, aad.len(), extra.len());
        if let Err(error) = lengths
            .check(opener.algorithm)
            .and_then(|()| lengths.spare(bytes, dst.capacity - dst.len))
        {
            return result(Err(error));
        }
        #[cfg(feature = "record")]
        if opener.origin != 0 {
            use super::capture::{self, Delivery};
            let header = &src[aad.clone()];
            let reason = if sealed.len() < tag_len { 1 } else { 2 };
            let args = capture::arguments(
                9,
                (dst.capacity - dst.len) as u64,
                &[
                    &opener.origin.to_le_bytes(),
                    &domain.to_le_bytes(),
                    &seq.to_le_bytes(),
                    &aad_at.to_le_bytes(),
                    &at.to_le_bytes(),
                    header,
                    sealed,
                    extra,
                ],
            );
            let mut target = Buffer {
                data: dst.data.wrapping_add(dst.len),
                len: 0,
                capacity: bytes,
            };
            let answer = with_aad(&*context, opener.algorithm, header, extra, |aad| {
                capture::issue(
                    context,
                    9,
                    args,
                    Delivery::Packet {
                        value: bytes as u64,
                        bytes,
                        tampered: Some(reason),
                    },
                    &mut target,
                    |out| {
                        if reason == 1 {
                            return tampered(reason);
                        }
                        let tail = spare(out, bytes);
                        match opener
                            .cipher
                            .as_ref()
                            .expect("live packet key")
                            .open_into(nonce, aad, sealed, tail)
                        {
                            Ok(()) => {
                                out.len = bytes;
                                result(Ok(bytes))
                            }
                            Err(_) => tampered(reason),
                        }
                    },
                    site,
                )
            });
            let answer = answer.unwrap_or_else(|error| result(Err(error)));
            if answer.status == 0 && answer.error == crate::provider::IoError::OK {
                dst.len += bytes;
            }
            return answer;
        }
        let tail = spare(dst, bytes);
        let opened = with_aad(&*context, opener.algorithm, &src[aad], extra, |aad| {
            opener
                .cipher
                .as_ref()
                .expect("live packet key")
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

#[cfg(feature = "record")]
fn tampered(reason: u32) -> Outcome {
    let mut outcome = result(Err(PacketError::Tampered));
    outcome.error.code = reason;
    outcome
}

fn with_aad<T>(
    context: &Context,
    algorithm: Algorithm,
    header: &[u8],
    extra: &[u8],
    use_aad: impl FnOnce(Aad<'_>) -> T,
) -> Result<T, PacketError> {
    if matches!(algorithm, Algorithm::Cm) {
        return Ok(use_aad((header, extra)));
    }
    if header.is_empty() {
        return Ok(use_aad((extra, &[])));
    }
    if extra.is_empty() {
        return Ok(use_aad((header, &[])));
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
    Ok(use_aad((&joined, &[])))
}

#[cfg(feature = "record")]
fn sealer_origin(sealer: &super::schedule::Sealer) -> [u8; 9] {
    let mut origin = [0; 9];
    origin[..8].copy_from_slice(&sealer.origin.to_le_bytes());
    origin[8] = sealer.origin_member;
    origin
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
        #[cfg(feature = "record")]
        if mask.origin != 0 {
            use super::capture::{self, Delivery};
            let args = capture::arguments(
                10,
                0,
                &[&mask.origin.to_le_bytes(), &at.to_le_bytes(), sample],
            );
            let mut target = Buffer {
                data: out.cast(),
                len: 0,
                capacity: 5,
            };
            return capture::issue(
                context,
                10,
                args,
                Delivery::Packet {
                    value: 0,
                    bytes: 5,
                    tampered: None,
                },
                &mut target,
                |out| {
                    let bytes = mask.key.as_ref().expect("live header key").mask(sample);
                    crate::secret::key_share::append(out, &bytes);
                    Outcome::default()
                },
                site,
            );
        }
        out.write(mask.key.as_ref().expect("live header key").mask(sample));
        Outcome::default()
    }
}
