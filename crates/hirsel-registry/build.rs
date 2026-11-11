hirsel_guard::install!();

use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[allow(dead_code)]
#[path = "src/declarations.rs"]
mod registry;

#[path = "src/products.rs"]
mod products;

fn core_products(out: &Path) {
    use hirsel_artifact_store::{Contents, Domain, Memory, Store};
    use hirsel_product_codec::{BaseIndex, TypeRecords};
    use hirsel_types::{TypeInterner, TypeKind};
    use std::sync::Arc;
    let types = TypeInterner::new(None);
    let signatures = registry::build::registry(&types);
    let revision = hirsel_syntax::ContentHash::of_parts(
        "hirsel.core.registry.v9",
        signatures.values().map(|s| s.fingerprint.0.as_slice()),
    );
    let roots: Vec<_> = types.ids().collect();
    let mut variants = std::collections::BTreeSet::<String>::new();
    for &id in &roots {
        if let TypeKind::Variant(cases) = &types.row(id).kind {
            variants.extend(cases.iter().map(|v| v.name.as_str().to_string()));
        }
    }
    let memory = Arc::new(Memory::default());
    let records = TypeRecords::new(types, memory.clone());
    let base = BaseIndex::capture(&records, &roots).expect("core type index");
    let signatures = signatures
        .into_iter()
        .map(|(d, signature)| {
            let bytes = records
                .encode_record(&signature, |_| None)
                .expect("core signature record");
            let reference = memory
                .put_content(Domain { kind: 3, schema: 1 }, &bytes)
                .expect("bounded core signature");
            (d, reference)
        })
        .collect();
    let index = products::CoreProducts {
        revision,
        base,
        signatures,
        variants,
    };
    fs::write(
        out.join("core-products.index"),
        bincode::serialize(&index).unwrap(),
    )
    .unwrap();
    // Embed ordinary Store containers, including their normal checked header.
    // There is no second image framing, codec or admission implementation.
    let store = Store::new(out.join("core-store"));
    let mut entries = memory.entries();
    entries.sort_unstable_by_key(|(r, _)| r.output.0);
    let mut source = String::from("hirsel_artifact_store::Embedded(&[\n");
    for (reference, bytes) in entries {
        assert_eq!(store.put(reference.domain, &bytes).unwrap(), reference);
        let relative = format!(
            "core-containers/{}",
            reference
                .output
                .0
                .0
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        fs::create_dir_all(out.join("core-containers")).unwrap();
        fs::write(out.join(&relative), &*store.container(reference).unwrap()).unwrap();
        source.push_str(&format!(
            "(hirsel_artifact_store::Reference {{ output: hirsel_artifact_store::Output(hirsel_syntax::ContentHash({:?})), domain: hirsel_artifact_store::Domain {{ kind: {}, schema: {} }}, len: {} }}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{}\"))),\n",
            reference.output.0.0, reference.domain.kind, reference.domain.schema, reference.len, relative
        ));
    }
    source.push_str("])\n");
    fs::write(out.join("core-products.rs"), source).unwrap();
}

fn main() {
    println!("cargo:rerun-if-changed=src");
    core_products(&PathBuf::from(env::var_os("OUT_DIR").unwrap()));
}
