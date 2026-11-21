//! Canonical release recipes, independent of the native toolchain.

use crate::Identity;
use crate::decode::{DecodeBudget, DecodeError, DecodeResult};
use crate::normal::{
    encode_identity, uleb,
    wire::{Read, Wire, invalid},
};
use hirsel_core_ids::ContentHash;

const MAGIC: &[u8] = b"HIRRECIP\0\x01\x00";
const NORMAL_FORM: u16 = 1;

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
}

impl GrantKind {
    const KINDS: [Self; 12] = [
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

fn text(bytes: &mut Vec<u8>, value: &str) {
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

impl Recipe {
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
        }
        bytes
    }

    pub fn hash(bytes: &[u8]) -> ContentHash {
        ContentHash::of_parts("hirsel.release-recipe.v1", [bytes])
    }

    /// Decode the whole frame before exposing its ordered object lists.
    /// Annexes are decoded against their corresponding bodies at admission.
    pub fn decode(bytes: &[u8], budget: &mut DecodeBudget) -> DecodeResult<Self> {
        let mut input = Read::new(bytes, MAGIC, budget)?;
        if u16::read(&mut input)? != NORMAL_FORM {
            return Err(DecodeError::Unsupported("recipe normal-form version"));
        }
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
            let values = Wire::read(input)?;
            Ok(GrantBinding {
                ordinal,
                kind,
                values,
            })
        })?;
        input.end()?;
        Ok(Self {
            entry,
            address,
            groups,
            bodies,
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
            launch: Launch {
                backend: Backend::Cranelift,
                target: "target".into(),
                root_limit: 9,
                n_place: 10,
                exit_status: true,
                grants: vec![GrantBinding {
                    ordinal: 2,
                    kind: GrantKind::Dir,
                    values: vec!["a".into(), "b".into()],
                }],
            },
        };
        // Independent frame, including two-byte ULEB for the group's length.
        let mut bytes = b"HIRRECIP\0\x01\0\x01\0\x0fapp@1/main/main\0\0".to_vec();
        bytes.extend_from_slice(&[1; 32]);
        bytes.push(1);
        bytes.extend_from_slice(&[2; 32]);
        let length = bytes.len();
        bytes.extend_from_slice(&[0xac, 0x02, 2]);
        for (hash, size) in [(3, 5), (4, 6), (5, 7), (6, 8)] {
            bytes.extend_from_slice(&[hash; 32]);
            bytes.push(size);
        }
        bytes.extend_from_slice(b"\0\x06target");
        bytes.extend_from_slice(&9u64.to_le_bytes());
        bytes.extend_from_slice(&10u64.to_le_bytes());
        bytes.extend_from_slice(b"\x01\x01\x02\0\x02\x01a\x01b");
        let decode = |bytes: &[u8]| Recipe::decode(bytes, &mut DecodeBudget::unlimited());
        assert_eq!(recipe.encode(), bytes);
        assert_eq!(decode(&bytes), Ok(recipe.clone()));

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
