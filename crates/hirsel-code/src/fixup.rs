//! Fixup application (PACK-2): a pure function of the bytes and the two addresses, so
//! it runs the same on every host and on a private copy.

use hirsel_core_layout::Arch;
use hirsel_mod::{Fixup, Kind};

/// `base + addend`, when it is inside a section of `length` bytes or one past its end.
fn inside(base: u64, length: usize, addend: i64) -> Option<u64> {
    (0..=length as i64)
        .contains(&addend)
        .then(|| base + addend as u64)
}

fn word(bytes: &mut [u8], site: usize) -> Option<&mut [u8; 4]> {
    bytes.get_mut(site..)?.first_chunk_mut()
}

fn in_text(arch: Arch, text: &mut [u8], t: u64, d: u64, data: usize, fixup: Fixup) -> Option<()> {
    let to = inside(d, data, fixup.addend)?;
    let site = fixup.site as usize;
    let p = t + site as u64;
    match arch {
        Arch::X86_64 => match fixup.kind {
            Kind::Rel32Data => {
                let bias = usize::from(fixup.bias);
                // The field follows a ModRM byte that names `[rip + disp32]`.
                let form = site.checked_sub(1).and_then(|at| text.get(at))? & 0xc7 == 0x05;
                if !(4..=8).contains(&bias) || site + bias > text.len() || !form {
                    return None;
                }
                let value = i32::try_from(to as i64 - (p as i64 + bias as i64)).ok()?;
                *word(text, site)? = value.to_le_bytes();
            }
            Kind::Abs64Text
            | Kind::Abs64Data
            | Kind::PageData
            | Kind::Low12AddData
            | Kind::Low12LoadData => return None,
        },
        Arch::Aarch64 => {
            if fixup.bias != 0 || site % 4 != 0 {
                return None;
            }
            let field = word(text, site)?;
            let old = u32::from_le_bytes(*field);
            let low = (to & 0xfff) as u32;
            let new = match fixup.kind {
                Kind::PageData => {
                    let pages = (to >> 12) as i64 - (p >> 12) as i64;
                    if old & 0x9f00_0000 != 0x9000_0000 || !(-(1 << 20)..1 << 20).contains(&pages) {
                        return None;
                    }
                    let pages = pages as u32 & 0x1f_ffff;
                    old & 0x9f00_001f | (pages & 3) << 29 | (pages >> 2) << 5
                }
                Kind::Low12AddData => {
                    if old & 0x7fc0_0000 != 0x1100_0000 {
                        return None;
                    }
                    old & !(0xfff << 10) | low << 10
                }
                Kind::Low12LoadData => {
                    if old & 0x3b00_0000 != 0x3900_0000 {
                        return None;
                    }
                    // A vector register's 128-bit access has size 0 and the high
                    // operation bit set.
                    let wide = old & 0x0480_0000 == 0x0480_0000;
                    let scale = match (wide, old >> 30) {
                        (true, 0) => 4,
                        (true, _) => return None,
                        (false, size) => size,
                    };
                    if low & ((1 << scale) - 1) != 0 {
                        return None;
                    }
                    old & !(0xfff << 10) | (low >> scale) << 10
                }
                Kind::Abs64Text | Kind::Abs64Data | Kind::Rel32Data => return None,
            };
            *field = new.to_le_bytes();
        }
    }
    Some(())
}

fn in_data(data: &mut [u8], t: u64, d: u64, text: usize, fixup: Fixup) -> Option<()> {
    let value = match fixup.kind {
        Kind::Abs64Text => inside(t, text, fixup.addend)?,
        Kind::Abs64Data => inside(d, data.len(), fixup.addend)?,
        Kind::Rel32Data | Kind::PageData | Kind::Low12AddData | Kind::Low12LoadData => return None,
    };
    if fixup.bias != 0 {
        return None;
    }
    let field: &mut [u8; 8] = data.get_mut(fixup.site as usize..)?.first_chunk_mut()?;
    *field = value.to_le_bytes();
    Some(())
}

/// Patch `text`, which will run at address `t`, and `data`, which will sit at `d`. A
/// refused fixup is named by its index, the text table's records first.
pub(crate) fn apply(
    arch: Arch,
    (text, t): (&mut [u8], u64),
    (data, d): (&mut [u8], u64),
    text_fixups: impl Iterator<Item = Fixup>,
    data_fixups: impl Iterator<Item = Fixup>,
) -> Result<(), u32> {
    let mut index = 0u32;
    for fixup in text_fixups {
        in_text(arch, text, t, d, data.len(), fixup).ok_or(index)?;
        index += 1;
    }
    for fixup in data_fixups {
        in_data(data, t, d, text.len(), fixup).ok_or(index)?;
        index += 1;
    }
    Ok(())
}
