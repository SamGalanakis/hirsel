//! Program-wide role facts are a pure derived product of identified inputs.
use crate::{Store, decode, encode, native};
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::{
    Module,
    roles::{BodyFacts, FACTS_SCHEMA, Facts},
};
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Serialize, Deserialize)]
pub(crate) struct ProgramFacts {
    pub facts: Vec<Facts>,
    pub consumed: Vec<Vec<BodyFacts>>,
}

pub(crate) fn key(
    compiler: ContentHash,
    entry: &DefPath,
    modules: &[ContentHash],
    instances: &[hirsel_core_ir::InstanceKey],
    recorded: &[ContentHash],
) -> io::Result<ContentHash> {
    native::key(
        "hirsel.native-program-facts.v1",
        &(FACTS_SCHEMA, compiler, entry, modules, instances, recorded),
    )
}

pub(crate) fn get(
    store: &Store,
    key: ContentHash,
    modules: &[&Module],
    recorded: &[BodyFacts],
) -> io::Result<ProgramFacts> {
    let selection = format!("facts-{}", native::selection(key));
    if let Some(identity) = store.selection(&selection)?
        && let Some(bytes) = store.get(identity)?
        && let Some(facts) = decode(&bytes)
    {
        return Ok(facts);
    }
    let facts = Facts::program(modules, recorded);
    let consumed = modules
        .iter()
        .zip(&facts)
        .map(|(module, facts)| facts.consumed(module))
        .collect();
    let result = ProgramFacts { facts, consumed };
    let identity = store.put(&encode(&result)?)?;
    store.select(&selection, identity)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_core_ir::{FuncId, text};

    fn module(caller: bool, escape: bool) -> Module {
        let body = if caller {
            "call f1 g0 (&a0)"
        } else if escape {
            "call f2 g0 (&a0)"
        } else {
            ""
        };
        let definition = |i| {
            format!(
                r#"fn f{i} = n{i} g0 {{
   addr a0 = v0 : t0
 b0(v0: ptr):
   address a0
   {body}
   return
 }}"#
            )
        };
        let caller_fn = if caller {
            definition(0)
        } else {
            "fn f0 = n0 g0 extern".into()
        };
        let leaf_fn = if caller {
            "fn f1 = n1 g0 extern".into()
        } else {
            definition(1)
        };
        text::parse(&format!(
            r#"core v1
 type t0 = own "String" {{ bytes: seq u8 }}
 sig g0 = (&mut t0) -> ()
 inst n0 = app@1/caller [] []
 inst n1 = lib@1/leaf [] []
 inst n2 = lib@1/opaque [] []
 {caller_fn}
 {leaf_fn}
 fn f2 = n2 g0 extern
 "#
        ))
        .unwrap()
    }
    fn identities(modules: &[&Module]) -> Vec<ContentHash> {
        modules
            .iter()
            .map(|m| ContentHash::of_bytes(&encode(m).unwrap()))
            .collect()
    }
    fn key(modules: &[ContentHash], recorded: &[ContentHash]) -> io::Result<ContentHash> {
        super::key(
            ContentHash::default(),
            &DefPath::core("caller"),
            modules,
            &[],
            recorded,
        )
    }
    fn fresh(modules: &[&Module], recorded: &[BodyFacts]) -> ProgramFacts {
        let facts = Facts::program(modules, recorded);
        let consumed = modules
            .iter()
            .zip(&facts)
            .map(|(m, f)| f.consumed(m))
            .collect();
        ProgramFacts { facts, consumed }
    }
    #[test]
    fn stored_facts_and_consumed_rows_equal_fresh_derivation() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path()).unwrap();
        let caller = module(true, false);
        let leaf = module(false, false);
        let modules = [&caller, &leaf];
        let key = key(&identities(&modules), &[]).unwrap();
        let expected = encode(&fresh(&modules, &[])).unwrap();
        assert_eq!(
            encode(&get(&store, key, &modules, &[]).unwrap()).unwrap(),
            expected
        );
        assert_eq!(
            encode(&get(&store, key, &modules, &[]).unwrap()).unwrap(),
            expected
        );
    }
    #[test]
    fn callee_body_identity_invalidates_callers_fixed_point() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path()).unwrap();
        let caller = module(true, false);
        let blind = module(false, false);
        let escaping = module(false, true);
        let before = [&caller, &blind];
        let after = [&caller, &escaping];
        let a = key(&identities(&before), &[]).unwrap();
        let b = key(&identities(&after), &[]).unwrap();
        assert_ne!(a, b);
        let a = get(&store, a, &before, &[]).unwrap();
        let b = get(&store, b, &after, &[]).unwrap();
        assert!(a.facts[0].place_blind(FuncId(0), 0));
        assert!(!b.facts[0].place_blind(FuncId(0), 0));
        assert_eq!(encode(&b).unwrap(), encode(&fresh(&after, &[])).unwrap());
    }
    #[test]
    fn recorded_summary_identity_changes_consumed_declaration_facts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path()).unwrap();
        let caller = module(true, false);
        let blind = module(false, false);
        let escaping = module(false, true);
        let a = Facts::new(&blind).record(&blind);
        let b = Facts::new(&escaping).record(&escaping);
        let modules = [&caller];
        let a_key = key(
            &identities(&modules),
            &[ContentHash::of_bytes(&encode(&a).unwrap())],
        )
        .unwrap();
        let b_key = key(
            &identities(&modules),
            &[ContentHash::of_bytes(&encode(&b).unwrap())],
        )
        .unwrap();
        assert_ne!(a_key, b_key);
        let a = get(&store, a_key, &modules, &a).unwrap();
        let b = get(&store, b_key, &modules, &b).unwrap();
        assert!(a.facts[0].place_blind(FuncId(0), 0));
        assert!(!b.facts[0].place_blind(FuncId(0), 0));
        assert_ne!(encode(&a.consumed).unwrap(), encode(&b.consumed).unwrap());
    }
}
