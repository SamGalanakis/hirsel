//! The arm64 Mach-O resident link. An edit patches the image in place under
//! the same rule as the ELF patch: every section keeps its size, and every
//! relocation and symbol stays the same, except that a local name may change
//! at the same length. Then no address moves. lld's layout, its branch
//! thunks, stubs, chained fixups, `__LINKEDIT` tables and export trie all
//! stay as they were, so the patch writes no branch island of its own.
//!
//! Two parts of the image are digests of the rest, and the patch derives
//! them again as lld does (lld/MachO `Writer::writeUuid` and
//! `CodeSignatureSection::writeHashes`):
//! - `LC_UUID` is xxh3-64 over the xxh3-64 of each 1 MiB chunk of the image
//!   and of the output's file name, `program`. The UUID field and the
//!   signature's hash slots are zero while the chunks are hashed. Only the
//!   chunks an edit touches are hashed again.
//! - the ad-hoc signature's SHA-256 hash of each 4 KiB page below the
//!   signature. Only the pages an edit or the new UUID touch are hashed
//!   again.
//!
//! Every other byte of a patched image is the byte lld writes for the
//! edited objects. The image is remembered only when a recomputed UUID and
//! signature equal lld's own.
//!
//! lld splits a section at each symbol (`subsections_via_symbols`) and
//! keeps one copy of a weak definition, dropping the pieces of the others.
//! So each piece is placed by a symbol of its own that the image holds once,
//! and a piece that defines a weak symbol is patched only when lld provably
//! kept that piece's copy (`super::Previous::kept`). Any edit outside these
//! rules is linked again by lld.
use super::Edit;
use rustc_hash::FxHashMap;
use sha2::Digest as _;
use std::cell::OnceCell;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const MH_OBJECT: u32 = 1;
const MH_EXECUTE: u32 = 2;
const MH_SUBSECTIONS_VIA_SYMBOLS: u32 = 0x2000;
const LC_SYMTAB: u32 = 0x2;
const LC_SEGMENT_64: u32 = 0x19;
const LC_UUID: u32 = 0x1b;
const LC_CODE_SIGNATURE: u32 = 0x1d;
const LC_LINKER_OPTIMIZATION_HINT: u32 = 0x2e;
const N_STAB: u8 = 0xe0;
const N_PEXT: u8 = 0x10;
const N_TYPE: u8 = 0x0e;
const N_INDR: u8 = 0x0a;
const N_EXT: u8 = 0x01;
const N_SECT: u8 = 0x0e;
const N_WEAK_DEF: u16 = 0x80;
const N_ALT_ENTRY: u16 = 0x200;
const SECTION_TYPE: u32 = 0xff;
const S_REGULAR: u32 = 0;
const S_ATTR_DEBUG: u32 = 0x0200_0000;
/// Zero-fill section types: S_ZEROFILL, S_GB_ZEROFILL,
/// S_THREAD_LOCAL_ZEROFILL.
const ZEROFILL: [u32; 3] = [0x1, 0xc, 0x12];
/// ARM64_RELOC_ADDEND carries an addend for the next entry and writes
/// nothing itself.
const ARM64_RELOC_ADDEND: u32 = 10;
/// The last arm64 relocation type, ARM64_RELOC_AUTHENTICATED_POINTER.
const ARM64_RELOC_LAST: u32 = 11;

const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xfade_0cc0;
const CSMAGIC_CODEDIRECTORY: u32 = 0xfade_0c02;
const CSSLOT_CODEDIRECTORY: u32 = 0;
/// CS_ADHOC | CS_LINKER_SIGNED.
const LINKER_SIGNED: u32 = 0x0002_0002;
const SHA256: u8 = 2;
const PAGE_SHIFT: u8 = 12;
const PAGE: usize = 1 << PAGE_SHIFT;
const HASH: usize = 32;
/// lld hashes the image for its UUID in chunks of this size.
const CHUNK: usize = 1 << 20;
/// The output file name lld hashes into the UUID: every Mach-O link writes
/// `program` (`native_link::macho_link`).
const OUTPUT_NAME: &[u8] = b"program";

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
fn be32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

#[derive(Clone, Copy)]
struct Section {
    segname: [u8; 16],
    sectname: [u8; 16],
    addr: u64,
    size: u64,
    offset: u32,
    reloff: u32,
    nreloc: u32,
    flags: u32,
}
impl Section {
    fn zerofill(&self) -> bool {
        ZEROFILL.contains(&(self.flags & SECTION_TYPE))
    }
    fn named(&self, segment: &[u8], section: &[u8]) -> bool {
        self.segname.split(|&b| b == 0).next() == Some(segment)
            && self.sectname.split(|&b| b == 0).next() == Some(section)
    }
    /// A section whose bytes lld copies, cut at symbols, and relocates
    /// only in its relocations' fields.
    fn patchable(&self) -> bool {
        self.flags & SECTION_TYPE == S_REGULAR
            && self.flags & S_ATTR_DEBUG == 0
            && !self.segname.starts_with(b"__DWARF\0")
            && !self.segname.starts_with(b"__LD\0")
            && !self.named(b"__TEXT", b"__eh_frame")
    }
}

#[derive(Clone, Copy)]
struct Nlist {
    strx: u32,
    kind: u8,
    sect: u8,
    desc: u16,
    value: u64,
}
impl Nlist {
    fn defined(&self) -> bool {
        self.kind & N_STAB == 0 && self.kind & N_TYPE == N_SECT
    }
    fn weak(&self) -> bool {
        self.desc & N_WEAK_DEF != 0
    }
    /// Named across objects: an exported or private-extern symbol.
    fn exported(&self) -> bool {
        self.kind & N_EXT != 0 && self.kind & N_PEXT == 0
    }
}

/// lld keeps a private label (`l…`, `L…`) out of the symbol table.
fn private_label(name: &[u8]) -> bool {
    name.first().is_some_and(|&c| c == b'l' || c == b'L')
}

/// A little-endian arm64 64-bit Mach-O file of one file type.
struct MachO<'a> {
    bytes: &'a [u8],
    flags: u32,
    sections: Vec<Section>,
    /// symoff, nsyms, stroff, strsize.
    symtab: Option<(usize, usize, usize, usize)>,
    uuid: Option<usize>,
    /// dataoff, datasize.
    signature: Option<(usize, usize)>,
    hints: u32,
}
impl<'a> MachO<'a> {
    fn parse(bytes: &'a [u8], filetype: u32) -> Option<Self> {
        if u32_at(bytes, 0)? != MH_MAGIC_64
            || u32_at(bytes, 4)? != CPU_TYPE_ARM64
            || u32_at(bytes, 12)? != filetype
        {
            return None;
        }
        let (ncmds, flags) = (u32_at(bytes, 16)?, u32_at(bytes, 24)?);
        let mut file = Self {
            bytes,
            flags,
            sections: Vec::new(),
            symtab: None,
            uuid: None,
            signature: None,
            hints: 0,
        };
        let mut at = 32;
        for _ in 0..ncmds {
            let (cmd, size) = (u32_at(bytes, at)?, u32_at(bytes, at + 4)? as usize);
            match cmd {
                LC_SEGMENT_64 => {
                    for n in 0..u32_at(bytes, at + 64)? as usize {
                        let s = at + 72 + n * 80;
                        file.sections.push(Section {
                            sectname: bytes.get(s..s + 16)?.try_into().ok()?,
                            segname: bytes.get(s + 16..s + 32)?.try_into().ok()?,
                            addr: u64_at(bytes, s + 32)?,
                            size: u64_at(bytes, s + 40)?,
                            offset: u32_at(bytes, s + 48)?,
                            reloff: u32_at(bytes, s + 56)?,
                            nreloc: u32_at(bytes, s + 60)?,
                            flags: u32_at(bytes, s + 64)?,
                        });
                    }
                }
                LC_SYMTAB => {
                    file.symtab = Some((
                        u32_at(bytes, at + 8)? as usize,
                        u32_at(bytes, at + 12)? as usize,
                        u32_at(bytes, at + 16)? as usize,
                        u32_at(bytes, at + 20)? as usize,
                    ));
                }
                LC_UUID => file.uuid = Some(at + 8),
                LC_CODE_SIGNATURE => {
                    file.signature = Some((
                        u32_at(bytes, at + 8)? as usize,
                        u32_at(bytes, at + 12)? as usize,
                    ));
                }
                LC_LINKER_OPTIMIZATION_HINT => file.hints = u32_at(bytes, at + 12)?,
                _ => {}
            }
            at += size;
        }
        Some(file)
    }
    fn data(&self, section: &Section) -> Option<&'a [u8]> {
        if section.zerofill() {
            return Some(&[]);
        }
        let start = section.offset as usize;
        self.bytes.get(start..start + section.size as usize)
    }
    fn symbols(&self) -> Option<Vec<Nlist>> {
        let (symoff, nsyms, _, _) = self.symtab?;
        (0..nsyms)
            .map(|n| {
                let at = symoff + n * 16;
                Some(Nlist {
                    strx: u32_at(self.bytes, at)?,
                    kind: *self.bytes.get(at + 4)?,
                    sect: *self.bytes.get(at + 5)?,
                    desc: u16_at(self.bytes, at + 6)?,
                    value: u64_at(self.bytes, at + 8)?,
                })
            })
            .collect()
    }
    fn name(&self, symbol: &Nlist) -> Option<&'a [u8]> {
        let (_, _, stroff, strsize) = self.symtab?;
        let table = self.bytes.get(stroff..stroff + strsize)?;
        let data = table.get(symbol.strx as usize..)?;
        Some(&data[..data.iter().position(|&b| b == 0)?])
    }
    /// Where the `len` bytes at image address `address` lie in the file.
    fn offset(&self, address: u64, len: u64) -> Option<usize> {
        self.sections.iter().find_map(|s| {
            (!s.zerofill() && address >= s.addr && address + len <= s.addr + s.size)
                .then(|| s.offset as usize + (address - s.addr) as usize)
        })
    }
}

/// The parts of an image lld derives from the rest of it, and the chunk
/// digests its UUID is made of.
struct Seal {
    chunks: Vec<u64>,
    uuid: usize,
    /// The signature's page hash slots, and the bytes they cover.
    slots: usize,
    limit: usize,
}
impl Seal {
    /// The image's seal, when lld's algorithm gives back its UUID and its
    /// signature exactly.
    fn of(image: &[u8]) -> Option<Self> {
        let file = MachO::parse(image, MH_EXECUTE)?;
        let uuid = file.uuid?;
        let (dataoff, datasize) = file.signature?;
        // Stabs name the link's temporary object files.
        if file.symbols()?.iter().any(|s| s.kind & N_STAB != 0) {
            return None;
        }
        let blob = image.get(dataoff..dataoff + datasize)?;
        if be32_at(blob, 0)? != CSMAGIC_EMBEDDED_SIGNATURE
            || be32_at(blob, 8)? != 1
            || be32_at(blob, 12)? != CSSLOT_CODEDIRECTORY
        {
            return None;
        }
        let directory = be32_at(blob, 16)? as usize;
        let cd = blob.get(directory..)?;
        let pages = dataoff.div_ceil(PAGE);
        if be32_at(cd, 0)? != CSMAGIC_CODEDIRECTORY
            || be32_at(cd, 12)? != LINKER_SIGNED
            || be32_at(cd, 24)? != 0
            || be32_at(cd, 28)? as usize != pages
            || be32_at(cd, 32)? as usize != dataoff
            || *cd.get(36)? as usize != HASH
            || *cd.get(37)? != SHA256
            || *cd.get(39)? != PAGE_SHIFT
        {
            return None;
        }
        let slots = dataoff + directory + be32_at(cd, 16)? as usize;
        image.get(slots..slots + pages * HASH)?;
        let mut seal = Self {
            chunks: Vec::new(),
            uuid,
            slots,
            limit: dataoff,
        };
        seal.chunks = (0..image.len().div_ceil(CHUNK))
            .map(|n| seal.chunk(image, n))
            .collect();
        let matches = seal.uuid_bytes() == image[uuid..uuid + 16]
            && (0..pages)
                .all(|page| seal.page(image, page) == image[slots + page * HASH..][..HASH]);
        matches.then_some(seal)
    }
    /// A chunk's digest as lld takes it: before the UUID and the page
    /// hashes are written.
    fn chunk(&self, image: &[u8], n: usize) -> u64 {
        let range = n * CHUNK..((n + 1) * CHUNK).min(image.len());
        let blanks = [
            (self.uuid, self.uuid + 16),
            (self.slots, self.slots + self.limit.div_ceil(PAGE) * HASH),
        ];
        if blanks
            .iter()
            .all(|&(s, e)| e <= range.start || s >= range.end)
        {
            return twox_hash::XxHash3_64::oneshot(&image[range]);
        }
        let mut chunk = image[range.clone()].to_vec();
        for (s, e) in blanks {
            let (s, e) = (s.max(range.start), e.min(range.end));
            if s < e {
                chunk[s - range.start..e - range.start].fill(0);
            }
        }
        twox_hash::XxHash3_64::oneshot(&chunk)
    }
    /// lld's `LCUuid::writeUuid`.
    fn uuid_bytes(&self) -> [u8; 16] {
        let mut hashes: Vec<u8> = self.chunks.iter().flat_map(|h| h.to_le_bytes()).collect();
        hashes.extend(twox_hash::XxHash3_64::oneshot(OUTPUT_NAME).to_le_bytes());
        let digest = twox_hash::XxHash3_64::oneshot(&hashes);
        let mut uuid = [0; 16];
        uuid[..8].copy_from_slice(b"LLD\xa1UU1D");
        uuid[8..].copy_from_slice(&digest.to_le_bytes());
        uuid.swap(3, 8);
        uuid
    }
    fn page(&self, image: &[u8], page: usize) -> [u8; HASH] {
        let start = page * PAGE;
        sha2::Sha256::digest(&image[start..(start + PAGE).min(self.limit)]).into()
    }
    /// Derive the UUID and the page hashes again after `dirty` changed.
    fn reseal(&mut self, image: &mut [u8], dirty: &[(usize, usize)]) {
        let mut chunks: Vec<usize> = dirty
            .iter()
            .flat_map(|&(s, e)| s / CHUNK..e.div_ceil(CHUNK))
            .collect();
        chunks.sort_unstable();
        chunks.dedup();
        for n in chunks {
            self.chunks[n] = self.chunk(image, n);
        }
        let uuid = self.uuid_bytes();
        image[self.uuid..self.uuid + 16].copy_from_slice(&uuid);
        let mut pages: Vec<usize> = dirty
            .iter()
            .chain([&(self.uuid, self.uuid + 16)])
            .flat_map(|&(s, e)| s / PAGE..e.min(self.limit).div_ceil(PAGE))
            .collect();
        pages.sort_unstable();
        pages.dedup();
        for page in pages {
            let hash = self.page(image, page);
            image[self.slots + page * HASH..][..HASH].copy_from_slice(&hash);
        }
    }
}

/// The image's symbols by name: how many share it and the last one's
/// index; and how many share each string.
struct Symbols {
    by_name: FxHashMap<Vec<u8>, (u32, usize)>,
    strings: FxHashMap<u32, u32>,
    symbols: Vec<Nlist>,
    stroff: usize,
}
impl Symbols {
    fn of(image: &[u8]) -> Option<Self> {
        let file = MachO::parse(image, MH_EXECUTE)?;
        let symbols = file.symbols()?;
        let mut by_name = FxHashMap::default();
        let mut strings = FxHashMap::default();
        for (n, symbol) in symbols.iter().enumerate() {
            let name = file.name(symbol)?;
            let entry = by_name.entry(name.to_vec()).or_insert((0, n));
            *entry = (entry.0 + 1, n);
            *strings.entry(symbol.strx).or_insert(0) += 1;
        }
        Some(Self {
            by_name,
            strings,
            symbols,
            stroff: file.symtab?.2,
        })
    }
    /// The one defined image symbol named `name`.
    fn unique(&self, name: &[u8]) -> Option<&Nlist> {
        match self.by_name.get(name) {
            Some(&(1, n)) if self.symbols[n].defined() => Some(&self.symbols[n]),
            _ => None,
        }
    }
}

/// An arm64 Mach-O image the resident link patches.
pub(super) struct Resident {
    seal: Seal,
    symbols: OnceCell<Option<Symbols>>,
}

impl Resident {
    /// An image lld linked and signed, whose UUID and signature this module
    /// derives exactly as lld did.
    pub(super) fn of(image: &[u8]) -> Option<Self> {
        Some(Self {
            seal: Seal::of(image)?,
            symbols: OnceCell::new(),
        })
    }
    pub(super) fn is_macho(image: &[u8]) -> bool {
        u32_at(image, 0) == Some(MH_MAGIC_64)
    }
    /// The edits that turn `image`, linked with `old`, into lld's link with
    /// `new` in its place. `kept` says whether lld kept this slot's
    /// definition of a weak name.
    pub(super) fn edit(
        &self,
        image: &[u8],
        old: &[u8],
        new: &[u8],
        kept: &dyn Fn(&[u8]) -> bool,
    ) -> Option<Vec<Edit>> {
        let symbols = self.symbols.get_or_init(|| Symbols::of(image)).as_ref()?;
        edit(image, symbols, old, new, kept)
    }
    pub(super) fn apply(&mut self, image: &mut [u8], edits: Vec<Edit>) {
        let mut dirty = Vec::new();
        for edit in edits {
            match edit {
                Edit::Bytes(at, bytes) => {
                    image[at..at + bytes.len()].copy_from_slice(&bytes);
                    dirty.push((at, at + bytes.len()));
                }
                Edit::Rename(old, new) => {
                    let symbols = self.symbols.get_mut().unwrap().as_mut().unwrap();
                    let (count, n) = symbols.by_name.remove(&old).unwrap();
                    let at = symbols.stroff + symbols.symbols[n].strx as usize;
                    image[at..at + new.len()].copy_from_slice(&new);
                    dirty.push((at, at + new.len()));
                    symbols.by_name.insert(new, (count, n));
                }
            }
        }
        self.seal.reseal(image, &dirty);
    }
}

/// One piece lld cuts a section into: a byte range of the section and the
/// symbols in it.
struct Piece {
    start: u64,
    end: u64,
    symbols: Vec<usize>,
}

/// The pieces of section `index`: cut at each symbol but an alternative
/// entry when the object uses subsections via symbols, else one piece
/// (lld/MachO `ObjFile::parseSymbols`).
fn cut_section(file: &MachO<'_>, symbols: &[Nlist], index: usize) -> Vec<Piece> {
    let section = &file.sections[index];
    let mut own: Vec<usize> = (0..symbols.len())
        .filter(|&n| symbols[n].defined() && symbols[n].sect as usize == index + 1)
        .collect();
    own.sort_by_key(|&n| symbols[n].value);
    let mut pieces = vec![Piece {
        start: 0,
        end: section.size,
        symbols: Vec::new(),
    }];
    for n in own {
        let offset = symbols[n].value.saturating_sub(section.addr);
        let last = pieces.last_mut().unwrap();
        if file.flags & MH_SUBSECTIONS_VIA_SYMBOLS != 0
            && offset > last.start
            && offset < section.size
            && symbols[n].desc & N_ALT_ENTRY == 0
        {
            last.end = offset;
            pieces.push(Piece {
                start: offset,
                end: section.size,
                symbols: vec![n],
            });
        } else {
            last.symbols.push(n);
        }
    }
    pieces
}

/// Where in the image lld put `piece` of the old object's section
/// `index`: by a symbol in it that the image holds once. A piece with a
/// weak definition lld did not provably keep (`kept`) may be dropped for
/// another copy, so it has no place.
fn place(
    image: &MachO<'_>,
    held: &Symbols,
    object: &MachO<'_>,
    symbols: &[Nlist],
    index: usize,
    piece: &Piece,
    kept: &dyn Fn(&[u8]) -> bool,
) -> Option<usize> {
    if piece
        .symbols
        .iter()
        .any(|&n| symbols[n].weak() && !object.name(&symbols[n]).is_some_and(kept))
    {
        return None;
    }
    let start = object.sections[index].addr + piece.start;
    piece.symbols.iter().find_map(|&n| {
        let name = object.name(&symbols[n])?;
        if private_label(name) {
            return None;
        }
        let at = held
            .unique(name)?
            .value
            .checked_sub(symbols[n].value - start)?;
        image.offset(at, piece.end - piece.start)
    })
}

/// The bytes of `object`'s section `index` that lld writes from its
/// relocations rather than copies; None for a relocation this link does not
/// patch around.
fn fixed(object: &MachO<'_>, index: usize) -> Option<Vec<bool>> {
    let section = &object.sections[index];
    let mut fixed = vec![false; section.size as usize];
    for n in 0..section.nreloc as usize {
        let at = section.reloff as usize + n * 8;
        let address = u32_at(object.bytes, at)?;
        let info = u32_at(object.bytes, at + 4)?;
        let (length, kind) = (info >> 25 & 3, info >> 28);
        // A scattered relocation, or a type arm64 does not define.
        if address & 0x8000_0000 != 0 || kind > ARM64_RELOC_LAST {
            return None;
        }
        if kind == ARM64_RELOC_ADDEND {
            continue;
        }
        let start = address as usize;
        let end = (start + (1 << length)).min(fixed.len());
        fixed.get_mut(start..end)?.fill(true);
    }
    Some(fixed)
}

/// The global and weak names a Mach-O object defines, each with whether it
/// is weak.
pub(super) fn globals(object: &[u8]) -> Option<Vec<(&[u8], bool)>> {
    let file = MachO::parse(object, MH_OBJECT)?;
    file.symbols()?
        .iter()
        .filter(|s| s.defined() && s.kind & N_EXT != 0)
        .map(|s| Some((file.name(s)?, s.weak())))
        .collect()
}

fn edit(
    image: &[u8],
    held: &Symbols,
    old: &[u8],
    new: &[u8],
    kept: &dyn Fn(&[u8]) -> bool,
) -> Option<Vec<Edit>> {
    let image = MachO::parse(image, MH_EXECUTE)?;
    let (o, n) = (MachO::parse(old, MH_OBJECT)?, MachO::parse(new, MH_OBJECT)?);
    if old.len() != new.len() {
        return None;
    }
    // The object writer can reorder strings when a local symbol is renamed.
    // Compare each nlist's name through its own string offset below. Every
    // load command, relocation and other byte outside these tables must match.
    let (symoff, count, stroff, strsize) = o.symtab?;
    let mut contents: Vec<(usize, usize)> = o
        .sections
        .iter()
        .filter(|s| !s.zerofill())
        .map(|s| (s.offset as usize, s.offset as usize + s.size as usize))
        .chain([(symoff, symoff + count * 16), (stroff, stroff + strsize)])
        .collect();
    contents.sort_unstable();
    let mut at = 0;
    for (start, end) in contents.into_iter().chain([(old.len(), old.len())]) {
        if start > at && old.get(at..start)? != new.get(at..start)? {
            return None;
        }
        at = at.max(end);
    }
    let symbols = o.symbols()?;
    let mut cut: FxHashMap<usize, Vec<Piece>> = FxHashMap::default();
    let mut placed: FxHashMap<(usize, u64), Option<usize>> = FxHashMap::default();
    let mut edits = Vec::new();
    for (symbol, next) in symbols.iter().zip(n.symbols()?) {
        // Keep the relocation's symbol index, definition and placement. An
        // indirect symbol also names its target by a string offset in n_value.
        if (symbol.kind, symbol.sect, symbol.desc, symbol.value)
            != (next.kind, next.sect, next.desc, next.value)
            || symbol.kind & N_TYPE == N_INDR
        {
            return None;
        }
        let (an, bn) = (o.name(symbol)?, n.name(&next)?);
        if an == bn {
            continue;
        }
        // A renamed symbol keeps its length and its definition, is named
        // by no other object, and stays in or out of the symbol table.
        if an.len() != bn.len()
            || !symbol.defined()
            || symbol.weak()
            || symbol.exported()
            || private_label(an) != private_label(bn)
        {
            return None;
        }
        if private_label(an) {
            continue;
        }
        let index = symbol.sect as usize - 1;
        let pieces = cut
            .entry(index)
            .or_insert_with(|| cut_section(&o, &symbols, index));
        let offset = symbol.value - o.sections.get(index)?.addr;
        let piece = pieces.iter().rfind(|p| p.start <= offset)?;
        let at = (*placed
            .entry((index, piece.start))
            .or_insert_with(|| place(&image, held, &o, &symbols, index, piece, kept)))?;
        let image_symbol = held.unique(an)?;
        // The image's symbol of this name is this one, and lld shares a
        // string between symbols of one name.
        if held.strings.get(&image_symbol.strx) != Some(&1)
            || held.by_name.contains_key(bn)
            || image.offset(image_symbol.value, 0) != Some(at + (offset - piece.start) as usize)
        {
            return None;
        }
        edits.push(Edit::Rename(an.to_vec(), bn.to_vec()));
    }
    for (index, section) in o.sections.iter().enumerate() {
        let (a, b) = (o.data(section)?, n.data(section)?);
        if a == b {
            continue;
        }
        if !section.patchable() || o.hints != 0 {
            return None;
        }
        let fixed = fixed(&o, index)?;
        let pieces = cut
            .entry(index)
            .or_insert_with(|| cut_section(&o, &symbols, index));
        for piece in pieces.iter() {
            let range = piece.start as usize..piece.end as usize;
            if a[range.clone()] == b[range.clone()] {
                continue;
            }
            let at = (*placed
                .entry((index, piece.start))
                .or_insert_with(|| place(&image, held, &o, &symbols, index, piece, kept)))?;
            let held = image.bytes.get(at..at + range.len())?;
            let mut d = range.start;
            while d < range.end {
                let h = held[d - range.start];
                if fixed[d] {
                    if a[d] != b[d] {
                        return None;
                    }
                    d += 1;
                    continue;
                }
                // lld copied this byte from the old object.
                if h != a[d] {
                    return None;
                }
                if a[d] == b[d] {
                    d += 1;
                    continue;
                }
                let start = d;
                while d < range.end && !fixed[d] && a[d] != b[d] {
                    if held[d - range.start] != a[d] {
                        return None;
                    }
                    d += 1;
                }
                edits.push(Edit::Bytes(at + start - range.start, b[start..d].to_vec()));
            }
        }
    }
    Some(edits)
}
