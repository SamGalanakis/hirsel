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
//! its objects, as a raw object (the link reads its checked bytes) selected by
//! a keyed frame; a unit whose bytes fail their check, or a frame under
//! another key, is a miss and is prelinked again.
use crate::native::NativeStop;
use crate::store::Bare;
use crate::{Store, decode, encode};
use hirsel_core_ids::ContentHash;
use serde::{Deserialize, Serialize};
use std::io;

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

/// The unit stored under `key`, with its checked bytes.
pub(crate) fn load(store: &Store, key: ContentHash) -> io::Result<Option<(Bare, Vec<u8>)>> {
    let Some(bytes) = store.get(key)? else {
        return Ok(None);
    };
    let Some(root) = decode::<Root>(&bytes).filter(|r| r.schema == SCHEMA) else {
        return Ok(None);
    };
    Ok(store.get_bare(root.unit)?.map(|bytes| (root.unit, bytes)))
}

/// Prelink `objects`, the bundle of the image objects' checked bytes in link
/// order, with `linker`; store the unit under `key` and return its bytes.
pub(crate) fn prelink(
    store: &Store,
    key: ContentHash,
    linker: &std::path::Path,
    objects: &crate::native::link_input::Bundle,
) -> Result<(Bare, Vec<u8>), NativeStop> {
    let scratch = store.scratch()?;
    let output = scratch.path().join("unit.o");
    let mut process = crate::tool_inputs::tool_command(linker);
    process.args(FLAGS);
    process
        .args(["--push-state", "--whole-archive"])
        .arg(objects.path())
        .arg("--pop-state");
    process.arg("-o").arg(&output);
    crate::native::command(&mut process)?;
    let bytes = std::fs::read(&output)?;
    let unit = store.put_bare(crate::native_object::object_name(key), &bytes)?;
    store.put_keyed(
        key,
        &encode(&Root {
            schema: SCHEMA,
            unit,
        })?,
    )?;
    Ok((unit, bytes))
}
