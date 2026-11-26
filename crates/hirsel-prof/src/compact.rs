//! Mach-O compact unwind (`__TEXT,__unwind_info`) for arm64 images. The
//! Darwin linker keeps an `__eh_frame` FDE only for a function whose rules
//! compact encodings cannot state, so most Rust and libSystem functions are
//! described here alone. Layout and encodings: `<mach-o/compact_unwind_encoding.h>`.
use std::sync::Arc;

use crate::unwind::{Registers, Stack, Step};

const MODE: u32 = 0x0F00_0000;
const FRAMELESS: u32 = 0x0200_0000;
const DWARF: u32 = 0x0300_0000;
const FRAME: u32 = 0x0400_0000;
const FRAMELESS_STACK: u32 = 0x00FF_F000;
const REGULAR_PAGE: u32 = 2;
const COMPRESSED_PAGE: u32 = 3;
/// Darwin user addresses fit in 47 bits; the bits above may carry a pointer
/// authentication code in a return address an arm64e library saved.
const ADDRESS: u64 = (1 << 47) - 1;

pub(crate) struct CompactUnwind {
    /// The image's `__TEXT` address, which function offsets count from.
    base: u64,
    section: Arc<[u8]>,
}

impl CompactUnwind {
    pub(crate) fn new(base: u64, section: &[u8]) -> Self {
        Self {
            base,
            section: Arc::from(section),
        }
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let (word, _) = self.section.get(at..)?.split_first_chunk::<4>()?;
        Some(u32::from_le_bytes(*word))
    }

    fn u16(&self, at: usize) -> Option<usize> {
        let (half, _) = self.section.get(at..)?.split_first_chunk::<2>()?;
        Some(u16::from_le_bytes(*half).into())
    }

    /// The encoding of the function containing `address`: `None` outside the
    /// table, and `Err` when the table is malformed.
    pub(crate) fn encoding(&self, address: u64) -> Result<Option<u32>, ()> {
        let Some(target) = address
            .checked_sub(self.base)
            .and_then(|o| u32::try_from(o).ok())
        else {
            return Ok(None);
        };
        let field = |at: usize| self.u32(at).ok_or(());
        if field(0)? != 1 {
            return Err(());
        }
        let common = field(4)? as usize;
        let common_count = field(8)?;
        let index = field(20)? as usize;
        let index_count = field(24)? as usize;
        // The last index entry is a sentinel holding the end of the table.
        let entry = |i: usize| -> Result<(u32, usize), ()> {
            let at = index + 12 * i;
            Ok((field(at)?, field(at + 4)? as usize))
        };
        let pages = index_count.checked_sub(1).ok_or(())?;
        if pages == 0 || target < entry(0)?.0 || target >= entry(pages)?.0 {
            return Ok(None);
        }
        let (mut lo, mut hi) = (0, pages);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if entry(mid)?.0 <= target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (first, page) = entry(lo)?;
        let half = |at: usize| self.u16(at).ok_or(());
        let entries = page + half(page + 4)?;
        let count = half(page + 6)?;
        // The last entry at or below `target` in a page sorted by offset.
        let last_at_or_below = |offset: &dyn Fn(usize) -> Result<u32, ()>| {
            let (mut lo, mut hi) = (0, count);
            while lo < hi {
                let mid = (lo + hi) / 2;
                if offset(mid)? <= target {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            Ok::<_, ()>(lo.checked_sub(1))
        };
        let encoding = match field(page)? {
            REGULAR_PAGE => {
                let Some(i) = last_at_or_below(&|i| field(entries + 8 * i))? else {
                    return Ok(None);
                };
                field(entries + 8 * i + 4)?
            }
            COMPRESSED_PAGE => {
                let word = |i: usize| field(entries + 4 * i);
                let offset = |i: usize| Ok(first + (word(i)? & 0x00FF_FFFF));
                let Some(i) = last_at_or_below(&offset)? else {
                    return Ok(None);
                };
                let slot = word(i)? >> 24;
                if slot < common_count {
                    field(common + 4 * slot as usize)?
                } else {
                    let local = page + half(page + 8)?;
                    let local_count = half(page + 10)?;
                    let slot = (slot - common_count) as usize;
                    if slot >= local_count {
                        return Err(());
                    }
                    field(local + 4 * slot)?
                }
            }
            _ => return Err(()),
        };
        Ok(Some(encoding))
    }

    /// One step from the frame containing `address`, or `None` when the
    /// table has no rules for it.
    pub(crate) fn step(
        &self,
        address: u64,
        registers: &Registers,
        stack: &Stack<'_>,
    ) -> Option<Step> {
        let encoding = match self.encoding(address) {
            Ok(Some(encoding)) => encoding,
            Ok(None) => return None,
            Err(()) => return Some(Step::EvalError),
        };
        Some(match encoding & MODE {
            0 => return None,
            FRAME => {
                // A frame record at FP: the caller's FP, then the return
                // address, with the CFA just above it.
                let Some(fp) = registers.fp else {
                    return Some(Step::EvalError);
                };
                if fp < registers.sp || !fp.is_multiple_of(8) {
                    return Some(Step::EvalError);
                }
                let (Some(previous), Some(pc)) = (stack.word(fp), stack.word(fp + 8)) else {
                    return Some(Step::Truncated);
                };
                match pc & ADDRESS {
                    0 => Step::Root,
                    pc => Step::Caller(Registers {
                        pc,
                        sp: fp + 16,
                        fp: Some(previous & ADDRESS),
                        link: None,
                    }),
                }
            }
            FRAMELESS => {
                // The return address is still in LR, which only the
                // interrupted frame holds.
                let size = u64::from((encoding & FRAMELESS_STACK) >> 12) * 16;
                match (registers.link, registers.sp.checked_add(size)) {
                    (Some(pc), Some(sp)) if pc & ADDRESS != 0 => Step::Caller(Registers {
                        pc: pc & ADDRESS,
                        sp,
                        fp: registers.fp,
                        link: None,
                    }),
                    _ => Step::EvalError,
                }
            }
            // The function's rules are its `__eh_frame` FDE, which the
            // caller looked up first.
            DWARF => Step::EvalError,
            _ => Step::EvalError,
        })
    }
}
