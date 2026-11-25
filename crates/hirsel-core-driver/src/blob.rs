//! Blobs: content named by its SHA-256 in `o/`, checked on every read by
//! the xxh3-64 that the record naming it carries.
use super::{Bare, Store, checksum, collect, decode_bare, encode_bare, write_bounded, wrote};
use hirsel_core_ids::ContentHash;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

impl Bare {
    /// The name and read check of `bytes` as a blob: SHA-256 names them,
    /// xxh3-64 checks them on every read.
    pub(crate) fn of(bytes: &[u8]) -> Self {
        hirsel_core_front::cost_scope!("non-check", "archive write and digest");
        Self {
            address: ContentHash(Sha256::digest(bytes).into()),
            check: checksum(bytes),
        }
    }
}

impl Store {
    /// Publish `bytes` as a blob named by their SHA-256. The file is passed
    /// directly to native linkers by path, so it carries no frame: its
    /// checksum travels in the record that names it, and a file at the name
    /// whose bytes are not the record's fails the read check, a miss.
    /// The bytes always replace the file by a rename: a file already at the
    /// path may be one whose check just failed, and an existing name says
    /// nothing about its bytes.
    pub(crate) fn put_blob(&self, bytes: &[u8]) -> io::Result<Bare> {
        let bare = Bare::of(bytes);
        self.write_blob(bare, bytes)?;
        Ok(bare)
    }
    /// Publish `bytes`, whose name and check are `bare` (`Bare::of`), as a
    /// blob; returns the published file, open for reading, which holds
    /// exactly those bytes whatever later replaces or evicts the name.
    pub(crate) fn write_blob(&self, bare: Bare, bytes: &[u8]) -> io::Result<File> {
        let file = self.publish_to(|file| {
            write_bounded(file, &[bytes])?;
            Ok((self.blob_path(bare.address), file.try_clone()?))
        })?;
        if let Some(cas) = &self.cas {
            cas.wrote(bytes.len());
        }
        wrote("bare");
        Ok(file)
    }
    /// Publish `bytes` as a blob and a record under `key` naming it.
    pub(crate) fn put_keyed_blob(&self, key: ContentHash, bytes: &[u8]) -> io::Result<Bare> {
        let bare = self.put_blob(bytes)?;
        self.put_keyed(key, &encode_bare(bare))?;
        Ok(bare)
    }
    /// Publish a record under `key` naming `bares`, blobs already written,
    /// in order.
    pub(crate) fn put_keyed_bares(&self, key: ContentHash, bares: &[Bare]) -> io::Result<()> {
        let payload: Vec<u8> = bares.iter().flat_map(|bare| encode_bare(*bare)).collect();
        self.put_keyed(key, &payload)
    }
    /// The blob the record under `key` names, with its bytes, when both
    /// check.
    pub(crate) fn get_keyed_blob(&self, key: ContentHash) -> io::Result<Option<(Bare, Vec<u8>)>> {
        let Some(bare) = self.get(key)?.as_deref().and_then(decode_bare) else {
            return Ok(None);
        };
        Ok(self.get_blob(bare)?.map(|bytes| (bare, bytes)))
    }
    /// The `N` blobs the record under `key` names, each read whole with the
    /// file it was read from, when the record and every blob check.
    pub(crate) fn get_keyed_blobs<const N: usize>(
        &self,
        key: ContentHash,
    ) -> io::Result<Option<[(Vec<u8>, File); N]>> {
        let Some(record) = self.get(key)? else {
            return Ok(None);
        };
        if record.len() != N * 40 {
            return Ok(None);
        }
        let mut blobs = Vec::with_capacity(N);
        for named in record.chunks_exact(40) {
            let bare = decode_bare(named).expect("a 40-byte name");
            match self.open_blob(bare)? {
                Some(blob) => blobs.push(blob),
                None => return Ok(None),
            }
        }
        Ok(blobs.try_into().ok())
    }
    /// A blob's bytes when they pass their check: a blob damaged, or
    /// swapped for another's, is a miss.
    pub(crate) fn get_blob(&self, bare: Bare) -> io::Result<Option<Vec<u8>>> {
        Ok(self.open_blob(bare)?.map(|(bytes, _)| bytes))
    }
    /// A blob's bytes when they pass their check, with the file they were
    /// read from: whatever later replaces or evicts the name, that file
    /// holds the checked bytes.
    pub(crate) fn open_blob(&self, bare: Bare) -> io::Result<Option<(Vec<u8>, File)>> {
        for (path, own) in self.layers(self.blob_path(bare.address)) {
            if let Some(blob) = self.open_blob_at(&path, own, bare)? {
                return Ok(Some(blob));
            }
        }
        Ok(None)
    }
    fn open_blob_at(
        &self,
        path: &Path,
        own: bool,
        bare: Bare,
    ) -> io::Result<Option<(Vec<u8>, File)>> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        crate::meter::count(crate::meter::Count::FilesOpened, 1);
        crate::meter::count(crate::meter::Count::ObjectsOpened, 1);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if checksum(&bytes) != bare.check {
            return Ok(None);
        }
        if own && self.cas.is_some() {
            collect::touch(&file);
        }
        Ok(Some((bytes, file)))
    }
}
