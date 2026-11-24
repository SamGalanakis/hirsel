use super::{DeclaredCollection, Describe, Error, Exported, ExportedSlot, HttpBinding, Reader};
use sha2::{Digest, Sha256};

impl Describe {
    pub fn read(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader { bytes, at: 0 };
        let description = Self::read_from(&mut r)?;
        r.done()?;
        Ok(description)
    }

    pub(crate) fn read_from(r: &mut Reader<'_>) -> Result<Self, Error> {
        Self::read_projected(r, None)
    }

    pub(crate) fn read_with_identity(bytes: &[u8]) -> Result<(Self, [u8; 32]), Error> {
        let mut r = Reader { bytes, at: 0 };
        let mut hash = Sha256::new();
        hash.update(b"\x12hirsel.identity.v1");
        let description = Self::read_projected(&mut r, Some(&mut hash))?;
        r.done()?;
        if description.revision.len() != 32 || description.digest.len() != 32 {
            return Err("Unknown release or executable identity".into());
        }
        Ok((description, hash.finalize().into()))
    }

    fn read_projected(
        r: &mut Reader<'_>,
        mut identity: Option<&mut Sha256>,
    ) -> Result<Self, Error> {
        let digest = r.bytes()?;
        let target = r.name()?;
        let n_place = r.word()?;
        let revision_start = r.at;
        let revision = r.bytes()?;
        let exports_start = r.at;
        let n = r.count()?;
        let count = &r.bytes[exports_start..r.at];
        let mut slots_wire = Vec::new();
        if let Some(hash) = identity.as_mut() {
            hash.update(&r.bytes[revision_start..exports_start]);
            hash.update(n_place.to_le_bytes());
            hash.update(count);
        }
        let mut exported = Vec::new();
        for _ in 0..n {
            let export_start = r.at;
            let kind = r.count()? as u32;
            if kind > 4 {
                return Err("Malformed: entry kind".into());
            }
            let mut entry = Exported {
                kind,
                identity: r.bytes()?,
                args: r.bytes()?,
                result: r.bytes()?,
                args_fp: r.word()?,
                result_fp: r.word()?,
                revision: r.bytes()?,
                slots: Vec::new(),
                value: Vec::new(),
            };
            let slots_start = r.at;
            if let Some(hash) = identity.as_mut() {
                hash.update(&r.bytes[export_start..slots_start]);
            }
            for _ in 0..r.count()? {
                entry.slots.push(ExportedSlot {
                    name: r.name()?,
                    kind: r.count()? as u32,
                    key: r.bytes()?,
                    key_json: r.bytes()?,
                    value: r.bytes()?,
                });
            }
            if identity.is_some() {
                slots_wire.push(&r.bytes[slots_start..r.at]);
            }
            entry.value = r.bytes()?;
            if kind != 2 && kind != 4 && (!entry.slots.is_empty() || !entry.value.is_empty()) {
                return Err("Malformed: call entry carries query schemas".into());
            }
            exported.push(entry);
        }
        let mut tables = Vec::new();
        for _ in 0..r.count()? {
            tables.push(DeclaredCollection {
                name: r.name()?,
                id: r.count()? as u32,
                owner: r.count()? as u32,
                kind: r.byte()?,
            });
        }
        let bindings_start = r.at;
        let mut bindings = Vec::new();
        for _ in 0..r.count()? {
            bindings.push(HttpBinding::read(r)?);
        }
        if let Some(hash) = identity {
            hash.update(&r.bytes[bindings_start..r.at]);
            hash.update(count);
            for slots in slots_wire {
                hash.update(slots);
            }
        }
        Ok(Self {
            digest,
            target,
            n_place,
            revision,
            exported,
            tables,
            bindings,
        })
    }
}
