use super::{DefinitionId, Resolver};

impl Resolver<'_, '_> {
    // The installed Store may fund the append-only cursor observer before
    // entering the recorded call. Applications cannot construct its handle.
    pub(super) fn witness_access(&self, id: DefinitionId) -> bool {
        let callee = self.index.definition_path(id);
        let caller = self.index.module_path(self.module);
        if !self.index.supplied_by_toolchain(&caller.package) {
            return false;
        }
        if callee.is_core("log.samples") {
            return caller.package.package_is("store", "1") && caller.modules == ["store"];
        }
        // Only the Runner that checked the immutable keyed policy may export.
        callee.package_is("store", "1")
            && self.index.supplied_by_toolchain(&callee.0.package)
            && callee.0.modules == ["store"]
            && callee.0.name == "encode_witness"
            && caller.package.package_is("runner", "1")
            && caller.modules == ["runner"]
    }
    // The parser only admits a dollar in compiler-generated items. The codec
    // derive may use std's descriptor protocol without exporting its makers.
    pub(super) fn codec_storage_access(&self, id: DefinitionId) -> bool {
        let Some(token) = self.item.tree.node(self.item.root).token else {
            return false;
        };
        let item = self.index.symbols.text(self.item.symbol(token));
        let Some((_, operation)) = item.rsplit_once('$') else {
            return false;
        };
        let path = self.index.definition_path(id);
        if !path.package_is("std", "1") || path.0.modules != ["codec"] {
            return false;
        }
        match operation {
            "codec" => matches!(path.0.name.as_str(), "storage_make" | "storage_checked"),
            "schema" => matches!(
                path.0.name.as_str(),
                "storage_mode" | "storage_via" | "storage_to" | "storage_field_end"
            ),
            _ => false,
        }
    }
}
