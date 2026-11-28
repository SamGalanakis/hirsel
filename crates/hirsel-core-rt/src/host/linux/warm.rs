//! Warm coverage feedback's host mechanisms (`crate::coverage`'s `warm`):
//! the executable's own `main`, and the process counters of its drift check.
use std::os::unix::fs::FileExt as _;

/// This executable's `main`, from its own symbol table: an executable
/// exports no `main` to the dynamic linker.
pub(crate) fn main_entry() -> Option<usize> {
    let file = std::fs::File::open("/proc/self/exe").ok()?;
    let read = |at: u64, len: usize| -> Option<Vec<u8>> {
        let mut bytes = vec![0u8; len];
        file.read_exact_at(&mut bytes, at).ok()?;
        Some(bytes)
    };
    let u16_at = |b: &[u8], at: usize| u16::from_le_bytes(b[at..at + 2].try_into().unwrap());
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    let u64_at = |b: &[u8], at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    let header = read(0, 64)?;
    if header[..4] != *b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return None;
    }
    let (entry, sections) = (u64_at(&header, 24), u64_at(&header, 40));
    let (size, count) = (
        usize::from(u16_at(&header, 58)),
        usize::from(u16_at(&header, 60)),
    );
    if size < 64 {
        return None;
    }
    let table = read(sections, size * count)?;
    let section = |i: usize| table.get(i * size..(i + 1) * size);
    let symtab = (0..count).filter_map(section).find(|s| u32_at(s, 4) == 2)?;
    let strtab = section(u32_at(symtab, 40) as usize)?;
    let symbols = read(
        u64_at(symtab, 24),
        usize::try_from(u64_at(symtab, 32)).ok()?,
    )?;
    let names = read(
        u64_at(strtab, 24),
        usize::try_from(u64_at(strtab, 32)).ok()?,
    )?;
    let value = symbols.chunks_exact(24).find_map(|symbol| {
        let name = names.get(u32_at(symbol, 0) as usize..)?;
        // A global defined function named exactly `main`.
        (symbol[4] == 0x12 && u16_at(symbol, 6) != 0 && name.starts_with(b"main\0"))
            .then(|| u64_at(symbol, 8))
    })?;
    // SAFETY: getauxval has no preconditions.
    let loaded = unsafe { libc::getauxval(libc::AT_ENTRY) };
    let address = value.wrapping_add(loaded.wrapping_sub(entry));
    (address != 0).then_some(address as usize)
}

/// Bytes the allocator holds for the program, its arena, mapped regions,
/// resident pages, open descriptors and threads.
pub(crate) fn counters() -> [u64; 6] {
    #[cfg(target_env = "gnu")]
    // SAFETY: mallinfo has no preconditions.
    let (held, arena) = {
        #[allow(deprecated)]
        let info = unsafe { libc::mallinfo() };
        let field = |value: libc::c_int| u64::from(value as u32);
        (
            field(info.uordblks) + field(info.hblkhd),
            field(info.arena) + field(info.hblkhd),
        )
    };
    #[cfg(not(target_env = "gnu"))]
    let (held, arena) = (0, 0);
    let maps = std::fs::read("/proc/self/maps").map_or(0, |maps| {
        maps.iter().filter(|&&byte| byte == b'\n').count() as u64
    });
    let resident = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split(' ').nth(1)?.parse().ok())
        .unwrap_or(0);
    let entries = |path| std::fs::read_dir(path).map_or(0, |dir| dir.count() as u64);
    // The listing's own descriptor is open while it reads.
    let descriptors = entries("/proc/self/fd").saturating_sub(1);
    [
        held,
        arena,
        maps,
        resident,
        descriptors,
        entries("/proc/self/task"),
    ]
}
