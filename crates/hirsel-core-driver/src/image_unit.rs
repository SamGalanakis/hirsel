//! The image unit: the image objects one link reaches, prelinked by the
//! link's own linker with `ld -r` into one relocatable object, which the link
//! reads in their place.
//!
//! The unit is a function of its key: the linker (the toolchain and the link
//! recipe), the target, the prelink flags and the image objects' identities in
//! link order. A build into an empty store prelinks the same objects into the
//! same unit, so a primed store and a fresh one link the same executable.
//! The unit is not claimed equal to a link of the separate objects: `ld -r`
//! renumbers local and section symbols. It lives in the image's store next to
//! its objects, as a raw object (the linker reads it by descriptor) selected by
//! a keyed frame; a unit whose bytes fail their check, or a frame under
//! another key, is a miss and is prelinked again.
use crate::native::NativeStop;
use crate::store::Bare;
use crate::{Store, decode, encode};
use hirsel_core_ids::ContentHash;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io;
use std::path::PathBuf;

const SCHEMA: u32 = 1;
/// The prelink's flags, part of the key.
const FLAGS: &[&str] = &["-r"];

#[derive(Serialize, Deserialize)]
struct Root {
    schema: u32,
    unit: Bare,
}

/// The unit's key.
pub(crate) fn key(
    toolchain: ContentHash,
    recipe: &crate::tool_inputs::LinkRecipe,
    target: &str,
    objects: &[ContentHash],
) -> io::Result<ContentHash> {
    crate::native::key(
        "hirsel.core-image-unit.v1",
        &(
            toolchain,
            recipe.identity,
            &recipe.linker,
            target,
            FLAGS,
            objects,
        ),
    )
}

/// The unit stored under `key`, with the descriptor its bytes were checked
/// through.
pub(crate) fn load(store: &Store, key: ContentHash) -> io::Result<Option<(Bare, File)>> {
    let Some(bytes) = store.get(key)? else {
        return Ok(None);
    };
    let Some(root) = decode::<Root>(&bytes).filter(|r| r.schema == SCHEMA) else {
        return Ok(None);
    };
    Ok(store
        .get_bare(root.unit)?
        .map(|(_, file)| (root.unit, file)))
}

/// Prelink `objects`, the checked descriptors of the image objects in link
/// order, with `linker`; store the unit under `key` and return the descriptor
/// its bytes were written through.
pub(crate) fn prelink(
    store: &Store,
    key: ContentHash,
    linker: &std::path::Path,
    objects: &[impl std::ops::Deref<Target = File>],
) -> Result<(Bare, File), NativeStop> {
    let scratch = store.scratch()?;
    let output = scratch.path().join("unit.o");
    let pid = std::process::id();
    let mut process = crate::tool_inputs::tool_command(linker);
    process.args(FLAGS);
    for object in objects {
        process.arg(PathBuf::from(format!(
            "/proc/{pid}/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(&**object)
        )));
    }
    process.arg("-o").arg(&output);
    crate::native::command(&mut process)?;
    let bytes = std::fs::read(&output)?;
    let (unit, file) = store.put_bare(crate::native_object::object_name(key), &bytes)?;
    store.put_keyed(
        key,
        &encode(&Root {
            schema: SCHEMA,
            unit,
        })?,
    )?;
    Ok((unit, file))
}
