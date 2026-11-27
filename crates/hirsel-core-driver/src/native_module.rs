//! `--emit module`: a build's checked units as one `.hirsel-mod` (PACK-2), linked in
//! this process with no linker and no runtime archive.
use crate::NativeStop;
use crate::native::NativeOptions;
use hirsel_core_ids::ContentHash;
use hirsel_core_ir::Identity;
use hirsel_core_layout::Target;
use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

#[path = "module_link.rs"]
mod link;

fn build_error(error: impl std::fmt::Display) -> NativeStop {
    NativeStop::Link(format!("module: {error}"))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pack(
    entry: &crate::native_entry::Plan<'_>,
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
            let revisions: Vec<_> = revisions()?
                .into_iter()
                .map(|(id, hash)| (hirsel_core_ir::revision_symbol(id.digest()), hash.0))
                .collect();
            let support = entry
                .emit_module_support(target, &sources, &revisions)
                .map_err(build_error)?;
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
            inputs.extend(members.iter().map(|member| &member[..]));
            let roots = [link::Root {
                symbol: entry.symbol(),
                signature: entry.signature()?,
                waits: false,
            }];
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
