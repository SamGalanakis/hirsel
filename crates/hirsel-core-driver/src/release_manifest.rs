//! The Release value around the runner's preview response.

use crate::{
    native::{recipe_build::display_grants, selection},
    sdk::{Describe, Reader},
};
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::release_recipe::Recipe;
use std::{path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

fn bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), Error> {
    out.extend_from_slice(&u32::try_from(value.len())?.to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn hash(value: Vec<u8>) -> Result<ContentHash, Error> {
    Ok(ContentHash(
        value
            .try_into()
            .map_err(|_| "manifest hash must be 32 bytes")?,
    ))
}

pub(crate) fn write(
    path: &Path,
    executable: &Path,
    root: ContentHash,
    recipe: &Recipe,
    timeout: Duration,
) -> Result<(), Error> {
    // The value codec uses a fixed little-endian u32 list count.
    let response = match crate::describe::preview_child(
        executable,
        recipe.launch.n_place,
        timeout,
        &0u32.to_le_bytes(),
    ) {
        Ok(bytes) => bytes,
        Err(crate::describe::DescribeError::PlaceError(bytes)) => {
            return Err(crate::sdk::place_error(&bytes)?.into());
        }
        Err(error) => return Err(error.into()),
    };
    let mut wire = Reader {
        bytes: &response,
        at: 0,
    };
    Describe::read_from(&mut wire)?;
    hash(wire.bytes()?)?;
    let release_end = wire.at;
    if wire.count()? != 0 {
        return Err("preview returned answers for an empty old list".into());
    }
    wire.done()?;

    let grants = display_grants(recipe);
    let mut manifest = response[..release_end].to_vec();
    manifest.extend_from_slice(&u32::try_from(grants.len())?.to_le_bytes());
    for grant in grants {
        bytes(&mut manifest, grant.as_bytes())?;
    }
    manifest.extend_from_slice(&recipe.launch.root_limit.to_le_bytes());
    bytes(&mut manifest, &root.0)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    crate::executable::write(path, &manifest, 0o644)?;
    Ok(())
}

pub(crate) fn show(path: &Path) -> Result<(), Error> {
    let bytes = std::fs::read(path)?;
    let mut wire = Reader {
        bytes: &bytes,
        at: 0,
    };
    let description = Describe::read_from(&mut wire)?;
    let release = hash(wire.bytes()?)?;
    let mut grants = Vec::new();
    for _ in 0..wire.count()? {
        grants.push(wire.name()?);
    }
    if !grants.windows(2).all(|p| p[0] <= p[1]) {
        return Err("manifest grants are out of order".into());
    }
    let limit = wire.word()?;
    let recipe = hash(wire.bytes()?)?;
    wire.done()?;

    println!("recipe {}", selection(recipe));
    println!("release {}", selection(release));
    if description.digest.is_empty() {
        println!("digest unavailable");
    } else {
        println!("digest {}", selection(hash(description.digest)?));
    }
    println!("target {}", description.target);
    println!("n_place {}", description.n_place);
    println!("root_limit {limit}");
    for grant in grants {
        println!("grant {grant}");
    }
    for entry in description.exported {
        let identity = entry
            .identity
            .strip_prefix(&[1])
            .ok_or("manifest entry identity version")?;
        let name = std::str::from_utf8(identity)?;
        let revision = if entry.revision.is_empty() {
            "unavailable".to_owned()
        } else {
            selection(hash(entry.revision)?)
        };
        println!(
            "entry {} {name} {:x} {:x} {revision}",
            entry.kind, entry.args_fp, entry.result_fp
        );
    }
    Ok(())
}
