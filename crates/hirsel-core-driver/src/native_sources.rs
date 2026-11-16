//! Native sites bind stable item identities to this request's source snapshot.
use super::*;
use hirsel_core_ir::source::{ItemSource, SourceFile, SourceMap};

impl CheckProduct {
    /// Demand only the source files named by native units. The published image
    /// has compact source rows, so this never loads a library file's syntax tree.
    pub fn native_sources(&self, items: &[DefPath]) -> io::Result<SourceMap> {
        let items: Vec<&DefPath> = items.iter().collect();
        self.source_map(&self.source_rows(&items)?)
    }

    /// The rows of a link's items: those of `prior`'s items at `recorded`
    /// (path indices) borrowed from the record where the record's place still
    /// holds, the rest and `items` found. A recorded place holds when its file
    /// is the image's, which the record's key names, or when the request's
    /// files are the record's (`same_files`), which fixes every module and
    /// offset. Each distinct item, in path order.
    pub(crate) fn link_rows<'p>(
        &self,
        prior: Option<&'p crate::reach::Reach>,
        recorded: &[u32],
        items: &[&'p DefPath],
        same_files: bool,
    ) -> io::Result<Vec<SourceRow<'p>>> {
        let mut rest = items.to_vec();
        let mut rows = Vec::with_capacity(recorded.len());
        if let Some(prior) = prior {
            let request: BTreeSet<ModuleId> = self.files.iter().map(|f| f.module).collect();
            for &i in recorded {
                let path = &prior.paths[i as usize];
                match prior.places[i as usize] {
                    Some((module, file_offset)) if same_files || !request.contains(&module) => rows
                        .push(SourceRow {
                            path,
                            module,
                            file_offset,
                        }),
                    _ => rest.push(path),
                }
            }
        }
        rows.extend(self.source_rows(&rest)?);
        rows.sort_unstable_by(|a, b| a.path.cmp(b.path));
        rows.dedup_by(|a, b| a.path == b.path);
        Ok(rows)
    }

    /// Each distinct item, in path order, with its module and file offset.
    pub(crate) fn source_rows<'p>(&self, items: &[&'p DefPath]) -> io::Result<Vec<SourceRow<'p>>> {
        hirsel_core_ids::published::catch_io(|| {
            let mut items = items.to_vec();
            items.sort_unstable();
            items.dedup();
            items
                .into_iter()
                .map(|path| {
                    let header = self.headers.get(path).ok_or_else(|| {
                        io::Error::other(format!("native source item is not indexed: {path}"))
                    })?;
                    let (module, item) = header.anchor;
                    let file_offset = match self.files.iter().find(|file| file.module == module) {
                        Some(file) => file.syntax.items[item as usize].file_offset,
                        None => self
                            .image
                            .as_ref()
                            .ok_or_else(|| io::Error::other("native source has no package image"))?
                            .file_offset(module, item)?,
                    };
                    Ok(SourceRow {
                        path,
                        module,
                        file_offset,
                    })
                })
                .collect()
        })
    }

    /// The source map of `rows`, files numbered by first use.
    pub(crate) fn source_map(&self, rows: &[SourceRow<'_>]) -> io::Result<SourceMap> {
        hirsel_core_ids::published::catch_io(|| {
            let mut result = SourceMap {
                files: vec![],
                items: BTreeMap::new(),
            };
            let mut files = BTreeMap::new();
            let mut items = Vec::with_capacity(rows.len());
            for row in rows {
                let file = if let Some(&file) = files.get(&row.module) {
                    file
                } else {
                    let text = match self.files.iter().find(|file| file.module == row.module) {
                        Some(file) => file.syntax.source.clone(),
                        None => self.image.as_ref().unwrap().source_text(row.module)?,
                    };
                    let file = u32::try_from(result.files.len())
                        .map_err(|_| io::Error::other("native source file table exceeds u32"))?;
                    result.files.push(SourceFile {
                        path: self.source_name(row.module),
                        text,
                    });
                    files.insert(row.module, file);
                    file
                };
                items.push((
                    row.path.clone(),
                    ItemSource {
                        file,
                        file_offset: row.file_offset,
                    },
                ));
            }
            // Rows are in path order and distinct: the map is built in bulk.
            result.items = items.into_iter().collect();
            Ok(result)
        })
    }

    /// What an executable calls one source file: its package and its place in
    /// the package (`std@1/list.hirsel`), which the module path is. Never a host
    /// path, so an executable does not depend on where the core product is
    /// installed or where the project lives.
    pub(crate) fn source_name(&self, module: ModuleId) -> String {
        let path = self.index.module_path(module);
        format!(
            "{}@{}/{}.hirsel",
            path.package.name,
            path.package.version,
            path.modules.join("/")
        )
    }
}

/// One native item's place in the source snapshot.
pub(crate) struct SourceRow<'p> {
    pub(crate) path: &'p DefPath,
    pub(crate) module: ModuleId,
    pub(crate) file_offset: u32,
}
