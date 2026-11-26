use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use gimli::{AttributeValue, EndianArcSlice, LittleEndian, UnitOffset, UnitRef};
use object::{Object, ObjectSection, ObjectSegment, ObjectSymbol};
use sha2::{Digest, Sha256};

pub(crate) use crate::capture::SymbolSource;
use crate::profile::Result;
use crate::record::Mapping;
use crate::unwind::UnwindInfo;

type Reader = EndianArcSlice<LittleEndian>;

impl SymbolSource {
    pub(crate) fn tier(&self) -> &'static str {
        match self {
            Self::EmbeddedDwarf | Self::DetachedDwarf(_) => "full_dwarf",
            Self::SymtabOnly => "symtab_only",
            Self::OffsetsOnly => "offsets_only",
        }
    }
}

pub(crate) fn local_symbols(image: &[u8]) -> SymbolSource {
    let Ok(file) = object::File::parse(image) else {
        return SymbolSource::OffsetsOnly;
    };
    if file.section_by_name(".debug_info").is_some() {
        SymbolSource::EmbeddedDwarf
    } else if !text_symbols(&file).is_empty() {
        SymbolSource::SymtabOnly
    } else {
        SymbolSource::OffsetsOnly
    }
}

pub(crate) struct Image {
    pub(crate) build_id: Option<String>,
    pub(crate) digest: String,
    pub(crate) unwind: UnwindInfo,
    pub(crate) main: bool,
    pub(crate) remarks: Vec<SourceRemark>,
    dwarf: Option<addr2line::Context<Reader>>,
    symbols: Vec<Symbol>,
    segments: Vec<Segment>,
}

/// Definition, instance, source file, line, column, kind, outcome and count.
pub(crate) type SourceRemark = (String, String, String, u32, u32, String, String, u64);

struct Symbol {
    name: String,
    start: u64,
    end: u64,
}

struct Segment {
    offset: u64,
    len: u64,
    address: u64,
    align: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Function {
    pub(crate) def_path: Option<String>,
    pub(crate) instance: String,
    pub(crate) name: String,
    pub(crate) kind: &'static str,
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) struct Symbolized {
    pub(crate) function: Option<Function>,
    pub(crate) file: Option<String>,
    pub(crate) line: Option<u32>,
    pub(crate) col: Option<u32>,
    pub(crate) inlined: bool,
}

/// The image's defined code symbols by address. A Mach-O symbol carries no
/// size and its name a leading `_`: it ends where the next symbol or its
/// section does, and its name is the one the source gave.
fn text_symbols(file: &object::File<'_>) -> Vec<Symbol> {
    let macho = file.format() == object::BinaryFormat::MachO;
    let mut symbols = Vec::new();
    for symbol in file.symbols().chain(file.dynamic_symbols()) {
        if !symbol.is_definition() || symbol.kind() != object::SymbolKind::Text {
            continue;
        }
        let Ok(name) = symbol.name() else {
            continue;
        };
        let end = if macho {
            symbol
                .section_index()
                .and_then(|index| file.section_by_index(index).ok())
                .and_then(|section| section.address().checked_add(section.size()))
        } else {
            symbol
                .address()
                .checked_add(symbol.size())
                .filter(|_| symbol.size() != 0)
        };
        if let Some(end) = end {
            symbols.push(Symbol {
                name: if macho {
                    name.strip_prefix('_').unwrap_or(name)
                } else {
                    name
                }
                .to_owned(),
                start: symbol.address(),
                end,
            });
        }
    }
    symbols.sort_by_key(|symbol| (symbol.start, symbol.end));
    if macho {
        for index in 1..symbols.len() {
            let next = symbols[index].start;
            let previous = &mut symbols[index - 1];
            if next > previous.start {
                previous.end = previous.end.min(next);
            }
        }
    }
    symbols
}

impl Image {
    pub(crate) fn parse(bytes: &[u8], source: SymbolSource, main: bool) -> Result<Self> {
        let file = object::File::parse(bytes)?;
        #[cfg(target_arch = "x86_64")]
        let arch = object::Architecture::X86_64;
        #[cfg(target_arch = "aarch64")]
        let arch = object::Architecture::Aarch64;
        if file.architecture() != arch || !file.is_little_endian() {
            return Err("profiling requires a native little-endian image".into());
        }
        let section = |name| {
            file.section_by_name(name)
                .and_then(|s| s.data().ok().map(|data| (s.address(), data)))
        };
        let embedded_frame = section(".debug_frame").map(|(_, data)| data);
        let debug_file = match &source {
            SymbolSource::EmbeddedDwarf => Some(object::File::parse(bytes)?),
            SymbolSource::DetachedDwarf(debug) => Some(object::File::parse(debug.as_slice())?),
            _ => None,
        };
        let detached_frame = debug_file
            .as_ref()
            .and_then(|debug| debug.section_by_name(".debug_frame"))
            .and_then(|s| s.data().ok());
        let remarks = debug_file
            .as_ref()
            .and_then(|file| file.section_by_name(crate::CODEGEN_REMARK_SECTION))
            .map(|section| {
                Ok::<_, Box<dyn std::error::Error>>(serde_json::from_slice(section.data()?)?)
            })
            .transpose()?
            .unwrap_or_default();
        let unwind = UnwindInfo::new(
            section(".eh_frame"),
            embedded_frame.or(detached_frame),
            file.section_by_name(".text").map_or(0, |s| s.address()),
            file.section_by_name(".got").map_or(0, |s| s.address()),
        );
        let dwarf = debug_file
            .map(|debug| {
                let dwarf = gimli::Dwarf::load(|id| -> object::Result<Reader> {
                    let bytes = debug
                        .section_by_name(id.name())
                        .map(|s| s.data())
                        .transpose()?;
                    Ok(EndianArcSlice::new(
                        Arc::from(bytes.unwrap_or_default()),
                        LittleEndian,
                    ))
                })?;
                Ok::<_, Box<dyn std::error::Error>>(addr2line::Context::from_dwarf(dwarf)?)
            })
            .transpose()?;
        let symbols = if matches!(source, SymbolSource::OffsetsOnly) {
            Vec::new()
        } else {
            text_symbols(&file)
        };
        let segments = file
            .segments()
            .filter_map(|segment| {
                let executable = match segment.flags() {
                    object::SegmentFlags::Elf { p_flags, .. } => {
                        p_flags & object::elf::PF_X == object::elf::PF_X
                    }
                    object::SegmentFlags::MachO { initprot, .. } => {
                        initprot.contains(object::macho::VM_PROT_EXECUTE)
                    }
                    _ => false,
                };
                if !executable {
                    return None;
                }
                let (offset, len) = segment.file_range();
                Some(Segment {
                    offset,
                    len,
                    address: segment.address(),
                    align: segment.align(),
                })
            })
            .collect();
        Ok(Self {
            remarks,
            build_id: file.build_id()?.map(hex),
            digest: hex(&Sha256::digest(bytes)),
            unwind,
            main,
            dwarf,
            symbols,
            segments,
        })
    }

    pub(crate) fn virtual_start(&self, mapping: &Mapping) -> Option<u64> {
        self.segments.iter().find_map(|segment| {
            let alignment = segment.align.max(1);
            let lower = segment.offset - segment.offset % alignment;
            if !(lower..segment.offset.checked_add(segment.len)?).contains(&mapping.offset) {
                return None;
            }
            let delta = i128::from(mapping.offset) - i128::from(segment.offset);
            u64::try_from(i128::from(segment.address) + delta).ok()
        })
    }

    pub(crate) fn physical_name(&self, address: u64) -> Option<&str> {
        let before = self
            .symbols
            .partition_point(|symbol| symbol.start <= address);
        self.symbols[..before]
            .last()
            .filter(|s| address < s.end)
            .map(|s| s.name.as_str())
    }

    pub(crate) fn frames(&self, address: u64) -> Result<Vec<Symbolized>> {
        let physical = self.physical_name(address);
        if let Some(dwarf) = &self.dwarf {
            let unit = dwarf.find_dwarf_and_unit(address).skip_all_loads();
            let hirsel_unit = unit.map(is_hirsel_unit).transpose()?.unwrap_or(false);
            let mut rows = dwarf.find_frames(address).skip_all_loads()?;
            let mut frames = Vec::new();
            while let Some(frame) = rows.next()? {
                let visible = frame
                    .function
                    .as_ref()
                    .map(|func| func.demangle())
                    .transpose()?;
                let raw = frame
                    .function
                    .as_ref()
                    .map(|func| func.raw_name())
                    .transpose()?;
                let def_name = match (unit, frame.dw_die_offset) {
                    (Some(unit), Some(offset)) => definition_name(unit, offset)?,
                    _ => None,
                };
                let loc = frame.location;
                let function = raw.map(|raw| {
                    let name = def_name.as_deref().or(visible.as_deref()).unwrap_or(&raw);
                    function(&raw, name, def_name.as_deref(), hirsel_unit)
                });
                frames.push(Symbolized {
                    function,
                    file: loc.as_ref().and_then(|loc| loc.file.map(str::to_owned)),
                    line: loc.as_ref().and_then(|loc| loc.line),
                    col: loc.as_ref().and_then(|loc| loc.column),
                    inlined: true,
                });
            }
            if let Some(last) = frames.last_mut() {
                last.inlined = false;
                last.function = match (physical, last.function.take()) {
                    (Some(raw), Some(mut resolved)) => {
                        resolved.instance = raw.to_owned();
                        Some(resolved)
                    }
                    (Some(raw), None) => Some(physical_function(raw)),
                    (None, resolved) => resolved,
                };
                return Ok(frames);
            }
        }
        Ok(vec![Symbolized {
            function: physical.map(physical_function),
            ..Symbolized::default()
        }])
    }
}

fn physical_function(raw: &str) -> Function {
    let name = addr2line::demangle_auto(Cow::Borrowed(raw), None);
    function(raw, &name, None, false)
}

fn function(raw: &str, name: &str, definition: Option<&str>, hirsel_unit: bool) -> Function {
    let is_hirsel = hirsel_unit
        || raw
            .strip_prefix('h')
            .and_then(|tail| tail.as_bytes().get(..64))
            .is_some_and(|digest| digest.iter().all(u8::is_ascii_hexdigit));
    let kind = if is_hirsel {
        "hirsel"
    } else if raw.starts_with("hirsel_")
        || name.contains("hirsel_core_rt::")
        || name.contains("hirsel_stack::")
    {
        "runtime"
    } else {
        "foreign"
    };
    Function {
        def_path: (kind == "hirsel")
            .then(|| definition.map(str::to_owned))
            .flatten(),
        instance: raw.to_owned(),
        name: name.to_owned(),
        kind,
    }
}

fn is_hirsel_unit(unit: UnitRef<'_, Reader>) -> Result<bool> {
    use gimli::Reader as _;
    let mut entries = unit.entries();
    let Some(root) = entries.next_dfs()? else {
        return Ok(false);
    };
    let Some(producer) = root.attr_value(gimli::DW_AT_producer) else {
        return Ok(false);
    };
    Ok(unit.attr_string(producer)?.to_string_lossy()? == "Hirsel")
}

fn definition_name(unit: UnitRef<'_, Reader>, offset: UnitOffset<usize>) -> Result<Option<String>> {
    let mut offset = offset;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(offset) {
            return Err("DWARF definition origin contains a cycle".into());
        }
        let entry = unit.entry(offset)?;
        if let Some(name) = entry.attr_value(gimli::DW_AT_name) {
            use gimli::Reader as _;
            return Ok(Some(
                unit.attr_string(name)?.to_string_lossy()?.into_owned(),
            ));
        }
        match entry
            .attr_value(gimli::DW_AT_abstract_origin)
            .or_else(|| entry.attr_value(gimli::DW_AT_specification))
        {
            Some(AttributeValue::UnitRef(next)) => offset = next,
            _ => return Ok(None),
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut result, "{byte:02x}").expect("writing to String");
    }
    result
}

pub(crate) fn host_fiber_invocation(name: &str) -> bool {
    let name = addr2line::demangle_auto(Cow::Borrowed(name), None);
    name.starts_with("<hirsel_core_rt::fiber::Stack>::call::invoke")
        || name.starts_with("hirsel_core_rt::fiber::Stack::call::invoke")
}

pub(crate) fn fiber_boundary(name: &str) -> bool {
    let name = addr2line::demangle_auto(Cow::Borrowed(name), None);
    name.contains("hirsel_stack::fiber_start") || name.contains("hirsel_stack::switch")
}

pub(crate) fn source_path(root: &Path, name: &str) -> std::path::PathBuf {
    let path = Path::new(name);
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}
