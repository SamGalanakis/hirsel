use hirsel_core_ids::ContentHash;
use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
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
    note(object, ".note.hirsel.source", OWNER, 2, &source.0);
}

fn deliver(path: &Path, image: &[u8], debug: &[u8]) -> io::Result<()> {
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
    crate::store::write_bounded(&mut staged, &[debug])?;
    staged.persist(target).map_err(|e| e.error)?;
    Ok(())
}

pub(crate) struct NativeImage {
    pub(crate) bytes: Arc<Vec<u8>>,
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
        Ok(Some(Self {
            bytes: Arc::new(bytes),
            debug,
        }))
    }

    pub(crate) fn publish(&self, store: &crate::Store, key: ContentHash) -> io::Result<()> {
        {
            let _phase = crate::meter::trace::span("DWARF cache publication");
            store.put_keyed(Self::debug_key(key), &self.debug)?;
        }
        let _phase = crate::meter::trace::span("executable cache publication");
        store.put_keyed_executable(key, &self.bytes)
    }

    pub(crate) fn deliver(&self, path: &Path) -> io::Result<()> {
        let _phase = crate::meter::trace::span("output delivery");
        deliver(path, &self.bytes, &self.debug)?;
        crate::executable::deliver(path, &self.bytes)
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
