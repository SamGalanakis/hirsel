//! The launcher session's `List[Wanted]` and package-root codecs, shared with the operator
//! socket.
use super::{Reader, Writer, malformed};
use crate::code_admission::ObjectKey;
use crate::code_admission::ObjectKind;
use hirsel_core_ids::ContentHash;
use std::io;

/// A `List[Wanted]`: each key's kind as a tag, then its 32-byte hash as Bytes.
pub(crate) fn put_wanted(w: &mut Writer, keys: &[ObjectKey]) {
    w.len(keys.len());
    for key in keys {
        put_object_key(w, key);
    }
}

/// One object `Wanted`: its kind's tag, then its hash.
pub(crate) fn put_object_key(w: &mut Writer, key: &ObjectKey) {
    w.u32(match key.kind {
        ObjectKind::Group => 0,
        ObjectKind::Body => 1,
        ObjectKind::Annex => 2,
        ObjectKind::ForeignArchive => 3,
        ObjectKind::Value => 4,
        ObjectKind::Recipe => unreachable!("a recipe is never needed"),
    });
    w.bytes(&key.hash.0);
}

pub(crate) fn get_wanted(r: &mut Reader<'_>) -> io::Result<Vec<ObjectKey>> {
    let n = r.count(8)?;
    (0..n)
        .map(|_| {
            let tag = r.u32()?;
            get_object_key(tag, r)
        })
        .collect()
}

/// The rest of an object `Wanted` whose tag is read: its hash.
pub(crate) fn get_object_key(tag: u32, r: &mut Reader<'_>) -> io::Result<ObjectKey> {
    let kind = match tag {
        0 => ObjectKind::Group,
        1 => ObjectKind::Body,
        2 => ObjectKind::Annex,
        3 => ObjectKind::ForeignArchive,
        4 => ObjectKind::Value,
        _ => return Err(malformed("needed object tag")),
    };
    let hash = <[u8; 32]>::try_from(r.bytes()?).map_err(|_| malformed("needed object hash"))?;
    Ok(ObjectKey {
        kind,
        hash: ContentHash(hash),
    })
}

/// A `List[Bytes]` of 32-byte package roots (`Approvals.packages`, `Registration.packages`).
pub(crate) fn put_roots(w: &mut Writer, roots: &[[u8; 32]]) {
    w.len(roots.len());
    for root in roots {
        w.bytes(root);
    }
}

pub(crate) fn get_roots(r: &mut Reader<'_>) -> io::Result<Vec<[u8; 32]>> {
    (0..r.count(36)?)
        .map(|_| {
            <[u8; 32]>::try_from(r.bytes()?).map_err(|_| malformed("a package root is 32 bytes"))
        })
        .collect()
}
