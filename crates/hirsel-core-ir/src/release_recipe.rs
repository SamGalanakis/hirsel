//! Canonical release recipes, independent of the native toolchain.

use crate::Identity;
use crate::decode::{DecodeBudget, DecodeError, DecodeResult};
use crate::normal::{
    encode_identity, uleb,
    wire::{Read, Wire, invalid},
};
use hirsel_core_ids::{ContentHash, DefPath};

const MAGIC: &[u8] = b"HIRRECIP\0\x02\x00";
const NORMAL_FORM: u16 = crate::normal::VERSION;
pub const HASH_DOMAIN: &str = "hirsel.release-recipe.v2";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectRef {
    pub hash: ContentHash,
    pub len: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BodyRef {
    pub body: ObjectRef,
    pub annex: ObjectRef,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForeignTarget {
    pub target: String,
    pub archive: ObjectRef,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForeignFunction {
    pub identity: Identity,
    pub header: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForeignLeaf {
    /// The module's definition path, ending with its last module component.
    pub module: DefPath,
    pub symbols: String,
    pub targets: Vec<ForeignTarget>,
    pub functions: Vec<ForeignFunction>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Backend {
    Cranelift = 0,
    Llvm = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum GrantKind {
    Dir = 0,
    Net = 1,
    Clock = 2,
    Random = 3,
    Stdout = 4,
    Stderr = 5,
    Stdin = 6,
    Env = 7,
    Process = 8,
    Tasks = 9,
    Args = 10,
    Secret = 11,
    Foreign = 12,
    Exec = 13,
}

impl GrantKind {
    const KINDS: [Self; 14] = [
        Self::Dir,
        Self::Net,
        Self::Clock,
        Self::Random,
        Self::Stdout,
        Self::Stderr,
        Self::Stdin,
        Self::Env,
        Self::Process,
        Self::Tasks,
        Self::Args,
        Self::Secret,
        Self::Foreign,
        Self::Exec,
    ];

    pub fn from_tag(tag: u8) -> Option<Self> {
        Self::KINDS.get(usize::from(tag)).copied()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantBinding {
    pub ordinal: u32,
    pub kind: GrantKind,
    pub values: Vec<String>,
    /// For a Foreign binding, its interface's archive for the recipe target.
    pub archive: Option<ObjectRef>,
    /// For a configured Exec binding, the launch policy digest.
    pub policy: Option<ContentHash>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Launch {
    pub backend: Backend,
    pub target: String,
    pub root_limit: u64,
    pub n_place: u64,
    /// MAIN's Error payload is Exit, whose status the launcher returns.
    pub exit_status: bool,
    pub grants: Vec<GrantBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recipe {
    pub entry: Identity,
    pub address: ContentHash,
    /// In strictly increasing hash order.
    pub groups: Vec<ObjectRef>,
    /// In strictly increasing body hash order.
    pub bodies: Vec<BodyRef>,
    /// In strictly increasing order of the module identity encoding.
    pub foreign_modules: Vec<ForeignLeaf>,
    /// In strictly increasing order of raw SHA-256.
    pub foreign_archives: Vec<ObjectRef>,
    pub launch: Launch,
}

impl ObjectRef {
    fn encode(self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.hash.0);
        uleb(bytes, self.len);
    }

    fn decode(input: &mut Read<'_, '_>) -> DecodeResult<Self> {
        Ok(Self {
            hash: Wire::read(input)?,
            len: input.unsigned()?,
        })
    }
}

pub(crate) fn text(bytes: &mut Vec<u8>, value: &str) {
    uleb(bytes, value.len() as u64);
    bytes.extend_from_slice(value.as_bytes());
}

fn ordered<T: Ord + Copy>(
    previous: &mut Option<T>,
    key: T,
    message: &'static str,
) -> DecodeResult<()> {
    if previous.is_some_and(|prior| prior >= key) {
        return Err(invalid(message));
    }
    *previous = Some(key);
    Ok(())
}

impl ForeignLeaf {
    /// FFI-9 hashes these rows without their count or the selected target.
    pub fn encode_archive_rows(&self, bytes: &mut Vec<u8>) {
        for row in &self.targets {
            text(bytes, &row.target);
            row.archive.encode(bytes);
        }
    }

    fn encode(&self, bytes: &mut Vec<u8>) {
        text(bytes, &self.module.definition_identity());
        text(bytes, &self.symbols);
        uleb(bytes, self.targets.len() as u64);
        self.encode_archive_rows(bytes);
        uleb(bytes, self.functions.len() as u64);
        for function in &self.functions {
            bytes.extend_from_slice(&encode_identity(&function.identity));
            text(bytes, &function.header);
        }
    }

    fn decode<'a>(input: &mut Read<'a, '_>, prev: &mut Option<&'a [u8]>) -> DecodeResult<Self> {
        let start = input.remaining();
        let module: DefPath = Wire::read(input)?;
        let frame = &start[..start.len() - input.remaining().len()];
        ordered(prev, frame, "unordered foreign modules")?;

        let symbols: String = Wire::read(input)?;
        let mut prefix = symbols.bytes();
        if symbols.len() > 32
            || !prefix.next().is_some_and(|b| b.is_ascii_alphabetic())
            || !prefix.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(invalid("invalid foreign symbol prefix"));
        }

        let mut prev_target = None;
        let count = input.count()?;
        let targets = input.read_vec(count, |input| {
            let start = input.remaining();
            let target: String = Wire::read(input)?;
            if target.is_empty()
                || target.len() > 64
                || !target
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
            {
                return Err(invalid("invalid foreign target"));
            }
            // Target rows are ordered by the text's bytes, without the length.
            let text_end = start.len() - input.remaining().len();
            ordered(
                &mut prev_target,
                &start[text_end - target.len()..text_end],
                "unordered foreign targets",
            )?;
            let archive = ObjectRef::decode(input)?;
            Ok(ForeignTarget { target, archive })
        })?;

        let mut prev_function = None;
        let count = input.count()?;
        let functions = input.read_vec(count, |input| {
            let start = input.remaining();
            let identity: Identity = Wire::read(input)?;
            let frame = &start[..start.len() - input.remaining().len()];
            ordered(&mut prev_function, frame, "unordered foreign functions")?;
            let path = &identity.definition.0;
            if path.package != module.0.package
                || !path.modules.iter().eq(module
                    .0
                    .modules
                    .iter()
                    .chain(std::iter::once(&module.0.name)))
            {
                return Err(invalid("foreign function outside its module"));
            }
            let header = Wire::read(input)?;
            Ok(ForeignFunction { identity, header })
        })?;
        Ok(Self {
            module,
            symbols,
            targets,
            functions,
        })
    }
}

impl Recipe {
    pub fn has_foreign(&self) -> bool {
        !self.foreign_modules.is_empty()
            || !self.foreign_archives.is_empty()
            || self
                .launch
                .grants
                .iter()
                .any(|grant| grant.kind == GrantKind::Foreign)
    }

    /// Encode a producer's recipe. The producer supplies the object and grant
    /// lists in their canonical order.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&NORMAL_FORM.to_le_bytes());
        bytes.extend_from_slice(&encode_identity(&self.entry));
        bytes.extend_from_slice(&self.address.0);
        uleb(&mut bytes, self.groups.len() as u64);
        for group in &self.groups {
            group.encode(&mut bytes);
        }
        uleb(&mut bytes, self.bodies.len() as u64);
        for row in &self.bodies {
            row.body.encode(&mut bytes);
            row.annex.encode(&mut bytes);
        }
        uleb(&mut bytes, self.foreign_modules.len() as u64);
        for leaf in &self.foreign_modules {
            leaf.encode(&mut bytes);
        }
        uleb(&mut bytes, self.foreign_archives.len() as u64);
        for archive in &self.foreign_archives {
            archive.encode(&mut bytes);
        }
        let launch = &self.launch;
        bytes.push(launch.backend as u8);
        text(&mut bytes, &launch.target);
        bytes.extend_from_slice(&launch.root_limit.to_le_bytes());
        bytes.extend_from_slice(&launch.n_place.to_le_bytes());
        bytes.push(u8::from(launch.exit_status));
        uleb(&mut bytes, launch.grants.len() as u64);
        for grant in &launch.grants {
            uleb(&mut bytes, u64::from(grant.ordinal));
            bytes.push(grant.kind as u8);
            uleb(&mut bytes, grant.values.len() as u64);
            for value in &grant.values {
                text(&mut bytes, value);
            }
            if grant.kind == GrantKind::Foreign {
                grant
                    .archive
                    .expect("a Foreign grant selects an archive")
                    .encode(&mut bytes);
            }
            if grant.kind == GrantKind::Exec && !grant.values.is_empty() {
                bytes.extend_from_slice(&grant.policy.expect("an Exec grant pins its policy").0);
            }
        }
        bytes
    }

    pub fn hash(bytes: &[u8]) -> ContentHash {
        ContentHash::of_parts(HASH_DOMAIN, [bytes])
    }

    /// Decode the whole frame before exposing its ordered object lists.
    /// Annexes are decoded against their corresponding bodies at admission.
    pub fn decode(bytes: &[u8], budget: &mut DecodeBudget) -> DecodeResult<Self> {
        let mut input = Read::new(bytes, MAGIC, budget)?;
        if u16::read(&mut input)? != NORMAL_FORM {
            return Err(DecodeError::Unsupported(
                "recipe normal-form version. Republish from source",
            ));
        }
        Self::decode_fields(input)
    }

    /// Read the recipe envelope for a host-owned assembly record. This names
    /// an executable the host already assembled, whose runtime owns the body
    /// version. The host uses these fields to launch it and retain its objects.
    /// Admission and assembly of code must use `decode`.
    pub fn decode_retained(bytes: &[u8], budget: &mut DecodeBudget) -> DecodeResult<Self> {
        let mut input = Read::new(bytes, MAGIC, budget)?;
        u16::read(&mut input)?;
        Self::decode_fields(input)
    }

    fn decode_fields(mut input: Read<'_, '_>) -> DecodeResult<Self> {
        let entry = Wire::read(&mut input)?;
        let address = Wire::read(&mut input)?;
        let mut prev = None;
        let count = input.count()?;
        let groups = input.read_vec(count, |input| {
            let object = ObjectRef::decode(input)?;
            ordered(&mut prev, object.hash, "unordered group references")?;
            Ok(object)
        })?;
        let mut prev = None;
        let count = input.count()?;
        let bodies = input.read_vec(count, |input| {
            let body = ObjectRef::decode(input)?;
            ordered(&mut prev, body.hash, "unordered body references")?;
            let annex = ObjectRef::decode(input)?;
            Ok(BodyRef { body, annex })
        })?;
        let mut prev = None;
        let count = input.count()?;
        let foreign_modules =
            input.read_vec(count, |input| ForeignLeaf::decode(input, &mut prev))?;
        let mut prev = None;
        let count = input.count()?;
        let foreign_archives = input.read_vec(count, |input| {
            let object = ObjectRef::decode(input)?;
            ordered(&mut prev, object.hash, "unordered foreign archives")?;
            Ok(object)
        })?;
        let backend = match u8::read(&mut input)? {
            0 => Backend::Cranelift,
            1 => Backend::Llvm,
            _ => return Err(invalid("unknown recipe backend tag")),
        };
        let target = Wire::read(&mut input)?;
        let root_limit = Wire::read(&mut input)?;
        let n_place = Wire::read(&mut input)?;
        let exit_status = Wire::read(&mut input)?;
        let mut prev = None;
        let count = input.count()?;
        let grants = input.read_vec(count, |input| {
            let ordinal = input.index()?;
            ordered(&mut prev, ordinal, "unordered grant ordinals")?;
            let kind = GrantKind::from_tag(u8::read(input)?)
                .ok_or_else(|| invalid("unknown grant kind tag"))?;
            let values: Vec<String> = Wire::read(input)?;
            let archive = if kind == GrantKind::Foreign {
                let [interface] = values.as_slice() else {
                    return Err(invalid("a Foreign grant takes one interface"));
                };
                if interface.is_empty() {
                    return Err(invalid("a Foreign grant takes one interface"));
                }
                Some(ObjectRef::decode(input)?)
            } else {
                None
            };
            let policy = if kind == GrantKind::Exec && !values.is_empty() {
                let [file, names @ ..] = values.as_slice() else {
                    return Err(invalid("invalid Exec launch request"));
                };
                if file.is_empty()
                    || file.contains('\0')
                    || names.is_empty()
                    || names
                        .iter()
                        .any(|name| name.is_empty() || name.contains([' ', ',', '\0']))
                {
                    return Err(invalid("invalid Exec launch request"));
                }
                Some(ContentHash::read(input)?)
            } else {
                None
            };
            Ok(GrantBinding {
                ordinal,
                kind,
                values,
                archive,
                policy,
            })
        })?;
        input.end()?;
        Ok(Self {
            entry,
            address,
            groups,
            bodies,
            foreign_modules,
            foreign_archives,
            launch: Launch {
                backend,
                target,
                root_limit,
                n_place,
                exit_status,
                grants,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{DecodeLimit, DecodeLimits};
    use hirsel_core_ids::DefPath;

    #[test]
    fn release_recipe_canonical_frame() {
        let recipe = Recipe {
            entry: Identity::plain(DefPath::from_full("app@1/main/main").unwrap()),
            address: ContentHash([1; 32]),
            groups: vec![ObjectRef {
                hash: ContentHash([2; 32]),
                len: 300,
            }],
            bodies: vec![
                BodyRef {
                    body: ObjectRef {
                        hash: ContentHash([3; 32]),
                        len: 5,
                    },
                    annex: ObjectRef {
                        hash: ContentHash([4; 32]),
                        len: 6,
                    },
                },
                BodyRef {
                    body: ObjectRef {
                        hash: ContentHash([5; 32]),
                        len: 7,
                    },
                    annex: ObjectRef {
                        hash: ContentHash([6; 32]),
                        len: 8,
                    },
                },
            ],
            foreign_modules: vec![ForeignLeaf {
                module: DefPath::from_full("lib@1/native").unwrap(),
                symbols: "hfl_".into(),
                targets: vec![ForeignTarget {
                    target: "target".into(),
                    archive: ObjectRef {
                        hash: ContentHash([7; 32]),
                        len: 11,
                    },
                }],
                functions: vec![ForeignFunction {
                    identity: Identity::plain(DefPath::from_full("lib@1/native/open").unwrap()),
                    header: "fn()".into(),
                }],
            }],
            foreign_archives: vec![
                ObjectRef {
                    hash: ContentHash([7; 32]),
                    len: 11,
                },
                ObjectRef {
                    hash: ContentHash([8; 32]),
                    len: 12,
                },
            ],
            launch: Launch {
                backend: Backend::Cranelift,
                target: "target".into(),
                root_limit: 9,
                n_place: 10,
                exit_status: true,
                grants: vec![
                    GrantBinding {
                        ordinal: 2,
                        kind: GrantKind::Dir,
                        values: vec!["a".into(), "b".into()],
                        archive: None,
                        policy: None,
                    },
                    GrantBinding {
                        ordinal: 3,
                        kind: GrantKind::Foreign,
                        values: vec!["lib@1/wrapper/Audit".into()],
                        archive: Some(ObjectRef {
                            hash: ContentHash([8; 32]),
                            len: 12,
                        }),
                        policy: None,
                    },
                ],
            },
        };
        // Independent frame, including two-byte ULEB for the group's length.
        let mut bytes = b"HIRRECIP\0\x02\0\x03\0\x0fapp@1/main/main\0\0".to_vec();
        bytes.extend_from_slice(&[1; 32]);
        bytes.push(1);
        bytes.extend_from_slice(&[2; 32]);
        let length = bytes.len();
        bytes.extend_from_slice(&[0xac, 0x02, 2]);
        for (hash, size) in [(3, 5), (4, 6), (5, 7), (6, 8)] {
            bytes.extend_from_slice(&[hash; 32]);
            bytes.push(size);
        }
        bytes.extend_from_slice(b"\x01\x0clib@1/native\x04hfl_\x01\x06target");
        bytes.extend_from_slice(&[7; 32]);
        bytes.extend_from_slice(b"\x0b\x01\x11lib@1/native/open\0\0\x04fn()");
        bytes.push(2);
        for (hash, size) in [(7, 11), (8, 12)] {
            bytes.extend_from_slice(&[hash; 32]);
            bytes.push(size);
        }
        bytes.extend_from_slice(b"\0\x06target");
        bytes.extend_from_slice(&9u64.to_le_bytes());
        bytes.extend_from_slice(&10u64.to_le_bytes());
        bytes.extend_from_slice(b"\x01\x02\x02\0\x02\x01a\x01b\x03\x0c\x01\x13lib@1/wrapper/Audit");
        bytes.extend_from_slice(&[8; 32]);
        bytes.push(12);
        let decode = |bytes: &[u8]| Recipe::decode(bytes, &mut DecodeBudget::unlimited());
        assert_eq!(recipe.encode(), bytes);
        assert_eq!(decode(&bytes), Ok(recipe.clone()));

        let mut version_one = bytes.clone();
        version_one[9] = 1;
        assert_eq!(
            decode(&version_one),
            Err(DecodeError::Unsupported("code frame version"))
        );
        let mut extra_archive = recipe.clone();
        extra_archive.foreign_archives.swap(0, 1);
        assert_eq!(
            decode(&extra_archive.encode()),
            Err(invalid("unordered foreign archives"))
        );

        let mut redundant = bytes.clone();
        redundant.splice(length..length + 2, [0xac, 0x82, 0x00]);
        assert_eq!(decode(&redundant), Err(invalid("invalid unsigned LEB128")));
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            decode(&trailing),
            Err(invalid("trailing normal form bytes"))
        );

        let mut duplicate = recipe;
        duplicate.bodies[1].body.hash = duplicate.bodies[0].body.hash;
        assert_eq!(
            decode(&duplicate.encode()),
            Err(invalid("unordered body references"))
        );
        let mut budget = DecodeBudget::new(DecodeLimits {
            bytes: 0,
            members: usize::MAX,
            slots: usize::MAX,
            depth: usize::MAX,
            deadline: None,
        });
        assert_eq!(
            Recipe::decode(&bytes, &mut budget),
            Err(DecodeError::Limit(DecodeLimit::Bytes))
        );
    }
}
