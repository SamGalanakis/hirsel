//! `--emit module`: a build's checked units as one `.hirsel-mod` (PACK-2), linked in
//! this process with no linker and no runtime archive.
use crate::native::NativeOptions;
use crate::{CheckProduct, NativeStop, PackExportDecl};
use hirsel_core_ids::{ContentHash, DefPath};
use hirsel_core_ir::Identity;
use hirsel_core_layout::Target;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::Arc;

#[path = "module_link.rs"]
mod link;

fn build_error(error: impl std::fmt::Display) -> NativeStop {
    NativeStop::Link(format!("module: {error}"))
}

fn emit_export_adapter(
    export: &PackExportDecl,
    options: &NativeOptions,
) -> Result<Vec<u8>, NativeStop> {
    let instance = Identity::plain(DefPath(hirsel_core_ids::ItemPath {
        package: export.path.0.package.clone(),
        modules: export.path.0.modules.clone(),
        name: format!("{}$pack_export", export.path.0.name),
    }));
    let object_key = ContentHash::of_parts(
        "hirsel.pack-export-object.v1",
        [
            export.adapter_symbol.as_bytes(),
            &export.signature.0,
            options.target.as_bytes(),
        ],
    );
    let symbols = export.module.link_symbols();
    let obj = super::emit_unit(&export.module, || symbols, options, object_key, &instance)
        .map_err(|message| NativeStop::Emit {
            definition: export.path.clone(),
            message,
        })?;
    Ok(obj.bytes)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pack(
    entry: &crate::native_entry::Plan<'_>,
    exports: &[&PackExportDecl],
    objects: &[super::ObjectProduct],
    options: &NativeOptions,
    sources: impl FnOnce() -> io::Result<(Arc<hirsel_core_ir::source::SourceMap>, Option<ContentHash>)>,
    key: ContentHash,
    store: &crate::Store,
    mut counters: crate::reach::ReachCounters,
    address: ContentHash,
    revisions: impl FnOnce() -> Result<BTreeMap<Identity, ContentHash>, NativeStop>,
) -> Result<super::Linked, NativeStop> {
    use super::{BuildProduct, Linked};
    let revision = hirsel_core_ir::code_revision(address);
    let compiled = objects.iter().filter(|o| o.compiled).count();
    let (bytes, linked) = match store.get(key)? {
        Some(bytes) => (bytes, false),
        None => {
            let references = objects.iter().collect::<Vec<_>>();
            let members = match super::link_input::object_members(&references, &mut counters)? {
                Ok(members) => members,
                Err(refused) => return Ok(Linked::Damaged(refused, compiled, counters)),
            };
            let (sources, sources_key) = sources()?;
            let target = Target::from_triple(&options.target)
                .ok_or_else(|| build_error(format_args!("no target {}", options.target)))?;
            let keys: Vec<_> = objects.iter().map(|o| (o.identity, o.check)).collect();
            let (sources, _) =
                crate::native_entry::runtime_sources(&sources, sources_key, &members, &keys)?;
            let mut revisions: Vec<_> = revisions()?
                .into_iter()
                .map(|(id, hash)| (hirsel_core_ir::revision_symbol(id.digest()), hash.0))
                .collect();
            for export in exports {
                if let Some((idx, _)) = export
                    .module
                    .functions
                    .iter()
                    .enumerate()
                    .find(|(_, f)| f.pack_export)
                {
                    let root = export
                        .module
                        .revision_root(hirsel_core_ir::FuncId(idx as u32));
                    let sym = hirsel_core_ir::revision_symbol(root.identity().digest());
                    if !revisions.iter().any(|(s, _)| s == &sym) {
                        revisions.push((sym, revision.0));
                    }
                }
            }
            let support = entry
                .emit_module_support(target, &sources, &revisions)
                .map_err(build_error)?;
            let mut adapter_objects = Vec::with_capacity(exports.len());
            for export in exports {
                adapter_objects.push(emit_export_adapter(export, options)?);
            }
            let header = hirsel_mod::Header {
                target,
                backend: match options.backend {
                    super::Backend::Cranelift => hirsel_mod::Backend::Cranelift,
                    super::Backend::Llvm => hirsel_mod::Backend::Llvm,
                },
                policy: u64::from(options.recording),
                abi: hirsel_mod::abi(),
                compiler: crate::reuse::compiler_identity(),
                coreir: address,
                text_align: 64,
                data_align: 64,
            };
            let mut inputs: Vec<&[u8]> = vec![&support];
            inputs.extend(adapter_objects.iter().map(|b| b.as_slice()));
            inputs.extend(members.iter().map(|member| &member[..]));
            let roots: Vec<link::Root<'_>> = if exports.is_empty() {
                vec![link::Root {
                    identity: None,
                    symbol: entry.symbol(),
                    descriptor: None,
                    signature: entry.signature()?,
                    waits: false,
                }]
            } else {
                exports
                    .iter()
                    .map(|e| link::Root {
                        identity: Some(e.entry_identity),
                        symbol: &e.adapter_symbol,
                        descriptor: e.descriptor_symbol.as_deref(),
                        signature: e.signature,
                        waits: e.waits,
                    })
                    .collect()
            };
            let site_bytes =
                crate::native_entry::encode_module_sources(&sources).map_err(build_error)?;
            let image =
                link::link(header, &inputs, &roots, site_bytes).map_err(NativeStop::Module)?;
            let bytes = image.encode();
            hirsel_mod::Module::parse(&bytes)
                .map_err(|e| build_error(format_args!("the linked image is {e:?}")))?;
            store.put_keyed(key, &bytes)?;
            (bytes, true)
        }
    };
    let product = BuildProduct {
        private_variants: objects.iter().map(|o| o.private_variants).sum(),
        executable: options.output.clone(),
        identity: key,
        address,
        recipe: None,
        assembly: None,
        revision,
        compiled,
        linked,
        observation_mode: false,
        debug_lines: options.debug_lines,
        reach: counters,
        coverage: None,
    };
    match std::fs::write(&options.output, &bytes) {
        Ok(()) => Ok(Linked::Built(product)),
        Err(error) => Err(NativeStop::Output {
            product: Box::new(product),
            error,
        }),
    }
}

impl CheckProduct {
    /// Build a multi-export `.hirsel-mod` containing every closed function listed in
    /// `exports` with its PACK-15 typed export adapter and `PACK-14` (`hirsel.pack-export.v1`)
    /// signature digest on either Cranelift or LLVM.
    pub fn build_pack_module(
        &self,
        exports: &[DefPath],
        options: &NativeOptions,
    ) -> Result<Vec<u8>, NativeStop> {
        if exports.is_empty() {
            return Err(NativeStop::Entry(
                "build_pack_module requires at least one export".into(),
            ));
        }
        // A module's candidates form a set. Registration order belongs to the
        // bound Place, so it must not change this realization's key or bytes.
        let candidates: BTreeSet<_> = exports.iter().collect();
        if candidates.len() != exports.len() {
            return Err(NativeStop::Entry("duplicate module export".into()));
        }
        let mut decls = Vec::with_capacity(candidates.len());
        for path in candidates {
            let decl = self.pack_exports.get(path).ok_or_else(|| {
                NativeStop::Entry(format!("no closed PACK-14 export available for {path}"))
            })?;
            decls.push(decl);
        }
        let target = Target::from_triple(&options.target)
            .ok_or_else(|| build_error(format_args!("no target {}", options.target)))?;
        let mut seen_instances: BTreeMap<Identity, Vec<u8>> = BTreeMap::new();
        let mut all_revisions: BTreeMap<Identity, ContentHash> = BTreeMap::new();
        let mut all_items: BTreeSet<DefPath> = BTreeSet::new();
        let mut address_parts: Vec<ContentHash> = Vec::with_capacity(exports.len());
        let mut adapter_objects = Vec::with_capacity(decls.len());
        for decl in &decls {
            let reached = self.reachable_objects(&decl.path, options)?;
            let instances = reached.require_native(self, &decl.path, options)?;
            let reached_addr = reached.address(&decl.path)?;
            address_parts.push(reached_addr);
            for (id, rev) in reached.revisions()? {
                all_revisions.insert(id, rev);
            }
            if let Some((idx, _)) = decl
                .module
                .functions
                .iter()
                .enumerate()
                .find(|(_, f)| f.pack_export)
            {
                let root = decl
                    .module
                    .revision_root(hirsel_core_ir::FuncId(idx as u32));
                all_revisions.insert(
                    root.identity().clone(),
                    hirsel_core_ir::code_revision(reached_addr),
                );
            }
            for inst in instances {
                for item in reached.materialized.items(inst)? {
                    all_items.insert(item.clone());
                }
            }
            let (objects, _) = self.emit_reached(&decl.path, &reached, options)?;
            let mut counters = crate::reach::ReachCounters::default();
            let refs: Vec<_> = objects.iter().collect();
            let members = match super::link_input::object_members(&refs, &mut counters)? {
                Ok(m) => m,
                Err(refused) => {
                    return Err(NativeStop::Link(format!(
                        "damaged object during pack module build: {refused:?}"
                    )));
                }
            };
            for (obj, member) in objects.iter().zip(members) {
                seen_instances
                    .entry(obj.instance.clone())
                    .or_insert_with(|| member.to_vec());
            }
            adapter_objects.push(emit_export_adapter(decl, options)?);
        }
        let item_refs: Vec<&DefPath> = all_items.iter().collect();
        let rows = self.link_rows(None, &[], &item_refs, false)?;
        let sources = self.source_map(&rows)?;
        let revisions: Vec<_> = all_revisions
            .into_iter()
            .map(|(id, hash)| (hirsel_core_ir::revision_symbol(id.digest()), hash.0))
            .collect();
        let support =
            crate::native_entry::emit_module_support_with_sites(target, &sources, &[], &revisions)
                .map_err(build_error)?;
        let coreir = ContentHash::of_parts(
            "hirsel.pack-module-coreir.v1",
            address_parts.iter().map(|h| h.0.as_slice()),
        );
        let header = hirsel_mod::Header {
            target,
            backend: match options.backend {
                super::Backend::Cranelift => hirsel_mod::Backend::Cranelift,
                super::Backend::Llvm => hirsel_mod::Backend::Llvm,
            },
            policy: u64::from(options.recording),
            abi: hirsel_mod::abi(),
            compiler: crate::reuse::compiler_identity(),
            coreir,
            text_align: 64,
            data_align: 64,
        };
        let mut inputs: Vec<&[u8]> = vec![&support];
        inputs.extend(adapter_objects.iter().map(|b| b.as_slice()));
        inputs.extend(seen_instances.values().map(|b| b.as_slice()));
        let roots: Vec<link::Root<'_>> = decls
            .iter()
            .map(|e| link::Root {
                identity: Some(e.entry_identity),
                symbol: &e.adapter_symbol,
                descriptor: e.descriptor_symbol.as_deref(),
                signature: e.signature,
                waits: e.waits,
            })
            .collect();
        let site_bytes =
            crate::native_entry::encode_module_sources(&sources).map_err(build_error)?;
        let image = link::link(header, &inputs, &roots, site_bytes).map_err(NativeStop::Module)?;
        let bytes = image.encode();
        hirsel_mod::Module::parse(&bytes)
            .map_err(|e| build_error(format_args!("the linked image is {e:?}")))?;
        Ok(bytes)
    }
}
