// Included in std's trace-only formatter. The loader supplies the main image's
// bias, which llvm-symbolizer needs for the absolute PCs of a PIE.
use crate::{fmt, ptr, slice};

pub(super) fn write(out: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut image = Image::default();
    // SAFETY: the callback borrows this live stack value synchronously.
    unsafe { libc::dl_iterate_phdr(Some(main_image), (&raw mut image).cast()) };
    if image.id.is_empty() {
        return writeln!(out, "executable load bias: {:#x}", image.bias);
    }
    out.write_str("executable build-id: ")?;
    for byte in image.id {
        write!(out, "{byte:02x}")?;
    }
    writeln!(out, " load bias: {:#x}", image.bias)
}

#[derive(Default)]
struct Image {
    bias: usize,
    id: &'static [u8],
}

unsafe extern "C" fn main_image(
    info: *mut libc::dl_phdr_info,
    _size: usize,
    opaque: *mut libc::c_void,
) -> libc::c_int {
    // SAFETY: the loader lends its first record, the main executable, for
    // this call. The caller lends its unique output slot.
    let (info, image) = unsafe { (&*info, &mut *opaque.cast::<Image>()) };
    image.bias = info.dlpi_addr as usize;
    // SAFETY: the executable's program headers are loaded for process life.
    let headers = unsafe { slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
    for note in headers
        .iter()
        .filter(|header| header.p_type == libc::PT_NOTE)
    {
        let start = note.p_vaddr as usize;
        let Some(end) = start.checked_add(note.p_filesz as usize) else {
            continue;
        };
        if !headers.iter().any(|load| {
            load.p_type == libc::PT_LOAD
                && load.p_flags & libc::PF_R != 0
                && load.p_vaddr <= note.p_vaddr
                && load
                    .p_vaddr
                    .checked_add(load.p_filesz)
                    .is_some_and(|limit| end as u64 <= limit)
        }) {
            continue;
        }
        // SAFETY: the note lies inside a readable, permanently mapped load
        // segment of the executable. Its ELF bytes have no alignment contract.
        let Some(address) = image.bias.checked_add(start) else {
            continue;
        };
        let bytes =
            unsafe { slice::from_raw_parts(ptr::with_exposed_provenance(address), end - start) };
        if let Some(id) = build_id(bytes) {
            image.id = id;
            break;
        }
    }
    1
}

fn build_id(mut notes: &[u8]) -> Option<&[u8]> {
    while let Some((header, payload)) = notes.split_first_chunk::<12>() {
        let names = u32::from_ne_bytes(header[..4].try_into().ok()?) as usize;
        let bytes = u32::from_ne_bytes(header[4..8].try_into().ok()?) as usize;
        let tag = u32::from_ne_bytes(header[8..].try_into().ok()?);
        let description = payload.get(names.next_multiple_of(4)..)?;
        if tag == 3 && payload.get(..names) == Some(b"GNU\0") {
            return description.get(..bytes);
        }
        notes = description.get(bytes.next_multiple_of(4)..)?;
    }
    None
}
