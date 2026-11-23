//! Static views and source bindings use the same ELF writer as generated code.
use super::*;
use hirsel_core_layout::source_positions;
use std::collections::BTreeSet;
use std::mem::offset_of;

const SOURCE_FILES: &str = "__hirsel_source_files";
const SOURCE_ROW_BYTES: usize = 40;

pub(super) struct Tables {
    pub parameters: DataId,
    pub names: DataId,
    pub directories: DataId,
    pub net_rules: DataId,
    pub secret_grants: DataId,
    pub env_patterns: DataId,
    pub exec_bindings: DataId,
    pub sources: DataId,
}

pub(super) fn bytes(
    object: &mut ObjectModule,
    name: Option<&str>,
    bytes: Vec<u8>,
    align: u64,
) -> Result<DataId> {
    define(object, name, bytes, align, &[])
}
/// A Bytes `{bytes, 32, 0}` over `revision`, exported as `symbol`.
pub(super) fn carrier(
    object: &mut ObjectModule,
    symbol: &str,
    revision: &[u8; 32],
) -> Result<DataId> {
    let bytes = bytes(object, None, revision.to_vec(), 1)?;
    let mut row = vec![0; 24];
    row[8..16].copy_from_slice(&32u64.to_le_bytes());
    define(object, Some(symbol), row, 8, &[(0, bytes)])
}
fn define(
    object: &mut ObjectModule,
    name: Option<&str>,
    bytes: Vec<u8>,
    align: u64,
    pointers: &[(u32, DataId)],
) -> Result<DataId> {
    let local;
    let (name, linkage) = if let Some(name) = name {
        (name, Linkage::Export)
    } else {
        local = format!(
            "__hirsel_data_{}",
            object.declarations().get_data_objects().count()
        );
        (local.as_str(), Linkage::Local)
    };
    let id = object
        .declare_data(name, linkage, false, false)
        .map_err(error)?;
    define_contents(object, id, bytes, align, pointers)?;
    Ok(id)
}

fn define_contents(
    object: &mut ObjectModule,
    id: DataId,
    mut bytes: Vec<u8>,
    align: u64,
    pointers: &[(u32, DataId)],
) -> Result<()> {
    if bytes.is_empty() {
        bytes.resize(8, 0);
    }
    let mut data = DataDescription::new();
    data.define(bytes.into_boxed_slice());
    data.set_align(align);
    for &(offset, target) in pointers {
        let reference = object.declare_data_in_data(target, &mut data);
        data.write_data_addr(offset, reference, 0);
    }
    object.define_data(id, &data).map_err(error)
}
fn views<'a>(object: &mut ObjectModule, strings: impl Iterator<Item = &'a str>) -> Result<DataId> {
    let mut rows = Vec::new();
    let mut pointers = Vec::new();
    for string in strings {
        let data = bytes(object, None, string.as_bytes().to_vec(), 1)?;
        pointers.push((rows.len() as u32, data));
        rows.extend_from_slice(&0u64.to_le_bytes());
        rows.extend_from_slice(&(string.len() as u64).to_le_bytes());
    }
    define(object, None, rows, 8, &pointers)
}
pub(super) fn write(
    object: &mut ObjectModule,
    plan: &Plan<'_>,
    sources: &hirsel_core_ir::source::SourceMap,
    live_sites: Option<&BTreeSet<u64>>,
) -> Result<Tables> {
    let mut requirements = Vec::new();
    for p in &plan.parameters {
        requirements.extend_from_slice(&u64::from(p.kind).to_le_bytes());
        requirements.extend_from_slice(&p.index.to_le_bytes());
    }
    let parameters = bytes(object, None, requirements, 8)?;
    let names = views(object, plan.parameters.iter().map(|p| p.name))?;
    let directories = views(object, plan.directories.iter().map(String::as_str))?;
    use hirsel_core_net::{GrantRule, RuleView};
    let mut rules = Vec::new();
    let mut pointers = Vec::new();
    let rows = plan.net_rules.iter().map(|rule| match rule {
        GrantRule::Address(address) => (RuleView::address(*address), None),
        GrantRule::Tls(rule) => {
            let patterns = rule.pattern_text();
            (RuleView::tls(rule, &patterns), Some(patterns))
        }
        GrantRule::Resolve(rule) => {
            let patterns = rule.pattern_text();
            (RuleView::resolve(&patterns), Some(patterns))
        }
        GrantRule::Unix(rule) => {
            let (label, path) = rule
                .endpoint()
                .expect("launch rule has a concrete endpoint");
            let text = [label, path].concat();
            (RuleView::unix(rule, &text), Some(text))
        }
    });
    let roots = plan.unix_roots.iter().map(|(label, path)| {
        let text = [label.as_str(), path.as_str()].concat();
        (RuleView::unix_root(label.len(), &text), Some(text))
    });
    for (view, patterns) in roots.chain(rows) {
        let mut row = [0; size_of::<RuleView>()];
        if let Some(patterns) = patterns {
            let offset = rules.len() + offset_of!(RuleView, patterns);
            let data = bytes(object, None, patterns.into_bytes(), 1)?;
            pointers.push((u32::try_from(offset).map_err(error)?, data));
        }
        let address = view.address;
        row[0] = address.dir;
        row[1] = address.family;
        row[8..16].copy_from_slice(&address.hi.to_le_bytes());
        row[16..24].copy_from_slice(&address.lo.to_le_bytes());
        row[24] = address.bits;
        row[26..28].copy_from_slice(&address.first.to_le_bytes());
        row[28..30].copy_from_slice(&address.last.to_le_bytes());
        let len = offset_of!(RuleView, patterns_len);
        row[len..len + 8].copy_from_slice(&(view.patterns_len as u64).to_le_bytes());
        row[offset_of!(RuleView, trust)] = view.trust;
        let roots = offset_of!(RuleView, roots);
        row[roots..roots + 32].copy_from_slice(&view.roots);
        rules.extend(row);
    }
    let net_rules = define(object, None, rules, 8, &pointers)?;
    let secret_grants = views(
        object,
        plan.secret_grants
            .iter()
            .flat_map(|grant| grant.map_or([""; 3], |g| [&g.parameter, &g.source, &g.policy])),
    )?;
    let env_patterns = bytes(
        object,
        None,
        plan.env_patterns
            .as_deref()
            .unwrap_or("")
            .as_bytes()
            .to_vec(),
        1,
    )?;
    let mut exec_rows = Vec::new();
    let mut exec_pointers = Vec::new();
    for (request, workdir, digest) in &plan.exec {
        let text = bytes(object, None, request.as_bytes().to_vec(), 1)?;
        exec_pointers.push((u32::try_from(exec_rows.len()).map_err(error)?, text));
        exec_rows.extend_from_slice(&0u64.to_le_bytes());
        exec_rows.extend_from_slice(&(request.len() as u64).to_le_bytes());
        let base = bytes(object, None, workdir.clone(), 1)?;
        exec_pointers.push((u32::try_from(exec_rows.len()).map_err(error)?, base));
        exec_rows.extend_from_slice(&0u64.to_le_bytes());
        exec_rows.extend_from_slice(&(workdir.len() as u64).to_le_bytes());
        exec_rows.extend_from_slice(digest);
    }
    let exec_bindings = define(object, None, exec_rows, 8, &exec_pointers)?;
    let (files, file_count) = source_files(sources, live_sites)?;
    let files_id = object
        .declare_data(SOURCE_FILES, Linkage::Local, false, false)
        .map_err(error)?;
    define_contents(object, files_id, files, 8, &[])?;
    // The decoder symbol binds the generated entry and Context ABI to the runtime.
    let decoder = object
        .declare_data(
            if live_sites.is_none() && sources.points.is_empty() {
                source_positions::TABLE_SYMBOL
            } else {
                source_positions::POINTS_TABLE_SYMBOL
            },
            Linkage::Import,
            false,
            false,
        )
        .map_err(error)?;
    let mut table = vec![0; 24];
    table[8..16].copy_from_slice(&(file_count as u64).to_le_bytes());
    let sources = define(object, None, table, 8, &[(0, files_id), (16, decoder)])?;
    if plan.charge_report.is_some() {
        let mut label = DataDescription::new();
        label.define(
            b"Hirsel observation build: timings are not performance samples\0"
                .to_vec()
                .into_boxed_slice(),
        );
        label.used = true;
        let id = object
            .declare_data("__hirsel_observation_build", Linkage::Local, false, false)
            .map_err(error)?;
        object.define_data(id, &label).map_err(error)?;
    }
    Ok(Tables {
        parameters,
        names,
        directories,
        net_rules,
        secret_grants,
        env_patterns,
        exec_bindings,
        sources,
    })
}

/// One 40-byte row per retained file. File starts use the original site space.
fn source_files(
    sources: &SourceMap,
    live_sites: Option<&BTreeSet<u64>>,
) -> Result<(Vec<u8>, usize)> {
    if live_sites.is_some_and(BTreeSet::is_empty) {
        return Ok((Vec::new(), 0));
    }
    let starts = sources.site_starts()?;
    let positions = live_sites.map(|_| sources.positions());
    let mut rows = Vec::new();
    for (index, (file, &start)) in sources.files.iter().zip(&starts).enumerate() {
        let positions = match (live_sites, &positions, sources.points.get(&file.path)) {
            (Some(live), Some(positions), _) => {
                let end = starts
                    .get(index + 1)
                    .copied()
                    .unwrap_or(source_positions::SITE_END);
                let mut points = Vec::new();
                for &site in live.range(start..end) {
                    let ordinal = (site - start) as u32;
                    let point = positions
                        .file_position(index, ordinal as usize)
                        .ok_or_else(|| format!("linked site {site} is outside {}", file.path))?;
                    points.push((ordinal, point));
                }
                if points.is_empty() {
                    continue;
                }
                source_positions::encode_points(&points)
            }
            (_, _, Some(points)) => source_positions::encode_points(
                &points
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(i, point)| (i as u32, point))
                    .collect::<Vec<_>>(),
            ),
            _ => source_positions::encode(&file.text),
        };
        rows.push((&file.path, start, positions));
    }
    let count = rows.len();
    let mut files = vec![0; SOURCE_ROW_BYTES * count];
    for (row, (path, start, positions)) in rows.into_iter().enumerate() {
        let row = SOURCE_ROW_BYTES * row;
        files[row + 32..row + SOURCE_ROW_BYTES].copy_from_slice(&start.to_le_bytes());
        for (view, content) in [path.as_bytes(), &positions].into_iter().enumerate() {
            let field = row + 16 * view;
            let offset = files.len() as u64;
            files[field..field + 8].copy_from_slice(&offset.to_le_bytes());
            files[field + 8..field + 16].copy_from_slice(&(content.len() as u64).to_le_bytes());
            files.extend_from_slice(content);
        }
    }
    Ok((files, count))
}

/// Resolve a corpus report's native word against the same file and coordinates
/// the linked program uses for failure diagnostics.
pub(crate) fn source_site(binary: &std::path::Path, site: u64) -> Option<String> {
    use object::{Object as _, ObjectSection as _, ObjectSymbol as _, read::ReadCache};

    if !(1..source_positions::SITE_END).contains(&site) {
        return None;
    }
    let file = ReadCache::new(std::fs::File::open(binary).ok()?);
    let elf = object::File::parse(&file).ok()?;
    let files = elf.symbol_by_name(SOURCE_FILES)?;
    let section = elf.section_by_index(files.section_index()?).ok()?;
    let data = section.data_range(files.address(), files.size()).ok()??;
    let word = |row: &[u8], at| u64::from_le_bytes(row[at..at + 8].try_into().unwrap());
    // The first path starts immediately after the file rows.
    let rows_end = word(data.get(..SOURCE_ROW_BYTES)?, 0) as usize;
    let (rows, _) = data.get(..rows_end)?.as_chunks::<SOURCE_ROW_BYTES>();
    let file = rows
        .partition_point(|row| word(row, 32) <= site)
        .checked_sub(1)?;
    let row = &rows[file];
    let view = |at| {
        let start = word(row, at) as usize;
        data.get(start..start.checked_add(word(row, at + 8) as usize)?)
    };
    let path = std::str::from_utf8(view(0)?).ok()?;
    let (tag, positions) = view(16)?.split_first()?;
    let offset = (site - word(row, 32)) as usize;
    let (line, column) = match *tag {
        source_positions::TAG => source_positions::position(positions, offset),
        source_positions::POINTS_TAG => source_positions::point_position(positions, offset)?,
        _ => return None,
    };
    Some(format!("{path}:{line}:{column}"))
}
