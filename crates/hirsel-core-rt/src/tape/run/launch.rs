use super::*;

pub(super) fn write_launch(out: &mut Vec<u8>, launch: &Metadata, managed: bool, build: &[u8; 32]) {
    for bytes in [launch.digest(), launch.target().as_bytes()] {
        put_varint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
    }
    out.extend_from_slice(&launch.n_place().to_le_bytes());
    out.push(
        u8::from(launch.is_describe())
            | (u8::from(managed) << 1)
            | (u8::from(launch.is_wake()) << 2)
            | (u8::from(launch.n_place_explicit()) << 3)
            | (u8::from(launch.witness().is_some()) << 4),
    );
    if launch.is_wake() {
        out.extend_from_slice(&launch.idle_ms().to_le_bytes());
    }
    if let Some(version) = launch.witness() {
        out.extend_from_slice(&version.to_le_bytes());
    }
    out.extend_from_slice(build);
}

/// The recorded launch metadata, whether the run was managed, and the
/// recording build's digest.
pub(super) fn read_launch(
    arg: &[u8],
    revision: [u8; 32],
) -> Result<(Metadata, bool, [u8; 32]), Corrupt> {
    let mut c = Cursor::new(arg);
    for _ in 0..c.varint()? {
        c.bytes()?;
    }
    let digest = match c.bytes()? {
        [] => None,
        bytes => Some(bytes.try_into().map_err(|_| Corrupt)?),
    };
    let target = std::str::from_utf8(c.bytes()?).map_err(|_| Corrupt)?;
    let n_place = u64::from_le_bytes(c.fixed()?);
    let flags = c.u8()?;
    if !matches!(flags & !24, 0 | 1 | 2 | 4 | 6) || flags & 16 != 0 && flags & 4 == 0 {
        return Err(Corrupt);
    }
    let mut launch = Metadata::embedded(digest, target, n_place, flags & 1 != 0)
        .with_launch_identity(Some(revision), flags & 8 != 0);
    if flags & 4 != 0 {
        launch = launch.with_wake(u64::from_le_bytes(c.fixed()?));
    }
    if flags & 16 != 0 {
        launch = launch.with_witness(Some(u64::from_le_bytes(c.fixed()?)));
    }
    let build = c.fixed()?;
    if !c.done() {
        return Err(Corrupt);
    }
    Ok((launch, flags & 2 != 0, build))
}
