//! Static views and source bindings use the same ELF writer as generated code.
use super::*;
use hirsel_core_layout::source_positions;
use std::mem::offset_of;

pub(super) struct Tables {
    pub parameters: DataId,
    pub names: DataId,
    pub directories: DataId,
    pub net_rules: DataId,
    pub secret_grants: DataId,
    pub env_patterns: DataId,
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
    mut bytes: Vec<u8>,
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
    object.define_data(id, &data).map_err(error)?;
    Ok(id)
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
    for rule in plan.net_rules {
        let mut row = [0; size_of::<RuleView>()];
        let (view, patterns) = match rule {
            GrantRule::Address(address) => (RuleView::address(*address), None),
            GrantRule::Tls(rule) => {
                let patterns = rule.pattern_text();
                (RuleView::tls(rule, &patterns), Some(patterns))
            }
            GrantRule::Resolve(rule) => {
                let patterns = rule.pattern_text();
                (RuleView::resolve(&patterns), Some(patterns))
            }
        };
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
            .flat_map(|grant| grant.map_or([""; 3], |g| [&g.parameter, &g.source, &g.patterns])),
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
    let files = bytes(object, None, source_files(sources)?, 8)?;
    // The versioned symbol binds the source layout to the supplied runtime.
    let decoder = object
        .declare_data(
            if sources.points.is_empty() {
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
    table[8..16].copy_from_slice(&(sources.files.len() as u64).to_le_bytes());
    let sources = define(object, None, table, 8, &[(0, files), (16, decoder)])?;
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
        sources,
    })
}

/// One 40-byte row per file: path and source views, then the file's first site word.
fn source_files(sources: &hirsel_core_ir::source::SourceMap) -> Result<Vec<u8>> {
    let mut files = vec![0; 40 * sources.files.len()];
    for ((row, file), start) in sources.files.iter().enumerate().zip(sources.site_starts()?) {
        files[40 * row + 32..40 * row + 40].copy_from_slice(&start.to_le_bytes());
        let positions = match sources.points.get(&file.path) {
            Some(points) => source_positions::encode_points(points),
            None => source_positions::encode(&file.text),
        };
        for (view, content) in [file.path.as_bytes(), &positions].into_iter().enumerate() {
            let field = 40 * row + 16 * view;
            let offset = files.len() as u64;
            files[field..field + 8].copy_from_slice(&offset.to_le_bytes());
            files[field + 8..field + 16].copy_from_slice(&(content.len() as u64).to_le_bytes());
            files.extend_from_slice(content);
        }
    }
    Ok(files)
}
