//! Structure checks for a linked Hirsel Mach-O image.
use object::LittleEndian as LE;
use object::macho::*;
use object::read::macho::{LoadCommandVariant, MachHeader as _, Section as _, Segment as _};
use std::{io, path::Path};

/// Check a thin arm64 macOS executable against the Hirsel link policy.
///
/// Returns an error if the file cannot be read or violates the policy.
pub fn macho_structure(path: &Path) -> io::Result<()> {
    let bytes = std::fs::read(path)?;
    executable_uuid(&bytes)
        .map(|_| ())
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

pub(super) fn executable_uuid(bytes: &[u8]) -> io::Result<[u8; 16]> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    let malformed = |error: object::Error| invalid(error.to_string());
    let header = MachHeader64::<LE>::parse(bytes, 0).map_err(malformed)?;
    if header.cputype(LE) != CPU_TYPE_ARM64
        || header.cpusubtype(LE).0 != CPU_SUBTYPE_ARM64_ALL.0
        || header.filetype(LE) != MH_EXECUTE
    {
        return Err(invalid("expected an arm64 MH_EXECUTE header".into()));
    }
    let mut commands = header.load_commands(LE, bytes, 0).map_err(malformed)?;
    let (mut build, mut uuid, mut signed) = (false, None, false);
    while let Some(command) = commands.next().map_err(malformed)? {
        match command.variant().map_err(malformed)? {
            LoadCommandVariant::BuildVersion(version, _) => {
                if version.platform.get(LE) != PLATFORM_MACOS
                    || version.minos.get(LE) != Version::new(26, 0, 0)
                {
                    return Err(invalid("LC_BUILD_VERSION must declare macOS 26.0".into()));
                }
                build = true;
            }
            LoadCommandVariant::Uuid(value) => uuid = Some(value.uuid),
            LoadCommandVariant::LinkeditData(value) if command.cmd() == LC_CODE_SIGNATURE => {
                signed = value.datasize.get(LE) != 0;
            }
            LoadCommandVariant::Dylib(dylib) => {
                let name = command.string(LE, dylib.dylib.name).map_err(malformed)?;
                if !matches!(
                    name,
                    b"/usr/lib/libSystem.B.dylib"
                        | b"/System/Library/Frameworks/Security.framework/Versions/A/Security"
                        | b"/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation"
                ) {
                    return Err(invalid(format!("unexpected dylib {}", String::from_utf8_lossy(name))));
                }
            }
            LoadCommandVariant::Segment64(segment, data) => {
                let max = segment.maxprot(LE);
                if max.contains(VM_PROT_WRITE) && max.contains(VM_PROT_EXECUTE) {
                    return Err(invalid(format!(
                        "segment {} has writable and executable maxprot",
                        String::from_utf8_lossy(segment.name()),
                    )));
                }
                for section in segment.sections(LE, data).map_err(malformed)? {
                    let name = section.name();
                    if section.size.get(LE) != 0
                        && (matches!(name, b"__mod_init_func" | b"__init_offsets")
                            || matches!(
                                section.section_type(LE),
                                S_MOD_INIT_FUNC_POINTERS | S_INIT_FUNC_OFFSETS
                            ))
                    {
                        return Err(invalid(format!(
                            "initializer entries in {}/{}",
                            String::from_utf8_lossy(segment.name()),
                            String::from_utf8_lossy(name),
                        )));
                    }
                }
            }
            LoadCommandVariant::Rpath(_) => return Err(invalid("unexpected LC_RPATH".into())),
            _ => {}
        }
    }
    if !build {
        return Err(invalid("no LC_BUILD_VERSION".into()));
    }
    if !signed {
        return Err(invalid("no LC_CODE_SIGNATURE".into()));
    }
    uuid.ok_or_else(|| invalid("no LC_UUID".into()))
}
