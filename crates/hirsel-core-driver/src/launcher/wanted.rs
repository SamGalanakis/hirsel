//! The launcher session's `List[Wanted]` codec, shared with the operator socket.
use super::{Reader, Writer, malformed};
use crate::code_admission::ObjectKey;
use crate::code_admission::ObjectKind;
use hirsel_core_ids::ContentHash;
use std::io;

/// A `List[Wanted]`: each key's kind as a tag, then its 32-byte hash as Bytes.
pub(crate) fn put_wanted(w: &mut Writer, keys: &[ObjectKey]) {
    w.len(keys.len());
    for key in keys {
        w.u32(match key.kind {
            ObjectKind::Group => 0,
            ObjectKind::Body => 1,
            ObjectKind::Annex => 2,
            ObjectKind::ForeignArchive => 3,
            ObjectKind::Recipe => unreachable!("a recipe is never needed"),
        });
        w.bytes(&key.hash.0);
    }
}

pub(crate) fn get_wanted(r: &mut Reader<'_>) -> io::Result<Vec<ObjectKey>> {
    let n = r.count(8)?;
    (0..n)
        .map(|_| {
            let kind = match r.u32()? {
                0 => ObjectKind::Group,
                1 => ObjectKind::Body,
                2 => ObjectKind::Annex,
                3 => ObjectKind::ForeignArchive,
                _ => return Err(malformed("needed object tag")),
            };
            let hash =
                <[u8; 32]>::try_from(r.bytes()?).map_err(|_| malformed("needed object hash"))?;
            Ok(ObjectKey {
                kind,
                hash: ContentHash(hash),
            })
        })
        .collect()
}
