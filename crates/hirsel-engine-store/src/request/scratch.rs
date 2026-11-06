//! Request-private std filesystem spooling. No durable index, cache lookup or
//! verifier capability is created. BTree/atomic admission stays in the parent.
use super::*;
#[cfg(not(unix))]
use std::io::{Read, Write};
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Entry {
    domain: Domain,
    len: u64,
    backing: Backing,
    read: Option<Box<CachedRead>>,
}
enum Backing {
    Spool(u64),
    Provider(Box<hirsel_artifact_store::HeldContent>),
}
struct CachedRead {
    bytes: hirsel_artifact_store::WeakBytes,
    admission: Arc<Admission>,
    charge: u64,
}
impl Drop for CachedRead {
    fn drop(&mut self) {
        self.admission.release(self.charge);
    }
}
impl Entry {
    fn matches(&self, reference: Reference) -> bool {
        self.domain == reference.domain && self.len == reference.len
    }
    fn reference(&self, output: ContentHash) -> Reference {
        Reference {
            output: hirsel_artifact_store::Output(output),
            domain: self.domain,
            len: self.len,
        }
    }
}
fn index_bytes(entries: usize) -> u64 {
    let mut weight = hirsel_query_products::retained::MetadataWeight::default();
    weight.tree::<ContentHash, Entry>(entries);
    weight.bytes()
}
struct Files {
    disk: u64,
    spool_bytes: u64,
    metadata: u64,
    entries: BTreeMap<ContentHash, Entry>,
    spool: fs::File,
}
pub(crate) struct ScratchStore {
    selected: Arc<dyn ResultStore>,
    // Close the spool before the last directory owner can remove its path.
    files: Mutex<Files>,
    admission: Arc<Admission>,
    root: Arc<Root>,
    disk_limit: u64,
}
struct ReadOwner {
    admission: Arc<Admission>,
    _root: Arc<Root>,
    bytes: u64,
}
impl Drop for ReadOwner {
    fn drop(&mut self) {
        self.admission.release(self.bytes);
    }
}
impl ScratchStore {
    pub(crate) fn new(
        selected: Arc<dyn ResultStore>,
        admission: Arc<Admission>,
        parent: &Path,
        disk_limit: u64,
    ) -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = parent.join(format!(
            ".hirsel-request-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        let root = Arc::new(Root(path));
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let spool = options.open(root.0.join("contents"))?;
        let metadata = ENTRY_CHARGE
            .saturating_add(root.0.capacity() as u64)
            .saturating_add(index_bytes(0));
        if !admission.charge(metadata) {
            return Err(std::io::Error::other(
                "scratch metadata exceeds admitted transport",
            ));
        }
        let _ = admission.scratch.set(root.clone());
        Ok(Self {
            selected,
            admission,
            root,
            disk_limit,
            files: Mutex::new(Files {
                disk: 0,
                spool_bytes: 0,
                metadata,
                entries: BTreeMap::new(),
                spool,
            }),
        })
    }
}
impl Drop for ScratchStore {
    fn drop(&mut self) {
        self.admission.release(
            self.files
                .get_mut()
                .unwrap_or_else(|e| e.into_inner())
                .metadata,
        );
    }
}
impl ResultStore for ScratchStore {
    fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        let bytes = bytes.checked_add(hirsel_engine_admission::retained_arc_bytes(
            &std::mem::MaybeUninit::<ArchiveLease>::uninit(),
        ))?;
        if !self.admission.charge(bytes) {
            return None;
        }
        Some(Arc::new(ArchiveLease {
            memory: self.admission.clone(),
            bytes,
        }))
    }

    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.selected.retention_scope()
    }

    fn capability(&self) -> ProviderCapability {
        ProviderCapability::RequestLocal(self.selected.persistence())
    }
    fn archive_encoded(&self, boundary: ArchiveBoundary) {
        self.selected.archive_encoded(boundary);
    }

    fn installation_output(&self) -> Option<Arc<dyn ResultStore>> {
        self.selected.installation_output()
    }

    fn installed_package(
        &self,
        package: &hirsel_syntax::PackageId,
        compiler: ContentHash,
    ) -> Result<Arc<InstalledPackage>, String> {
        self.selected.installed_package(package, compiler)
    }
    fn cache_product(&self, address: Address, action: Action, domain: Domain, bytes: &[u8]) {
        self.selected.cache_product(address, action, domain, bytes);
    }
    fn cache_query(&self, key: &EntryKey, reads: Reads, bytes: &[u8]) -> Option<Reference> {
        self.selected.cache_query(key, reads, bytes)
    }
    fn read_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.selected.read_scope()
    }
    fn name(&self) -> &'static str {
        self.selected.name()
    }
    fn local_product_store(&self) -> Option<hirsel_artifact_store::Store> {
        self.selected.local_product_store()
    }
    fn verifier(&self) -> Option<Arc<hirsel_validate::VerifierCache>> {
        self.selected.verifier()
    }
    fn get(&self, key: &EntryKey) -> Option<Bytes> {
        self.selected.get(key)
    }
    fn put(&self, key: &EntryKey, bytes: &[u8]) -> crate::WriteOutcome {
        self.selected.put(key, bytes)
    }
    fn demand(&self, keys: &[EntryKey]) {
        self.selected.demand(keys);
    }
    fn lookup_product(&self, address: Address) -> Option<Association> {
        self.selected.lookup_product(address)
    }
    fn associate_product(&self, address: Address, action: Action, reference: Reference) {
        self.selected.associate_product(address, action, reference);
    }
    fn associate_written_product(&self, address: Address, action: Action, reference: Reference) {
        self.selected.associate_product(address, action, reference);
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        let expected = domain.reference(bytes)?;
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = files.entries.get(&expected.output.0) {
            return entry.matches(expected).then_some(expected);
        }
        if expected.len > self.disk_limit.saturating_sub(files.disk) {
            return None;
        }
        // Charge the next record's index before retaining provider backing or
        // appending an independently checked range to the private spool.
        let mut metadata = index_bytes(files.entries.len().checked_add(1)?)
            .checked_sub(index_bytes(files.entries.len()))?;
        if !self.admission.charge(metadata) {
            return None;
        }
        // A private hard link retains the provider inode across eviction and
        // abort without keeping a descriptor open for every produced record.
        let held = if self.selected.put_content(domain, bytes) == Some(expected)
            && self.admission.try_charge(ENTRY_CHARGE)
        {
            let held = self
                .selected
                .local_product_store()
                .and_then(|store| {
                    store.hold_content(
                        expected,
                        self.root
                            .0
                            .join(format!("provider-{}", files.entries.len())),
                    )
                })
                .filter(|held| held.retained_bytes() <= self.disk_limit.saturating_sub(files.disk))
                .filter(|held| held.metadata_bytes() <= ENTRY_CHARGE);
            self.admission
                .release(ENTRY_CHARGE - held.as_ref().map_or(0, |held| held.metadata_bytes()));
            held
        } else {
            None
        };
        let backing = if let Some(held) = held {
            metadata += held.metadata_bytes();
            Backing::Provider(Box::new(held))
        } else {
            let offset = files.spool_bytes;
            if write_at(&files.spool, bytes, offset).is_err() {
                let _ = files.spool.set_len(offset);
                self.admission.release(metadata);
                return None;
            }
            files.spool_bytes += expected.len;
            Backing::Spool(offset)
        };
        files.disk += match &backing {
            Backing::Provider(held) => held.retained_bytes(),
            Backing::Spool(_) => expected.len,
        };
        files.metadata += metadata;
        files.entries.insert(
            expected.output.0,
            Entry {
                domain: expected.domain,
                len: expected.len,
                backing,
                read: None,
            },
        );
        Some(expected)
    }
    fn materialize(&self) -> bool {
        let Some(_retention) = self.selected.retention_scope() else {
            return false;
        };
        let references = self
            .files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .map(|(output, entry)| entry.reference(*output))
            .collect::<Vec<_>>();
        for reference in &references {
            let Some(bytes) = self.get_content(*reference) else {
                return false;
            };
            if self.selected.put_content(reference.domain, &bytes) != Some(*reference) {
                return false;
            }
        }
        self.selected.materialize()
            && references
                .into_iter()
                .all(|reference| self.selected.get_content(reference).is_some())
    }
    fn get_content(&self, reference: Reference) -> Option<Bytes> {
        self.read_product_content(reference).ok()
    }
    fn read_product_content(&self, reference: Reference) -> Result<Bytes, ProductArchiveError> {
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let Files {
            spool_bytes,
            spool,
            entries,
            ..
        } = &mut *files;
        let Some(entry) = entries.get_mut(&reference.output.0) else {
            drop(files);
            return self.selected.read_product_content(reference);
        };
        if !entry.matches(reference) {
            return Err(ProductArchiveError::InvalidContent);
        }
        if matches!(entry.backing, Backing::Spool(_))
            && spool
                .metadata()
                .map_err(ProductArchiveError::InvalidArchive)?
                .len()
                != *spool_bytes
        {
            return Err(ProductArchiveError::InvalidContent);
        }
        if let Some(bytes) = entry
            .read
            .as_ref()
            .and_then(|cached| cached.bytes.upgrade())
        {
            return Ok(bytes);
        }
        entry.read = None;
        let cache_charge = (size_of::<CachedRead>() as u64)
            .checked_add(hirsel_artifact_store::WeakBytes::metadata_bytes())
            .ok_or(ProductArchiveError::LayoutOverflow)?;
        let retained = reference
            .len
            .checked_add(ENTRY_CHARGE)
            .ok_or(ProductArchiveError::LayoutOverflow)?;
        self.admission.archive_charge(retained, "scratch read")?;
        if let Err(error) = self
            .admission
            .archive_charge(cache_charge, "scratch read metadata")
        {
            self.admission.release(retained);
            return Err(error);
        }
        let read = || -> Result<Bytes, ProductArchiveError> {
            match &entry.backing {
                Backing::Provider(held) => held.get().ok_or(ProductArchiveError::InvalidContent),
                Backing::Spool(offset) => {
                    let mut value = vec![
                        0;
                        usize::try_from(reference.len)
                            .map_err(|_| ProductArchiveError::LayoutOverflow)?
                    ];
                    read_at(spool, &mut value, *offset)
                        .map_err(ProductArchiveError::InvalidArchive)?;
                    Bytes::from(value)
                        .checked(reference)
                        .ok_or(ProductArchiveError::InvalidContent)
                }
            }
        };
        let value = match read() {
            Ok(value) => value,
            Err(error) => {
                self.admission.release(retained + cache_charge);
                return Err(error);
            }
        };
        let owner = Arc::new(ReadOwner {
            admission: self.admission.clone(),
            _root: self.root.clone(),
            bytes: retained,
        });
        let value = value.retain_owner(owner);
        entry.read = Some(Box::new(CachedRead {
            bytes: value.downgrade(),
            admission: self.admission.clone(),
            charge: cache_charge,
        }));
        Ok(value)
    }
}

// Rust std FileExt supplies retry/short-I/O handling without reopening or
// changing a shared cursor. Non-Unix hosts serialize their cursor under Files.
fn write_at(file: &fs::File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.write_all_at(bytes, offset)
    }
    #[cfg(not(unix))]
    {
        use std::io::{Seek, SeekFrom};
        let mut file = file;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)
    }
}
fn read_at(file: &fs::File, bytes: &mut [u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(bytes, offset)
    }
    #[cfg(not(unix))]
    {
        use std::io::{Seek, SeekFrom};
        let mut file = file;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain() -> Domain {
        Domain {
            kind: 0x4e00001f,
            schema: 1,
        }
    }
    #[test]
    fn one_payload_backing_fits_its_original_read_grant_and_refuses_a_second_live_read() {
        let parent = tempfile::tempdir().unwrap();
        let ram = 48 << 10;
        let budget = hirsel_engine_admission::ProcessBudget::new(ram);
        let request = RequestStore::with_scratch(
            Arc::new(NoneStore),
            budget.reserve(ram).unwrap(),
            parent.path(),
            64 << 10,
        )
        .unwrap();
        let reference = request.put_content(domain(), &vec![7; 32 << 10]).unwrap();
        let other = request.put_content(domain(), &vec![8; 16 << 10]).unwrap();
        let before = request.retained_bytes();
        let (held, actual) =
            crate::test_alloc::measured(|| request.get_content(reference).unwrap());
        assert_eq!(&*held, &vec![7; 32 << 10]);
        assert!(actual.live > 0);
        assert!(request.retained_bytes() - before >= actual.live as u64);
        assert!(request.retained_bytes() - before >= actual.peak as u64);
        assert!(request.retained_bytes() <= ram);
        let charged = request.retained_bytes();
        let (duplicate, allocations) =
            crate::test_alloc::measured(|| request.get_content(reference).unwrap());
        assert_eq!(duplicate.as_ptr(), held.as_ptr());
        assert_eq!(allocations.allocations, 0);
        assert_eq!(request.retained_bytes(), charged);
        drop(duplicate);
        let (refused, allocations) = crate::test_alloc::measured(|| request.get_content(other));
        assert!(refused.is_none());
        assert_eq!(allocations.allocations, 0);
        assert_eq!(request.retained_bytes(), charged);
        let child = held.range(3, 11, 1).unwrap();
        drop(held);
        drop(request);
        assert_eq!(&*child, &[7; 11]);
        assert_eq!(budget.reserved(), ram);
        drop(child);
        assert_eq!(budget.reserved(), 0);
    }
    #[test]
    fn compact_spool_index_admits_actual_backing_and_rejects_forged_metadata() {
        for count in [32u32, 64, 128] {
            let parent = tempfile::tempdir().unwrap();
            let ram = 256 << 10;
            let budget = hirsel_engine_admission::ProcessBudget::new(ram);
            let request = RequestStore::with_scratch(
                Arc::new(NoneStore),
                budget.reserve(ram).unwrap(),
                parent.path(),
                2 << 20,
            )
            .unwrap();
            let before = request.retained_bytes();
            let (reference, actual) = crate::test_alloc::measured(|| {
                let mut last = None;
                for number in 0..count {
                    last = request.put_content(domain(), &number.to_le_bytes());
                    assert!(last.is_some());
                }
                last.unwrap()
            });
            let index = request.retained_bytes() - before;
            assert!(actual.live > 0);
            assert!(
                index >= actual.live as u64,
                "admitted={index}, actual={}",
                actual.live
            );
            let mut old = hirsel_query_products::retained::MetadataWeight::default();
            old.tree::<ContentHash, (Reference, u64)>(count as usize);
            assert!(index < old.bytes(), "index={index}, old={}", old.bytes());
            for fault in [0, 1] {
                let mut forged = reference;
                if fault == 0 {
                    forged.len += 1;
                } else {
                    forged.domain.schema += 1;
                }
                let (refused, allocations) =
                    crate::test_alloc::measured(|| request.get_content(forged));
                assert!(refused.is_none());
                assert_eq!(allocations.allocations, 0);
                assert_eq!(request.retained_bytes(), before + index);
            }
            assert_eq!(
                &*request.get_content(reference).unwrap(),
                &(count - 1).to_le_bytes()
            );
            drop(request);
            assert_eq!(budget.reserved(), 0);
        }
    }
    #[test]
    fn scratch_transports_more_than_ram_and_last_child_owns_cleanup() {
        let parent = tempfile::tempdir().unwrap();
        let ram = 256 << 10;
        let budget = hirsel_engine_admission::ProcessBudget::new(ram);
        let request = RequestStore::with_scratch(
            Arc::new(NoneStore),
            budget.reserve(ram).unwrap(),
            parent.path(),
            2 << 20,
        )
        .unwrap();
        let directory = fs::read_dir(parent.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut references = Vec::new();
        for n in 0..20u8 {
            references.push(request.put_content(domain(), &vec![n; 32 << 10]).unwrap());
        }
        assert!(request.retained_bytes() < ram);
        assert!(request.local_product_store().is_none());
        assert!(request.verifier().is_none());
        let held = request
            .get_content(references[19])
            .unwrap()
            .range(7, 13, 1)
            .unwrap();
        assert_eq!(&*held, &[19; 13]);
        assert!(request.retained_bytes() <= ram);
        drop(request);
        assert!(directory.exists());
        assert_eq!(budget.reserved(), ram);
        assert_eq!(&*held, &[19; 13]);
        drop(held);
        assert!(!directory.exists());
        assert_eq!(budget.reserved(), 0);
    }
    #[test]
    fn scratch_many_small_records_use_their_index_layout_bound() {
        let parent = tempfile::tempdir().unwrap();
        let ram = 256 << 10;
        let budget = hirsel_engine_admission::ProcessBudget::new(ram);
        let request = RequestStore::with_scratch(
            Arc::new(NoneStore),
            budget.reserve(ram).unwrap(),
            parent.path(),
            2 << 20,
        )
        .unwrap();
        let mut last = None;
        for number in 0u32..400 {
            let mut value = vec![0; 256];
            value[..4].copy_from_slice(&number.to_le_bytes());
            last = Some(
                request
                    .put_content(domain(), &value)
                    .expect("bounded descriptor index"),
            );
        }
        let held = request.get_content(last.unwrap()).unwrap();
        assert_eq!(&held[..4], &399u32.to_le_bytes());
        assert!(request.retained_bytes() <= ram);
        assert!(budget.reserve(1).is_err());
        drop(request);
        assert_eq!(budget.reserved(), ram);
        drop(held);
        assert_eq!(budget.reserved(), 0);
    }
    #[test]
    fn scratch_live_reads_disk_and_corruption_never_expand_admission() {
        let parent = tempfile::tempdir().unwrap();
        let ram = 128 << 10;
        let budget = hirsel_engine_admission::ProcessBudget::new(ram);
        let request = RequestStore::with_scratch(
            Arc::new(NoneStore),
            budget.reserve(ram).unwrap(),
            parent.path(),
            64 << 10,
        )
        .unwrap();
        let first = request.put_content(domain(), &vec![1; 32 << 10]).unwrap();
        let second = request.put_content(domain(), &vec![2; 32 << 10]).unwrap();
        let directory = fs::read_dir(parent.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let disk: u64 = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert_eq!(disk, 64 << 10);
        // Files are untrusted bytes, never acceptance authority.
        let spool = fs::OpenOptions::new()
            .write(true)
            .open(directory.join("contents"))
            .unwrap();
        write_at(&spool, b"corrupted", 0).unwrap();
        let before = request.retained_bytes();
        assert!(request.get_content(first).is_none());
        assert!(matches!(
            request.copy_product_archive(first),
            Err(ProductArchiveError::InvalidContent)
        ));
        assert_eq!(request.retained_bytes(), before);
        let absent = domain().reference(b"missing").unwrap();
        assert!(matches!(
            request.copy_product_archive(absent),
            Err(ProductArchiveError::Unavailable)
        ));
        let lease_bytes = hirsel_engine_admission::retained_arc_bytes(&std::mem::MaybeUninit::<
            ArchiveLease,
        >::uninit());
        let held_admission = request
            .admit_product_memory(ram - before - lease_bytes)
            .unwrap();
        assert!(
            matches!(request.copy_product_archive(second), Err(ProductArchiveError::Admission {
            stage: "scratch read", requested, retained, limit,
        }) if requested == second.len + ENTRY_CHARGE && retained == ram && limit == ram)
        );
        assert_eq!(request.retained_bytes(), ram);
        drop(held_admission);
        assert_eq!(request.retained_bytes(), before);
        let held = request.get_content(second).unwrap();
        assert_eq!(&*held, &vec![2; 32 << 10]);
        spool.set_len((64 << 10) - 1).unwrap();
        assert!(
            request.get_content(second).is_none(),
            "truncated last record"
        );
        assert_eq!(
            &*held,
            &vec![2; 32 << 10],
            "held bytes survive spool damage"
        );
        assert!(request.retained_bytes() <= ram);
        assert!(request.put_content(domain(), &vec![3; 128 << 10]).is_none());
        assert!(request.retained_bytes() <= ram);
        assert!(budget.reserve(1).is_err());
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn retained_provider_records_do_not_retain_one_descriptor_per_output() {
        use std::os::unix::fs::MetadataExt;

        let parent = tempfile::tempdir().unwrap();
        let selected = Arc::new(DirStore::new(parent.path().join("provider")));
        let ram = 512 << 10;
        let budget = hirsel_engine_admission::ProcessBudget::new(ram);
        let request = RequestStore::with_scratch(
            selected.clone(),
            budget.reserve(ram).unwrap(),
            parent.path(),
            1 << 20,
        )
        .unwrap();
        let operation = request.operation_scope();
        let references: Vec<_> = (0u8..96)
            .map(|number| request.put_content(domain(), &vec![number; 5000]).unwrap())
            .collect();
        let store = request.local_product_store().unwrap();
        let provider_inodes: Vec<_> = references
            .iter()
            .map(|reference| {
                let metadata = fs::metadata(store.output_path(*reference)).unwrap();
                (metadata.dev(), metadata.ino())
            })
            .collect();
        let descriptors = fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|entry| fs::metadata(entry.ok()?.path()).ok())
            .filter(|metadata| provider_inodes.contains(&(metadata.dev(), metadata.ino())))
            .count();
        assert_eq!(
            descriptors, 0,
            "provider descriptors must close after retaining backing"
        );
        drop(operation);
        selected.local_product_store().unwrap().collect(0);
        for (number, reference) in references.into_iter().enumerate() {
            assert_eq!(
                &*request.get_content(reference).unwrap(),
                &vec![number as u8; 5000]
            );
        }
        drop(request);
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn scratch_preserves_selected_backend_capabilities_and_zero_refuses() {
        let parent = tempfile::tempdir().unwrap();
        let empty = hirsel_engine_admission::ProcessBudget::new(0);
        assert!(
            RequestStore::with_scratch(
                Arc::new(NoneStore),
                empty.reserve(0).unwrap(),
                parent.path(),
                1024
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
        #[cfg(unix)]
        {
            let selected = Arc::new(DirStore::new(parent.path().join("provider")));
            let ram = 128 << 10;
            let budget = hirsel_engine_admission::ProcessBudget::new(ram);
            let request = RequestStore::with_scratch(
                selected.clone(),
                budget.reserve(ram).unwrap(),
                parent.path(),
                64 << 10,
            )
            .unwrap();
            let operation = request.operation_scope();
            let payload = vec![42; 32 << 10];
            let reference = request.put_content(domain(), &payload).unwrap();
            let directory = fs::read_dir(parent.path())
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".hirsel-request-")
                })
                .unwrap();
            assert_eq!(
                fs::metadata(directory.join("contents")).unwrap().len(),
                0,
                "held provider backing must not be copied to the spool"
            );
            let held = request.get_content(reference).unwrap();
            let duplicate = request.get_content(reference).unwrap();
            assert_eq!(
                held.as_ptr(),
                duplicate.as_ptr(),
                "live readers share the checked range"
            );
            drop(duplicate);
            let file = fs::OpenOptions::new()
                .write(true)
                .open(
                    request
                        .local_product_store()
                        .unwrap()
                        .output_path(reference),
                )
                .unwrap();
            let last = file.metadata().unwrap().len() - 1;
            write_at(&file, &[0], last).unwrap();
            assert_eq!(
                &*request.get_content(reference).unwrap(),
                &payload,
                "a held immutable read is independent of later file damage"
            );
            drop(held);
            assert!(
                request.get_content(reference).is_none(),
                "an expired read must recheck the complete provider payload"
            );
            write_at(&file, &[42], last).unwrap();
            assert_eq!(&*request.get_content(reference).unwrap(), &payload);
            for fault in 0..4 {
                let mut forged = reference;
                match fault {
                    0 => forged.len -= 1,
                    1 => forged.domain.kind += 1,
                    2 => forged.domain.schema += 1,
                    _ => forged.output.0.0[0] ^= 1,
                }
                assert!(request.get_content(forged).is_none());
            }
            // The private link retains the original bytes after an unpublished
            // provider tail is aborted.
            drop(operation);
            selected.local_product_store().unwrap().collect(0);
            assert_eq!(&*request.get_content(reference).unwrap(), &payload);
            assert!(request.retained_bytes() <= ram);
            drop(request);
            assert_eq!(budget.reserved(), 0);
        }
        let selected = Arc::new(MemStore::default());
        let verifier = selected.verifier().unwrap();
        let budget = hirsel_engine_admission::ProcessBudget::new(8 << 10);
        let request = RequestStore::with_scratch(
            selected.clone(),
            budget.reserve(8 << 10).unwrap(),
            parent.path(),
            0,
        )
        .unwrap();
        assert!(Arc::ptr_eq(&request.verifier().unwrap(), &verifier));
        let before = request.retained_bytes();
        let reference = request.put_content(domain(), b"selected provider").unwrap();
        assert!(request.retained_bytes() > before);
        drop(selected.take());
        assert_eq!(
            &*request.get_content(reference).unwrap(),
            b"selected provider"
        );
        drop(request);
        let request = RequestStore::with_scratch(
            selected.clone(),
            budget.reserve(8 << 10).unwrap(),
            parent.path(),
            1024,
        )
        .unwrap();
        let reference = request.put_content(domain(), b"selected provider").unwrap();
        // Successful provider publication must not make an evictable cache
        // the only owner of a settled request's original bytes.
        assert_eq!(
            &*selected.get_content(reference).unwrap(),
            b"selected provider"
        );
        drop(selected.take());
        assert!(selected.get_content(reference).is_none());
        assert_eq!(
            &*request.get_content(reference).unwrap(),
            b"selected provider"
        );
        assert!(request.retained_bytes() <= 8 << 10);
        assert!(!request.exhausted());
    }
}
