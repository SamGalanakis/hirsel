use super::*;

impl Store {
    /// A body record's head and `ir_hash`, read under its address without
    /// its IR.
    ///
    /// A head read checks BODY_MAGIC, VERSION, both lengths, the exact file
    /// length, the trailer address and the prefix checksum over exactly the
    /// head bytes. It returns only fields the prefix covers, and nothing
    /// derived from IR. `get_body` alone checks the IR and decodes it.
    pub(crate) fn get_body_head(&self, key: ContentHash) -> io::Result<Option<Head>> {
        let image = crate::meter::ImageRead::start(self.image);
        if let Some(records) = &self.records {
            let Some(bytes) = records.read(&self.path(key), None)? else {
                return Ok(None);
            };
            let size = bytes.len() as u64;
            return Self::read_head(key, io::Cursor::new(bytes), size, None);
        }
        let file = match File::open(self.path(key)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        crate::meter::count(crate::meter::Count::FilesOpened, 1);
        if let Some(image) = &image {
            image.opened();
        }
        let size = file.metadata()?.len();
        Self::read_head(key, file, size, image.as_ref())
    }

    fn read_head(
        key: ContentHash,
        mut file: impl Read + Seek,
        size: u64,
        image: Option<&crate::meter::ImageRead>,
    ) -> io::Result<Option<Head>> {
        let mut header = [0u8; BODY_HEADER];
        if size < BODY_HEADER as u64 || file.read_exact(&mut header).is_err() {
            return Ok(None);
        }
        if &header[..8] != BODY_MAGIC || header[8..12] != VERSION.to_le_bytes() {
            return Ok(None);
        }
        let (head_len, ir_len) = (word(&header, 12), word(&header, 20));
        let Some(prefix) = (BODY_HEADER as u64)
            .checked_add(head_len)
            .and_then(|n| n.checked_add(32))
        else {
            return Ok(None);
        };
        if prefix
            .checked_add(ir_len)
            .and_then(|n| n.checked_add(BODY_TRAILER as u64))
            != Some(size)
        {
            return Ok(None);
        }
        let Ok(prefix) = usize::try_from(prefix) else {
            return Ok(None);
        };
        let mut trailer = [0u8; BODY_TRAILER];
        file.seek(SeekFrom::Start(size - BODY_TRAILER as u64))?;
        file.read_exact(&mut trailer)?;
        if trailer[16..] != key.0 {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(prefix).map_err(io::Error::other)?;
        bytes.extend_from_slice(&header);
        bytes.resize(prefix, 0);
        file.seek(SeekFrom::Start(BODY_HEADER as u64))?;
        file.read_exact(&mut bytes[BODY_HEADER..])?;
        if let Some(image) = &image {
            image.bytes(BODY_TRAILER + (prefix - BODY_HEADER));
        }
        let sum = || checksum(&bytes);
        let check = match &image {
            Some(image) => image.digest(prefix, sum),
            None => sum(),
        };
        if check != word(&trailer, 0) {
            return Ok(None);
        }
        let ir_hash = ContentHash(bytes[prefix - 32..].try_into().unwrap());
        bytes.truncate(prefix - 32);
        Ok(Some(Head {
            bytes,
            head: BODY_HEADER..prefix - 32,
            ir_hash,
        }))
    }
}
