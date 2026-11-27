use hirsel_core_ids::ContentHash;
use hirsel_core_ids::hex;
use hirsel_core_layout::ObjectFormat;
use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

const IMAGE_NOTE: &str = ".note.hirsel.image-digest";
const UUID_NOTE: &str = ".note.hirsel.image-uuid";
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
    /// A Mach-O release image's `LC_UUID`, which ld64 derives from its
    /// content as a GNU build ID is derived.
    Uuid([u8; 16]),
    ImageDigest(ContentHash),
}

/// The Hirsel object format of a native image or object.
pub(crate) fn format(format: object::BinaryFormat) -> io::Result<ObjectFormat> {
    match format {
        object::BinaryFormat::Elf => Ok(ObjectFormat::Elf),
        object::BinaryFormat::MachO => Ok(ObjectFormat::MachO),
        other => Err(io::Error::other(format!(
            "no native image format {other:?}"
        ))),
    }
}

fn build_id_path(bytes: &[u8]) -> PathBuf {
    let id = hex::encode(bytes);
    Path::new(".build-id")
        .join(&id[..2])
        .join(format!("{}.debug", &id[2..]))
}

impl<'a> Identity<'a> {
    pub(super) fn of(file: &object::File<'a>, image: &[u8]) -> io::Result<Self> {
        let recorded = match format(file.format())? {
            ObjectFormat::Elf => file
                .build_id()
                .map_err(io::Error::other)?
                .filter(|id| !id.is_empty())
                .map(Self::BuildId),
            ObjectFormat::MachO => file.mach_uuid().map_err(io::Error::other)?.map(Self::Uuid),
        };
        Ok(recorded.unwrap_or_else(|| Self::ImageDigest(ImageDigest::Detached.hash(image))))
    }

    /// Where the debug file of the image at `image` lives.
    fn path(&self, image: &Path) -> PathBuf {
        match self {
            Self::BuildId(bytes) => image.with_file_name(build_id_path(bytes)),
            Self::Uuid(_) => super::dsym::path(image),
            Self::ImageDigest(hash) => {
                image.with_file_name(Path::new(".hirsel-debug").join(format!("{hash}.debug")))
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
            Self::Uuid(uuid) => note(object, UUID_NOTE, OWNER, 3, uuid),
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
        let header = note_bytes(OWNER, 1, &[]).len();
        if let Some(section) = debug.section_by_name(UUID_NOTE) {
            let data = section.data().map_err(io::Error::other)?;
            return Ok(data
                .get(header..header + 16)
                .map(|uuid| Self::Uuid(uuid.try_into().unwrap())));
        }
        let Some(section) = debug.section_by_name(IMAGE_NOTE) else {
            return Ok(None);
        };
        let data = section.data().map_err(io::Error::other)?;
        Ok(data
            .get(header..header + 32)
            .map(|hash| Self::ImageDigest(ContentHash(hash.try_into().unwrap()))))
    }

    fn matches(&self, file: &object::File<'_>) -> bool {
        match self {
            Self::BuildId(id) => file.build_id().is_ok_and(|actual| actual == Some(*id)),
            Self::Uuid(uuid) => file.mach_uuid().is_ok_and(|actual| actual == Some(*uuid)),
            Self::ImageDigest(hash) => file
                .section_by_name(IMAGE_NOTE)
                .and_then(|section| section.data().ok())
                .is_some_and(|data| data == note_bytes(OWNER, 1, &hash.0)),
        }
    }
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
    match format(object.format()).expect("Hirsel writes ELF and Mach-O objects") {
        ObjectFormat::Elf => note(object, ".note.hirsel.source", OWNER, 2, &source.0),
        // Mach-O has no notes; ld64's LC_UUID digests the image, so the
        // identity is image bytes ld64 must keep without a reference.
        ObjectFormat::MachO => {
            let section = object.add_section(
                b"__TEXT".to_vec(),
                b"__hirsel_source".to_vec(),
                object::SectionKind::ReadOnlyData,
            );
            object.section_mut(section).flags = object::SectionFlags::MachO {
                flags: object::macho::S_REGULAR | object::macho::S_ATTR_NO_DEAD_STRIP,
                reserved2: 0,
            };
            let offset = object.append_section_data(section, &source.0, 1);
            // The resident linker locates changed input bytes by a symbol in
            // the linked image. The source digest changes even on a text edit.
            object.add_symbol(object::write::Symbol {
                name: b"hirsel_source_identity".to_vec(),
                value: offset,
                size: source.0.len() as u64,
                kind: object::SymbolKind::Data,
                scope: object::SymbolScope::Compilation,
                weak: false,
                section: object::write::SymbolSection::Section(section),
                flags: object::SymbolFlags::None,
            });
        }
    }
}

fn deliver(path: &Path, image: &[u8], debug: &[u8], source: Option<&File>) -> io::Result<()> {
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
    let target = identity.path(path);
    if let Identity::Uuid(_) = identity {
        return super::dsym::write(&target, image, debug);
    }
    let parent = target.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    crate::executable::fill(staged.as_file_mut(), debug, source)?;
    staged.persist(target).map_err(|e| e.error)?;
    Ok(())
}

/// A linked executable and its detached debug file: in a content store, two
/// blobs named, in that order, by the record under the link key.
pub(crate) struct NativeImage {
    pub(crate) bytes: Arc<Vec<u8>>,
    pub(crate) debug: Vec<u8>,
    /// The blobs the checked bytes were read from or written to, which
    /// delivery reflinks: whatever later replaces or evicts their names,
    /// these files hold those bytes. None before the image is stored.
    blobs: Option<[File; 2]>,
}

impl NativeImage {
    pub(crate) fn new(bytes: Arc<Vec<u8>>, debug: Vec<u8>) -> Self {
        Self {
            bytes,
            debug,
            blobs: None,
        }
    }

    /// The image under `key` when its record and both blobs check: a blob
    /// damaged, or swapped for another's, is a miss, and the build links
    /// again.
    pub(crate) fn load(store: &crate::Store, key: ContentHash) -> io::Result<Option<Self>> {
        let Some([(bytes, image), (debug, detached)]) = store.get_keyed_blobs(key)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            bytes: Arc::new(bytes),
            debug,
            blobs: Some([image, detached]),
        }))
    }

    pub(crate) fn publish(&mut self, store: &crate::Store, key: ContentHash) -> io::Result<()> {
        let _phase = crate::meter::trace::span("executable cache publication");
        let bares = {
            let _digest = crate::meter::trace::span("executable digest");
            [
                crate::store::Bare::of(&self.bytes),
                crate::store::Bare::of(&self.debug),
            ]
        };
        let image = store.write_blob(bares[0], &self.bytes)?;
        let detached = store.write_blob(bares[1], &self.debug)?;
        store.put_keyed_bares(key, &bares)?;
        self.blobs = Some([image, detached]);
        Ok(())
    }

    pub(crate) fn deliver(&self, path: &Path) -> io::Result<()> {
        let _phase = crate::meter::trace::span("output delivery");
        let [image, detached] = match &self.blobs {
            Some([image, detached]) => [Some(image), Some(detached)],
            None => [None, None],
        };
        deliver(path, &self.bytes, &self.debug, detached)?;
        crate::executable::deliver(path, &self.bytes, image)
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
        let mut paths = vec![identity.path(image_path)];
        if let Identity::BuildId(id) = identity {
            paths.push(Path::new("/usr/lib/debug").join(build_id_path(id)));
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
