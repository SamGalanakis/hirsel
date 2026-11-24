use hirsel_core_ids::ContentHash;
use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};

const IMAGE_NOTE: &str = ".note.hirsel.image-digest";
const OWNER: &[u8] = b"Hirsel\0";

pub(crate) enum ImageDigest {
    Assembly,
    Detached,
}

impl ImageDigest {
    pub(crate) fn hash(self, image: &[u8]) -> ContentHash {
        let mut sha = Sha256::new();
        if matches!(self, Self::Detached) {
            // Match the existing ContentHash::of_bytes framing. Assembly
            // records and the launcher's image files use raw SHA-256.
            let domain = b"hirsel.text.v1";
            sha.update((domain.len() as u64).to_le_bytes());
            sha.update(domain);
            sha.update((image.len() as u64).to_le_bytes());
        }
        sha.update(image);
        ContentHash(sha.finalize().into())
    }
}

pub(super) enum Identity<'a> {
    BuildId(&'a [u8]),
    ImageDigest(ContentHash),
}

impl<'a> Identity<'a> {
    pub(super) fn of(file: &object::File<'a>, image: &[u8]) -> io::Result<Self> {
        Ok(match file.build_id().map_err(io::Error::other)? {
            Some(id) if !id.is_empty() => Self::BuildId(id),
            _ => Self::ImageDigest(ImageDigest::Detached.hash(image)),
        })
    }

    fn path(&self) -> PathBuf {
        match self {
            Self::BuildId(bytes) => {
                let id = hex(bytes);
                Path::new(".build-id")
                    .join(&id[..2])
                    .join(format!("{}.debug", &id[2..]))
            }
            Self::ImageDigest(hash) => {
                Path::new(".hirsel-debug").join(format!("{}.debug", hex(&hash.0)))
            }
        }
    }

    pub(super) fn write_note(&self, object: &mut object::write::Object<'_>) {
        match self {
            Self::BuildId(id) => note(
                object,
                ".note.gnu.build-id",
                b"GNU\0",
                object::elf::NT_GNU_BUILD_ID.0,
                id,
            ),
            Self::ImageDigest(hash) => note(object, IMAGE_NOTE, OWNER, 1, &hash.0),
        }
    }

    /// The identity `debug`, a detached debug object, was bound to: the
    /// note [`Self::write_note`] gave it.
    fn recorded(debug: &object::File<'a>) -> io::Result<Option<Self>> {
        if let Some(id) = debug.build_id().map_err(io::Error::other)?
            && !id.is_empty()
        {
            return Ok(Some(Self::BuildId(id)));
        }
        let Some(section) = debug.section_by_name(IMAGE_NOTE) else {
            return Ok(None);
        };
        let data = section.data().map_err(io::Error::other)?;
        let header = note_bytes(OWNER, 1, &[]).len();
        Ok(data
            .get(header..header + 32)
            .map(|hash| Self::ImageDigest(ContentHash(hash.try_into().unwrap()))))
    }

    fn matches(&self, file: &object::File<'_>) -> bool {
        match self {
            Self::BuildId(id) => file.build_id().is_ok_and(|actual| actual == Some(*id)),
            Self::ImageDigest(hash) => file
                .section_by_name(IMAGE_NOTE)
                .and_then(|section| section.data().ok())
                .is_some_and(|data| data == note_bytes(OWNER, 1, &hash.0)),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

fn note_bytes(owner: &[u8], kind: u32, bytes: &[u8]) -> Vec<u8> {
    let mut note = Vec::new();
    note.extend_from_slice(&(owner.len() as u32).to_le_bytes());
    note.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    note.extend_from_slice(&kind.to_le_bytes());
    note.extend_from_slice(owner);
    note.resize(note.len().next_multiple_of(4), 0);
    note.extend_from_slice(bytes);
    note.resize(note.len().next_multiple_of(4), 0);
    note
}

fn note(object: &mut object::write::Object<'_>, name: &str, owner: &[u8], kind: u32, bytes: &[u8]) {
    let section = object.add_section(vec![], name.as_bytes().to_vec(), object::SectionKind::Note);
    object.append_section_data(section, &note_bytes(owner, kind, bytes), 4);
}

// Equal code can bind to different source coordinates. Include the binding
// identity in the linker build ID even when only the side product holds DWARF.
pub(crate) fn add_source_note(object: &mut object::write::Object<'_>, source: ContentHash) {
    // Mach-O has no notes, and no build ID for them to enter.
    if object.format() != object::BinaryFormat::Elf {
        return;
    }
    note(object, ".note.hirsel.source", OWNER, 2, &source.0);
}

fn deliver(path: &Path, image: &[u8], debug: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    // The debug object records the image's identity; hashing the image again
    // would only find it.
    let recorded = object::File::parse(debug).map_err(io::Error::other)?;
    let identity = match Identity::recorded(&recorded)? {
        Some(identity) => identity,
        None => Identity::of(
            &object::File::parse(image).map_err(io::Error::other)?,
            image,
        )?,
    };
    let target = path.with_file_name(identity.path());
    let parent = target.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(debug)?;
    staged.persist(target).map_err(|e| e.error)?;
    if image.starts_with(&0xfeed_facf_u32.to_le_bytes()) {
        write_dsym(path, image, debug)?;
    }
    Ok(())
}

/// A `.debug_aranges` section for every unit in `sections` (Mach-O names).
fn address_ranges(sections: &[([u8; 16], &[u8])]) -> Option<Vec<u8>> {
    use gimli::{EndianSlice, LittleEndian};
    let find = |name: &str| {
        let wanted = format!("__{}", name.trim_start_matches('.'));
        let wanted = &wanted.as_bytes()[..wanted.len().min(16)];
        sections
            .iter()
            .find(|(sectname, _)| {
                &sectname[..wanted.len()] == wanted
                    && sectname.get(wanted.len()).is_none_or(|b| *b == 0)
            })
            .map_or(&[][..], |(_, data)| *data)
    };
    let dwarf = gimli::Dwarf::load(|id| {
        Ok::<_, gimli::Error>(EndianSlice::new(find(id.name()), LittleEndian))
    })
    .ok()?;
    let mut out = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().ok()? {
        let offset = header.offset().to_debug_info_offset(&header)?.0 as u32;
        let unit = dwarf.unit(header).ok()?;
        let mut ranges = dwarf.unit_ranges(&unit).ok()?;
        let mut tuples = Vec::new();
        while let Some(range) = ranges.next().ok()? {
            if range.end > range.begin {
                tuples.push((range.begin, range.end - range.begin));
            }
        }
        // A DWARF32 set: a 12-byte header padded to 16, then the tuples.
        let length = 12 + tuples.len() * 16 + 16;
        out.extend_from_slice(&(length as u32).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&[8, 0, 0, 0, 0, 0]);
        for (address, length) in tuples {
            out.extend_from_slice(&address.to_le_bytes());
            out.extend_from_slice(&length.to_le_bytes());
        }
        out.extend_from_slice(&[0; 16]);
    }
    Some(out)
}

/// macOS debuggers find DWARF in a `.dSYM` bundle beside the executable,
/// matched by its LC_UUID: ld64 never copies DWARF into an executable. The
/// bundle's file is an MH_DSYM image with the executable's UUID and segment
/// layout (without file contents) and the detached DWARF in `__DWARF`.
fn write_dsym(path: &Path, image: &[u8], debug: &[u8]) -> io::Result<()> {
    use object::LittleEndian as LE;
    use object::macho::MachHeader64;
    use object::read::macho::{LoadCommandVariant, MachHeader as _, Segment as _};
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const MH_DSYM: u32 = 0xa;
    const LC_UUID: u32 = 0x1b;
    const LC_SEGMENT_64: u32 = 0x19;
    const S_ATTR_DEBUG: u32 = 0x0200_0000;
    let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let header = MachHeader64::<LE>::parse(image, 0).map_err(io::Error::other)?;
    let mut commands = header
        .load_commands(LE, image, 0)
        .map_err(io::Error::other)?;
    let mut uuid = None;
    let mut segments = Vec::new();
    let mut end = 0u64;
    while let Some(command) = commands.next().map_err(io::Error::other)? {
        match command.variant().map_err(io::Error::other)? {
            LoadCommandVariant::Uuid(command) => uuid = Some(command.uuid),
            LoadCommandVariant::Segment64(segment, _) if segment.name() != b"__LINKEDIT" => {
                // The layout without its contents: no file range anywhere.
                let mut raw = command.raw_data().to_vec();
                raw[40..56].fill(0);
                for section in 0..segment.nsects.get(LE) as usize {
                    let at = 72 + section * 80;
                    raw[at + 48..at + 52].fill(0);
                    raw[at + 56..at + 64].fill(0);
                }
                end = end.max(segment.vmaddr.get(LE) + segment.vmsize.get(LE));
                segments.push(raw);
            }
            _ => {}
        }
    }
    let uuid = uuid.ok_or_else(|| invalid("the executable has no LC_UUID"))?;
    let debug = object::File::parse(debug).map_err(io::Error::other)?;
    let mut dwarf = Vec::new();
    for section in debug.sections() {
        let Some(name) = section
            .name()
            .ok()
            .and_then(|name| name.strip_prefix(".debug_"))
        else {
            continue;
        };
        let data = section.data().map_err(io::Error::other)?;
        if data.is_empty() {
            continue;
        }
        // Mach-O section names hold 16 bytes: "__debug_str_offsets" becomes
        // "__debug_str_offs", as dsymutil writes it.
        let mut sectname = [0u8; 16];
        let full = format!("__debug_{name}");
        let len = full.len().min(16);
        sectname[..len].copy_from_slice(&full.as_bytes()[..len]);
        dwarf.push((sectname, data));
    }
    // lldb maps an address to its unit through .debug_aranges.
    let aranges = address_ranges(&dwarf);
    if let Some(aranges) = &aranges {
        let mut sectname = [0u8; 16];
        sectname[..15].copy_from_slice(b"__debug_aranges");
        dwarf.push((sectname, aranges));
    }
    const PAGE: u64 = 0x4000;
    let commands_size: usize =
        24 + segments.iter().map(Vec::len).sum::<usize>() + 72 + 80 * dwarf.len();
    let data_start = (32 + commands_size as u64).next_multiple_of(PAGE);
    let base = end.next_multiple_of(PAGE);
    let mut data = Vec::new();
    let mut rows = Vec::new();
    for (name, bytes) in &dwarf {
        let at = data.len() as u64;
        data.extend_from_slice(bytes);
        rows.push((name, at, bytes.len() as u64));
    }
    let size = data.len() as u64;
    let mut out = Vec::new();
    let u32le = |out: &mut Vec<u8>, value: u32| out.extend_from_slice(&value.to_le_bytes());
    let u64le = |out: &mut Vec<u8>, value: u64| out.extend_from_slice(&value.to_le_bytes());
    let name16 = |out: &mut Vec<u8>, name: &[u8]| {
        let mut field = [0u8; 16];
        field[..name.len()].copy_from_slice(name);
        out.extend_from_slice(&field);
    };
    u32le(&mut out, MH_MAGIC_64);
    u32le(&mut out, header.cputype(LE).0 as u32);
    u32le(&mut out, header.cpusubtype(LE).0 as u32);
    u32le(&mut out, MH_DSYM);
    u32le(&mut out, 2 + segments.len() as u32);
    u32le(&mut out, commands_size as u32);
    u32le(&mut out, 0);
    u32le(&mut out, 0);
    u32le(&mut out, LC_UUID);
    u32le(&mut out, 24);
    out.extend_from_slice(&uuid);
    for segment in &segments {
        out.extend_from_slice(segment);
    }
    u32le(&mut out, LC_SEGMENT_64);
    u32le(&mut out, 72 + 80 * dwarf.len() as u32);
    name16(&mut out, b"__DWARF");
    u64le(&mut out, base);
    u64le(&mut out, size.next_multiple_of(PAGE));
    u64le(&mut out, data_start);
    u64le(&mut out, size);
    u32le(&mut out, 7);
    u32le(&mut out, 3);
    u32le(&mut out, dwarf.len() as u32);
    u32le(&mut out, 0);
    for (name, at, len) in rows {
        out.extend_from_slice(name);
        name16(&mut out, b"__DWARF");
        u64le(&mut out, base + at);
        u64le(&mut out, len);
        u32le(&mut out, (data_start + at) as u32);
        u32le(&mut out, 0);
        u32le(&mut out, 0);
        u32le(&mut out, 0);
        u32le(&mut out, S_ATTR_DEBUG);
        u32le(&mut out, 0);
        u32le(&mut out, 0);
        u32le(&mut out, 0);
    }
    out.resize(data_start as usize, 0);
    out.extend_from_slice(&data);

    let name = path
        .file_name()
        .ok_or_else(|| invalid("the executable path has no file name"))?;
    let mut bundle = path.as_os_str().to_owned();
    bundle.push(".dSYM");
    let contents = PathBuf::from(bundle).join("Contents");
    let dwarf_dir = contents.join("Resources/DWARF");
    std::fs::create_dir_all(&dwarf_dir)?;
    let identifier = name.to_string_lossy();
    std::fs::write(
        contents.join("Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n\
             \t<key>CFBundleDevelopmentRegion</key>\n\t<string>English</string>\n\
             \t<key>CFBundleIdentifier</key>\n\t<string>com.apple.xcode.dsym.{identifier}</string>\n\
             \t<key>CFBundleInfoDictionaryVersion</key>\n\t<string>6.0</string>\n\
             \t<key>CFBundlePackageType</key>\n\t<string>dSYM</string>\n\
             \t<key>CFBundleSignature</key>\n\t<string>????</string>\n\
             \t<key>CFBundleShortVersionString</key>\n\t<string>1.0</string>\n\
             \t<key>CFBundleVersion</key>\n\t<string>1</string>\n\
             </dict>\n</plist>\n"
        ),
    )?;
    use std::io::Write as _;
    let mut staged = tempfile::NamedTempFile::new_in(&dwarf_dir)?;
    staged.write_all(&out)?;
    staged.persist(dwarf_dir.join(name)).map_err(|e| e.error)?;
    Ok(())
}

pub(crate) struct NativeImage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) debug: Vec<u8>,
}

impl NativeImage {
    fn debug_key(key: ContentHash) -> ContentHash {
        ContentHash::of_parts("hirsel.core-executable-dwarf.v1", [&key.0[..]])
    }

    pub(crate) fn load(store: &crate::Store, key: ContentHash) -> io::Result<Option<Self>> {
        let Some(bytes) = store.get(key)? else {
            return Ok(None);
        };
        let Some(debug) = store.get(Self::debug_key(key))? else {
            return Ok(None);
        };
        Ok(Some(Self { bytes, debug }))
    }

    pub(crate) fn publish(&self, store: &crate::Store, key: ContentHash) -> io::Result<()> {
        store.put_keyed(Self::debug_key(key), &self.debug)?;
        store.put_keyed(key, &self.bytes)
    }

    pub(crate) fn deliver(&self, path: &Path) -> io::Result<()> {
        deliver(path, &self.bytes, &self.debug)?;
        crate::native::deliver(path, &self.bytes)
    }
}

/// The best symbol source that matches the sampled ELF image.
pub enum SymbolSource {
    EmbeddedDwarf,
    DetachedDwarf(Vec<u8>),
    SymtabOnly,
    OffsetsOnly,
}

impl SymbolSource {
    /// The symbol tier recorded for the mapping in a CPU profile.
    pub fn tier(&self) -> &'static str {
        match self {
            Self::EmbeddedDwarf | Self::DetachedDwarf(_) => "full_dwarf",
            Self::SymtabOnly => "symtab_only",
            Self::OffsetsOnly => "offsets_only",
        }
    }
}

fn has_dwarf(file: &object::File<'_>) -> bool {
    [".debug_info", ".debug_line"].into_iter().all(|name| {
        file.section_by_name(name)
            .is_some_and(|section| section.size() != 0)
    })
}

/// Resolve standard ELF DWARF by GNU build ID or the exact image digest.
/// `image` is the sampled mapping's ELF file, even if its path has been replaced.
/// A supplied `debug_path` takes the place of the default detached-file search.
pub fn resolve_symbols(
    image_path: &Path,
    image: &[u8],
    debug_path: Option<&Path>,
) -> io::Result<SymbolSource> {
    let file = object::File::parse(image).map_err(io::Error::other)?;
    if has_dwarf(&file) {
        return Ok(SymbolSource::EmbeddedDwarf);
    }
    let identity = Identity::of(&file, image)?;
    let candidates = if let Some(path) = debug_path {
        vec![path.to_owned()]
    } else {
        let relative = identity.path();
        let mut paths = vec![image_path.with_file_name(&relative)];
        if matches!(identity, Identity::BuildId(_)) {
            paths.push(Path::new("/usr/lib/debug").join(&relative));
        }
        paths
    };
    for path in candidates {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if let Ok(debug) = object::File::parse(bytes.as_slice())
            && file.architecture() == debug.architecture()
            && identity.matches(&debug)
            && has_dwarf(&debug)
        {
            return Ok(SymbolSource::DetachedDwarf(bytes));
        }
    }
    Ok(
        if file
            .symbols()
            .any(|symbol| symbol.kind() == object::SymbolKind::Text && symbol.is_definition())
        {
            SymbolSource::SymtabOnly
        } else {
            SymbolSource::OffsetsOnly
        },
    )
}
