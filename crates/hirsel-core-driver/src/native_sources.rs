//! Native sites bind stable item identities to this request's source snapshot.
use super::*;
use hirsel_core_ir::source::{ItemSource, SourceFile, SourceMap};
use std::collections::BTreeSet;
use std::fmt::Write;

pub(crate) fn entry_table(sources: &SourceMap) -> String {
    let mut out = String::new();
    for (i, item) in sources.items.keys().enumerate() {
        writeln!(
            out,
            "const uint64_t {} = UINT64_C({}) << 32;",
            hirsel_core_ir::source::site_symbol(item),
            i + 1,
        )
        .unwrap();
    }
    out.push_str("static const HirselCoreSourceFile hirsel_source_files[] = {");
    if sources.files.is_empty() {
        out.push_str("{{0,0},{0,0}}");
    }
    for file in &sources.files {
        write!(
            out,
            "{{{},{}}},",
            crate::native::c_view(&file.path),
            crate::native::c_view(&file.text)
        )
        .unwrap();
    }
    out.push_str("};\nstatic const HirselCoreSourceItem hirsel_source_items[] = {");
    if sources.items.is_empty() {
        out.push_str("{0,0}");
    }
    for item in sources.items.values() {
        write!(out, "{{{}, {}}},", item.file, item.file_offset).unwrap();
    }
    writeln!(out, "}};\nstatic const HirselCoreSourceTable hirsel_sources = {{hirsel_source_files,{},hirsel_source_items,{}}};", sources.files.len(), sources.items.len()).unwrap();
    out
}

impl CheckProduct {
    /// Demand only the source files named by native units. The published image
    /// has compact source rows, so this never loads a library file's syntax tree.
    pub fn native_sources(&self, items: &[DefPath]) -> io::Result<SourceMap> {
        hirsel_core_ids::published::catch_io(|| self.native_sources_inner(items))
    }

    fn native_sources_inner(&self, items: &[DefPath]) -> io::Result<SourceMap> {
        let mut result = SourceMap {
            files: vec![],
            items: BTreeMap::new(),
        };
        let mut files = BTreeMap::new();
        for path in items.iter().collect::<BTreeSet<_>>() {
            let header = self.headers.get(path).ok_or_else(|| {
                io::Error::other(format!("native source item is not indexed: {path}"))
            })?;
            let (module, item) = header.anchor;
            let own = self.files.iter().find(|file| file.module == module);
            let file_offset = if let Some(file) = own {
                file.syntax.items[item as usize].file_offset
            } else {
                self.image
                    .as_ref()
                    .ok_or_else(|| io::Error::other("native source has no package image"))?
                    .file_offset(module, item)?
            };
            let file = if let Some(&file) = files.get(&module) {
                file
            } else {
                let source = if let Some(file) = own {
                    SourceFile {
                        path: file.path.to_string_lossy().into_owned(),
                        text: file.syntax.source.clone(),
                    }
                } else {
                    self.image.as_ref().unwrap().source_file(module)?
                };
                let file = u32::try_from(result.files.len())
                    .map_err(|_| io::Error::other("native source file table exceeds u32"))?;
                result.files.push(source);
                files.insert(module, file);
                file
            };
            result
                .items
                .insert(path.clone(), ItemSource { file, file_offset });
        }
        Ok(result)
    }
}
