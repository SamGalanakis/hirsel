//! Reached-only heads: which installed files a request adopts. Name probes
//! (candidates, absence, shadowing, help) read each image's whole name index
//! (N) through the scope tables, so only a file whose definitions the request
//! may demand needs its parse heads. The seed is every installed name the
//! program's own text spells as a word, every instance-bodied or generated
//! name (whole-package scans read their facts), and every convention (K);
//! the closure follows the image's edges (E) and, since a file is adopted
//! whole, every name of each reached file. An image without N is adopted
//! whole, and so is every image of a program that generates bodies. A demand
//! outside the reached files is an explicit internal error
//! (`QueryEngine::materialize`), never a missing source name.
use hirsel_engine_store::InstalledPackage;
use hirsel_span::{FileId, SourceMap};
use hirsel_syntax::{DefId, DefPath, FileContext, PackageId};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

/// The files a request adopts, and the installed names whose files it does
/// not: only those may be declared without heads.
pub(crate) struct Reach {
    pub files: BTreeSet<FileId>,
    pub unreached: Arc<BTreeSet<DefId>>,
}

pub(crate) fn reached_files(
    images: &BTreeMap<PackageId, Arc<InstalledPackage>>,
    files: &[(FileId, FileContext)],
    sources: &SourceMap,
) -> Reach {
    let mut words = BTreeSet::<&str>::new();
    for (file, context) in files {
        if images.contains_key(&context.package) {
            continue;
        }
        if let Some(source) = sources.file(*file) {
            words.extend(
                source
                    .text()
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .filter(|word| !word.is_empty()),
            );
        }
    }
    // A program's `= comptime` bodies generate text only during the request,
    // and that text names installed definitions the program's own words do
    // not: such a program adopts every image whole.
    let generates = words.contains("comptime");
    let mut whole = BTreeSet::new();
    let mut source_of = BTreeMap::<DefPath, u32>::new();
    // Adoption is per file: a reached file adopts, and may check, every one
    // of its definitions, so reaching a name reaches its file's names.
    let mut in_source = BTreeMap::<(&PackageId, u32), Vec<DefPath>>::new();
    let mut work = Vec::new();
    for (package, image) in images {
        let Some(names) = image.names().filter(|_| !generates) else {
            whole.insert(package);
            continue;
        };
        for (path, facts) in names.paths(package) {
            if facts.instance_bodied
                || path.0.name.contains('$')
                || words.contains(path.0.name.as_str())
            {
                work.push(path.clone());
            }
            in_source
                .entry((package, facts.source))
                .or_default()
                .push(path.clone());
            source_of.insert(path, facts.source);
        }
        work.extend(image.conventions().into_iter().flatten().map(|c| c.path()));
    }
    let mut reached = BTreeSet::new();
    let mut reached_sources = BTreeSet::new();
    while let Some(path) = work.pop() {
        if !reached.insert(path.clone()) {
            continue;
        }
        if let Some((package, _)) = images.get_key_value(&path.0.package)
            && let Some(&source) = source_of.get(&path)
            && reached_sources.insert((package, source))
        {
            work.extend(
                in_source[&(package, source)]
                    .iter()
                    .filter(|name| !reached.contains(*name))
                    .cloned(),
            );
        }
        if let Some(edges) = images.get(&path.0.package).and_then(|image| image.edges()) {
            work.extend(
                edges
                    .targets(&path)
                    .filter(|target| !reached.contains(*target))
                    .cloned(),
            );
        }
    }
    let file_of = |path: &DefPath| {
        let source = *source_of.get(path)?;
        let (package, image) = images.get_key_value(&path.0.package)?;
        Some((
            package,
            image.sources.get(source as usize)?.logical.as_path(),
        ))
    };
    let logical: BTreeSet<(&PackageId, &Path)> = reached.iter().filter_map(file_of).collect();
    let unreached = Arc::new(
        source_of
            .keys()
            .filter(|path| file_of(path).is_none_or(|file| !logical.contains(&file)))
            .map(DefId::intern)
            .collect(),
    );
    let files = files
        .iter()
        .filter(|(file, context)| {
            images.contains_key(&context.package)
                && (whole.contains(&context.package)
                    || sources.file(*file).is_some_and(|source| {
                        logical.contains(&(&context.package, source.logical_path()))
                    }))
        })
        .map(|(file, _)| *file)
        .collect();
    Reach { files, unreached }
}
