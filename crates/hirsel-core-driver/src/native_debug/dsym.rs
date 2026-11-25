//! The detached DWARF of a Mach-O release image as a `.dSYM` bundle, where
//! `lldb` and Apple's tools look with no configuration: an `MH_DSYM` file with
//! the image's `LC_UUID`, its segment layout without file contents, and the
//! DWARF in a `__DWARF` segment. ld64 copies no DWARF into an image and the
//! recipe passes `-S`, so there is no debug map and no `dsymutil`.
use object::{Object as _, ObjectSection as _};
use std::{
    io,
    path::{Path, PathBuf},
};

/// `NAME.dSYM/Contents/Resources/DWARF/NAME` for the image at `image`.
pub(super) fn path(image: &Path) -> PathBuf {
    let name = image.file_name().unwrap_or_default();
    let mut bundle = image.as_os_str().to_owned();
    bundle.push(".dSYM");
    PathBuf::from(bundle)
        .join("Contents/Resources/DWARF")
        .join(name)
}

/// Write the bundle at `target` ([`path`]) for `image` from `debug`, the
/// detached debug object whose `.debug_*` sections it carries.
pub(super) fn write(target: &Path, image: &[u8], debug: &[u8]) -> io::Result<()> {
    let bytes = file(image, debug)?;
    let dwarf = target.parent().unwrap();
    let contents = dwarf.parent().and_then(Path::parent).unwrap();
    std::fs::create_dir_all(dwarf)?;
    let identifier = target.file_name().unwrap_or_default().to_string_lossy();
    let plist = format!(
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
    );
    for (path, bytes) in [
        (contents.join("Info.plist"), plist.as_bytes()),
        (target.to_owned(), &bytes[..]),
    ] {
        let mut staged = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        crate::store::write_bounded(&mut staged, &[bytes])?;
        staged.persist(path).map_err(|e| e.error)?;
    }
    Ok(())
}

/// The `MH_DSYM` file of `image` carrying `debug`'s DWARF.
pub(super) fn file(image: &[u8], debug: &[u8]) -> io::Result<Vec<u8>> {
    use object::LittleEndian as LE;
    use object::macho::MachHeader64;
    use object::read::macho::{LoadCommandVariant, MachHeader as _, Segment as _};
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const MH_DSYM: u32 = 0xa;
    const LC_UUID: u32 = 0x1b;
    const LC_SEGMENT_64: u32 = 0x19;
    const S_ATTR_DEBUG: u32 = 0x0200_0000;
    const PAGE: u64 = 0x4000;
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
    let uuid = uuid.ok_or_else(|| invalid("the image has no LC_UUID"))?;
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
        if !data.is_empty() {
            dwarf.push((section_name(&format!("__debug_{name}")), data));
        }
    }
    // lldb maps an address to its unit through .debug_aranges.
    let aranges = address_ranges(&dwarf);
    if let Some(aranges) = &aranges {
        dwarf.push((section_name("__debug_aranges"), aranges));
    }
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
    u32le(&mut out, MH_MAGIC_64);
    // cputype and cpusubtype, as the image states them.
    out.extend_from_slice(&image[4..12]);
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
    out.extend_from_slice(&section_name("__DWARF"));
    u64le(&mut out, base);
    u64le(&mut out, size.next_multiple_of(PAGE));
    u64le(&mut out, data_start);
    u64le(&mut out, size);
    // maxprot and initprot: read, write, execute; read, write.
    u32le(&mut out, 7);
    u32le(&mut out, 3);
    u32le(&mut out, dwarf.len() as u32);
    u32le(&mut out, 0);
    for (name, at, len) in rows {
        out.extend_from_slice(name);
        out.extend_from_slice(&section_name("__DWARF"));
        u64le(&mut out, base + at);
        u64le(&mut out, len);
        u32le(&mut out, (data_start + at) as u32);
        // align, reloff, nreloc, flags, reserved1-3.
        for word in [0, 0, 0, S_ATTR_DEBUG, 0, 0, 0] {
            u32le(&mut out, word);
        }
    }
    out.resize(data_start as usize, 0);
    out.extend_from_slice(&data);
    Ok(out)
}

/// A Mach-O section or segment name: 16 bytes, so "__debug_str_offsets"
/// becomes "__debug_str_offs", as dsymutil writes it.
fn section_name(name: &str) -> [u8; 16] {
    let mut field = [0u8; 16];
    let len = name.len().min(16);
    field[..len].copy_from_slice(&name.as_bytes()[..len]);
    field
}

/// A `.debug_aranges` section for every unit in `sections` (Mach-O names).
fn address_ranges(sections: &[([u8; 16], &[u8])]) -> Option<Vec<u8>> {
    use gimli::{EndianSlice, LittleEndian};
    let find = |name: &str| {
        let wanted = section_name(&format!("__{}", name.trim_start_matches('.')));
        sections
            .iter()
            .find(|(sectname, _)| *sectname == wanted)
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
        // A DWARF32 set: a 12-byte header padded to 16, then the tuples and
        // a terminating pair.
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
