use super::{CheckProduct, DefPath, R, snapshot::Snapshot};
use hirsel_core_ir::{FuncId, Identity};
use hirsel_core_layout::Target;
use std::collections::BTreeMap;

pub(super) fn capture(
    product: &CheckProduct,
    prefix: &str,
    names: &[String],
    target: Target,
) -> R<Snapshot> {
    let roots = names
        .iter()
        .map(|name| {
            let path = DefPath::from_full(&format!("{prefix}/{name}"))
                .ok_or_else(|| format!("{name}: invalid definition path"))?;
            Ok(Identity::plain(path))
        })
        .collect::<R<Vec<_>>>()?;
    let first = roots.first().ok_or("no functions named")?;
    let units = product
        .units(roots.iter().cloned(), crate::native::DEFAULT_INSTANCE_LIMIT)
        .map_err(|error| error.to_string())?;
    let units = units.modules().map_err(|error| error.to_string())?;
    let (module, _) = hirsel_core_materialize::join(&units, first)?;
    let functions: BTreeMap<_, _> = module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, function)| function.body.is_some())
        .map(|(index, _)| {
            let function = FuncId(index as u32);
            (module.code_identity(function), function)
        })
        .collect();
    let roots = roots
        .iter()
        .map(|root| {
            functions
                .get(&root.digest())
                .copied()
                .ok_or_else(|| format!("{}: no linked definition", root.definition))
        })
        .collect::<R<Vec<_>>>()?;
    let sources = product
        .native_sources(&module.items)
        .map_err(|error| error.to_string())?;
    Snapshot::capture(module, roots, target, None, &sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CheckRequest, Driver, LibraryInput, Meter};
    use std::path::Path;
    use std::time::Instant;

    #[test]
    fn linked_langarena_round_trips_as_checked_coreir() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let directory = tempfile::tempdir().unwrap();
        let mut inputs = crate::tests::langarena_inputs(
            &root,
            &root.join("core/core"),
            &root.join("core/langarena"),
        );
        // The nested layout and IO fixtures use their own source manifests.
        inputs
            .packages
            .last_mut()
            .unwrap()
            .sources
            .retain(|source| source.modules.len() == 1);
        let product = Driver::default()
            .check(
                CheckRequest {
                    manifest: None,
                    inputs,
                    store: directory.path().join("store"),
                    library: LibraryInput::Sources,
                    entry: None,
                },
                &mut Meter::new(false),
            )
            .unwrap();
        assert!(product.accepted(), "{:?}", product.diagnostics);

        let start = Instant::now();
        let snapshot = capture(
            &product,
            "langarena@1/template",
            &["main".into()],
            Target::X86_64Linux,
        )
        .unwrap();
        let mut bytes = Vec::new();
        snapshot.write(&mut bytes).unwrap();
        let decoded = Snapshot::read(bytes.as_slice()).unwrap();
        let printed = decoded.print();
        assert_eq!(printed, hirsel_core_ir::text::print(&snapshot.module));
        assert_eq!(snapshot, decoded);
        eprintln!(
            "LangArena template/main: {} functions, {} instructions, {} bytes, {:?}",
            snapshot.module.functions.len(),
            snapshot
                .module
                .functions
                .iter()
                .filter_map(|function| function.body.as_ref())
                .map(|body| body.insts.len())
                .sum::<usize>(),
            bytes.len(),
            start.elapsed(),
        );
    }
}
