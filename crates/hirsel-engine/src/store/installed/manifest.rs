//! Read the existing bincode fixed-int format. The structured prefix uses
//! bincode; the two fixed-width tables use checked byte slices instead of one
//! Serde call per digest byte. Publication still uses the original serializer.
//!
//! Layout: bincode 1.3.3 src/de/mod.rs:293-323 (tuples), 344-350 (sequences),
//! 402-411 (structs), and src/config/int.rs (FixintEncoding). Only the fixed
//! tables specialize those loops; the structured prefix keeps its decoder.
use super::*;
use bincode::Options;

#[derive(Deserialize)]
struct Header {
    compiler: ContentHash,
    package: PackageId,
    manifest: Vec<u8>,
    sources: Vec<InstalledSource>,
    dependencies: Vec<PackageId>,
    definitions: BTreeMap<DefPath, InstalledDefinition>,
}

fn invalid() -> Box<bincode::ErrorKind> {
    Box::new(bincode::ErrorKind::Custom(
        "invalid installed product tables".into(),
    ))
}

fn table<'a>(bytes: &mut &'a [u8], width: usize) -> Option<&'a [u8]> {
    let (count, rest) = bytes.split_at_checked(8)?;
    let count = usize::try_from(u64::from_le_bytes(count.try_into().ok()?)).ok()?;
    let (table, rest) = rest.split_at_checked(count.checked_mul(width)?)?;
    *bytes = rest;
    Some(table)
}

fn reference(bytes: &[u8]) -> Option<Reference> {
    Some(Reference {
        output: hirsel_artifact_store::Output(ContentHash(bytes.get(..32)?.try_into().ok()?)),
        domain: Domain {
            kind: u32::from_le_bytes(bytes.get(32..36)?.try_into().ok()?),
            schema: u32::from_le_bytes(bytes.get(36..40)?.try_into().ok()?),
        },
        len: u64::from_le_bytes(bytes.get(40..48)?.try_into().ok()?),
    })
}

pub(crate) fn decode(bytes: &[u8]) -> bincode::Result<InstalledPackage> {
    if bytes.len() > MAX_RECORD {
        return Err(invalid());
    }
    let mut cursor = std::io::Cursor::new(bytes);
    let header: Header = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_RECORD as u64)
        .deserialize_from(&mut cursor)?;
    let mut remaining = bytes
        .get(usize::try_from(cursor.position()).map_err(|_| invalid())?..)
        .ok_or_else(invalid)?;
    let rows = table(&mut remaining, 112)
        .ok_or_else(invalid)?
        .chunks_exact(112)
        .map(|row| {
            Some((
                ContentHash(row.get(..32)?.try_into().ok()?),
                ContentHash(row.get(32..64)?.try_into().ok()?),
                reference(row.get(64..)?)?,
            ))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(invalid)?;
    let contents = table(&mut remaining, 48)
        .ok_or_else(invalid)?
        .chunks_exact(48)
        .map(reference)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(invalid)?;
    if !remaining.is_empty() {
        return Err(invalid());
    }
    Ok(InstalledPackage {
        compiler: header.compiler,
        identity: ContentHash::default(),
        package: header.package,
        manifest: header.manifest,
        sources: header.sources,
        dependencies: header.dependencies,
        definitions: header.definitions,
        rows,
        contents,
        records: Default::default(),
        sources_owner: None,
    })
}
