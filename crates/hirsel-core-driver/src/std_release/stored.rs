//! How a release std side is kept in its store: its record, its parts'
//! bitcode and objects and its lent bodies, each under a name its key
//! derives, and the compressed form a prebuilt side ships in.
use super::*;

/// The installed store keeps the original byte length and one zlib frame.
pub(super) fn write_compressed(store: &Store, key: ContentHash, bytes: &[u8]) -> io::Result<()> {
    let compressed = hirsel_llvm_lazy::compress_std_record(bytes).map_err(io::Error::other)?;
    let mut payload = Vec::with_capacity(8 + compressed.len());
    payload.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    payload.extend_from_slice(&compressed);
    store.put_keyed_blob(key, &payload).map(drop)
}

pub(super) fn read_compressed(
    store: &Store,
    key: ContentHash,
) -> io::Result<Option<(Bare, Vec<u8>)>> {
    let Some((blob, payload)) = store.get_keyed_blob(key)? else {
        return Ok(None);
    };
    let (header, bytes) = payload
        .split_first_chunk::<8>()
        .ok_or_else(|| io::Error::other("a compressed release std record has no length"))?;
    let size = usize::try_from(u64::from_le_bytes(*header))
        .ok()
        .and_then(std::num::NonZeroUsize::new)
        .ok_or_else(|| io::Error::other("invalid release std record length"))?;
    let decoded = hirsel_llvm_lazy::decompress_std_record(bytes, size).map_err(io::Error::other)?;
    if decoded.len() != size.get() {
        return Err(io::Error::other("invalid release std record length"));
    }
    Ok(Some((blob, decoded)))
}

/// A linker input only for a prebuilt part the program actually reaches,
/// named by its compressed blob and checked as the bytes it links.
pub(super) fn read_prebuilt_object(
    store: &Store,
    key: ContentHash,
) -> io::Result<Option<(Bare, NativeObject)>> {
    let Some((blob, payload)) = read_compressed(store, key)? else {
        return Ok(None);
    };
    let native: NativeObject =
        decode(&payload).ok_or_else(|| io::Error::other("invalid prebuilt release std object"))?;
    let check = crate::store::checksum(&native.bytes);
    Ok(Some((Bare { check, ..blob }, native)))
}

/// The record stored under `key` when it decodes, is of `kin` and its own
/// content derives `key`, and names its parts and lent bodies by the names
/// `key` derives; anything else is a miss.
pub(super) fn load_record(
    store: &Store,
    key: ContentHash,
    kin: ContentHash,
) -> io::Result<Option<StdRecord>> {
    let Some(record) = store
        .get(key)?
        .and_then(|bytes| decode::<StdRecord>(&bytes))
    else {
        return Ok(None);
    };
    let sound = record.schema == STD_SCHEMA
        && !record.parts.is_empty()
        && record
            .parts
            .iter()
            .all(|part| part.module < record.exports.len())
        && record.lent == lent_name(key)?
        && side_key(kin, record.prebuilt, &record.requested, &record.units)? == key;
    Ok(sound.then_some(record))
}

/// Whether the side under `key` still holds what a link of it may read:
/// its lent bodies and every part's bitcode, from which a part whose object
/// is missing is generated. A side missing any is a miss and is lowered
/// again, so a cache that lost a file never fails a build (2834). A prebuilt
/// side ships in its image, where nothing collects it, and is taken whole.
/// A blob present but damaged is still a stop when it is read.
pub(super) fn whole(store: &Store, key: ContentHash, record: &StdRecord) -> io::Result<bool> {
    if record.prebuilt {
        return Ok(true);
    }
    if !store.has_keyed_blob(record.lent)? {
        return Ok(false);
    }
    for i in 0..record.parts.len() {
        if !store.has_keyed_blob(code_name(key, i)?)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn part_name(key: ContentHash, i: usize) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-part.v1", &(key, i as u64))
}

pub(super) fn code_name(key: ContentHash, i: usize) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-code.v1", &(key, i as u64))
}

pub(super) fn lent_name(key: ContentHash) -> io::Result<ContentHash> {
    crate::native::key("hirsel.core-std-lent.v1", &key)
}
