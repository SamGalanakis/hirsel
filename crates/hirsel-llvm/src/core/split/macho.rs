//! The Mach-O forms of the symbol queries and the std rename, read from the
//! object's bytes by <mach-o/loader.h> and <mach-o/nlist.h>, as the rename
//! writes them. Names are the compiler's: the leading `_` is taken off on read
//! and put back on write.
use super::error;

const HEADER: usize = 32;
const NLIST: usize = 16;
const LC_SYMTAB: u32 = 0x2;
const N_STAB: u8 = 0xe0;
const N_EXT: u8 = 0x01;
const N_TYPE: u8 = 0x0e;
const N_UNDF: u8 = 0x0;
const N_SECT: u8 = 0xe;
const N_WEAK_DEF: u16 = 0x0080;

fn word(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| "truncated Mach-O object".to_owned())
}

fn unmangled(name: &[u8]) -> &[u8] {
    name.strip_prefix(b"_").unwrap_or(name)
}

/// The LC_SYMTAB command's offset, if the object has one.
fn symtab(object: &[u8]) -> Result<Option<usize>, String> {
    let mut at = HEADER;
    for _ in 0..word(object, 16)? {
        if word(object, at)? == LC_SYMTAB {
            return Ok(Some(at));
        }
        at += word(object, at + 4)? as usize;
    }
    Ok(None)
}

/// One nlist_64: its offset, name, n_type and n_desc.
struct Symbol<'a> {
    entry: usize,
    name: &'a [u8],
    kind: u8,
    desc: u16,
}

fn symbols(object: &[u8], command: usize) -> Result<Vec<Symbol<'_>>, String> {
    let symoff = word(object, command + 8)? as usize;
    let nsyms = word(object, command + 12)? as usize;
    let stroff = word(object, command + 16)? as usize;
    let strings = object
        .get(stroff..stroff + word(object, command + 20)? as usize)
        .ok_or("truncated Mach-O string table")?;
    (0..nsyms)
        .map(|index| {
            let entry = symoff + index * NLIST;
            let raw = object
                .get(entry..entry + NLIST)
                .ok_or("truncated Mach-O symbol table")?;
            let rest = strings
                .get(word(raw, 0)? as usize..)
                .ok_or("Mach-O symbol name out of range")?;
            let end = rest
                .iter()
                .position(|&b| b == 0)
                .ok_or("unterminated Mach-O symbol name")?;
            Ok(Symbol {
                entry,
                name: &rest[..end],
                kind: raw[4],
                desc: u16::from_le_bytes([raw[6], raw[7]]),
            })
        })
        .collect()
}

fn names(object: &[u8], keep: impl Fn(&Symbol<'_>) -> bool) -> Result<Vec<Vec<u8>>, String> {
    let Some(command) = symtab(object)? else {
        return Ok(Vec::new());
    };
    Ok(symbols(object, command)?
        .iter()
        .filter(|symbol| !symbol.name.is_empty() && symbol.kind & N_STAB == 0 && keep(symbol))
        .map(|symbol| unmangled(symbol.name).to_vec())
        .collect())
}

/// External definitions in a section that are not weak.
pub(super) fn strong_definitions(object: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    names(object, |s| {
        s.kind & N_EXT != 0 && s.kind & N_TYPE == N_SECT && s.desc & N_WEAK_DEF == 0
    })
}

/// External names the object does not define.
pub(super) fn undefined_symbols(object: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    names(object, |s| s.kind & N_EXT != 0 && s.kind & N_TYPE == N_UNDF)
}

/// Renames symbols by appending their new names to the string table, which
/// an object file ends with, and pointing each symbol at its own.
pub(super) fn rename(
    object: &[u8],
    rename: &dyn Fn(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let Some(command) = symtab(object)? else {
        return Ok(object.to_vec());
    };
    let stroff = word(object, command + 16)? as usize;
    let strsize = word(object, command + 20)? as usize;
    if stroff + strsize != object.len() {
        return Err("Mach-O string table is not at the end of the object".into());
    }
    let mut out = object.to_vec();
    let mut strings = Vec::new();
    for symbol in symbols(object, command)? {
        if symbol.name.is_empty() {
            continue;
        }
        if let Some(new) = rename(unmangled(symbol.name)) {
            let offset = u32::try_from(strsize + strings.len()).map_err(error)?;
            if symbol.name.starts_with(b"_") {
                strings.push(b'_');
            }
            strings.extend_from_slice(&new);
            strings.push(0);
            out[symbol.entry..symbol.entry + 4].copy_from_slice(&offset.to_le_bytes());
        }
    }
    strings.resize(strings.len().next_multiple_of(8), 0);
    let size = u32::try_from(strsize + strings.len()).map_err(error)?;
    out.extend_from_slice(&strings);
    out[command + 20..command + 24].copy_from_slice(&size.to_le_bytes());
    Ok(out)
}
