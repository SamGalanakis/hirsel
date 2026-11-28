//! Reviewed decoder inputs. These have no mutable state or property search tape.
use hirsel_core_ids::hex;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle<C> {
    schema_version: u32,
    objects: BTreeMap<String, String>,
    cases: Vec<C>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Case<L, S, E, P> {
    pub id: String,
    target: String,
    law: String,
    input_kind: String,
    generator_version: u32,
    digest: String,
    pub limits: L,
    pub setup: S,
    oracle: Oracle,
    pub expected: E,
    pub schedules: Vec<P>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Oracle {
    name: String,
    version: u32,
}

/// Load the exact cohort declared by the owner. The type arguments specify its
/// limits, setup, expected observations and schedules.
pub(crate) fn read<
    L: DeserializeOwned,
    S: DeserializeOwned,
    E: DeserializeOwned,
    P: DeserializeOwned,
>(
    json: &[u8],
    target: &str,
    law: &str,
    input_kind: &str,
    oracle: &str,
) -> Result<Vec<(Case<L, S, E, P>, Vec<u8>)>, String> {
    #[derive(Deserialize)]
    struct Version {
        schema_version: u32,
    }
    let version: Version = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    if version.schema_version != 1 {
        return Err(format!(
            "unsupported reviewed schema version {}",
            version.schema_version
        ));
    }
    let bundle: Bundle<Case<L, S, E, P>> =
        serde_json::from_slice(json).map_err(|e| e.to_string())?;
    debug_assert_eq!(bundle.schema_version, 1);
    if bundle.cases.is_empty() || bundle.cases.windows(2).any(|c| c[0].id >= c[1].id) {
        return Err("expected a nonempty, strictly sorted case list".into());
    }
    let mut objects = BTreeMap::new();
    for (key, encoded) in bundle.objects {
        let bytes = hex::decode(&encoded).ok_or("invalid object hex")?;
        if encoded.bytes().any(|b| b.is_ascii_uppercase())
            || hex::encode(&Sha256::digest(&bytes)) != key
        {
            return Err(format!("invalid reviewed object {key}"));
        }
        objects.insert(key, bytes);
    }
    let mut used = BTreeSet::new();
    let mut cases = Vec::new();
    for case in bundle.cases {
        if case.generator_version != 1 {
            return Err(format!(
                "{}: stale generator version {}",
                case.id, case.generator_version
            ));
        }
        if case.id.is_empty()
            || case.target != target
            || case.law != law
            || case.input_kind != input_kind
            || case.oracle.name != oracle
            || case.oracle.version != 1
            || case.schedules.is_empty()
        {
            return Err(format!("{}: wrong cohort, oracle or schedules", case.id));
        }
        let payload = objects
            .get(&case.digest)
            .ok_or_else(|| format!("{}: missing object {}", case.id, case.digest))?;
        used.insert(case.digest.clone());
        cases.push((case, payload.clone()));
    }
    if used.len() != objects.len() {
        return Err("unreferenced reviewed object".into());
    }
    Ok(cases)
}
