//! Static views and source bindings use the same ELF writer as generated code.
use super::*;

pub(super) struct Tables {
    pub parameters: DataId,
    pub names: DataId,
    pub directories: DataId,
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
pub(super) fn write(object: &mut ObjectModule, plan: &Plan<'_>) -> Result<Tables> {
    let mut requirements = Vec::new();
    for p in &plan.parameters {
        requirements.extend_from_slice(&u64::from(p.kind).to_le_bytes());
        requirements.extend_from_slice(&p.index.to_le_bytes());
    }
    let parameters = bytes(object, None, requirements, 8)?;
    let names = views(object, plan.parameters.iter().map(|p| p.name))?;
    let directories = views(object, plan.directories.iter().map(String::as_str))?;
    let mut files = Vec::new();
    let mut pointers = Vec::new();
    for file in &plan.sources.files {
        for string in [&file.path, &file.text] {
            let data = bytes(object, None, string.as_bytes().to_vec(), 1)?;
            pointers.push((files.len() as u32, data));
            files.extend_from_slice(&0u64.to_le_bytes());
            files.extend_from_slice(&(string.len() as u64).to_le_bytes());
        }
    }
    let files = define(object, None, files, 8, &pointers)?;
    let mut items = Vec::new();
    for (i, (path, item)) in plan.sources.items.iter().enumerate() {
        bytes(
            object,
            Some(&hirsel_core_ir::source::site_symbol(path)),
            ((i as u64 + 1) << 32).to_le_bytes().to_vec(),
            8,
        )?;
        items.extend_from_slice(&item.file.to_le_bytes());
        items.extend_from_slice(&item.file_offset.to_le_bytes());
    }
    let items = bytes(object, None, items, 4)?;
    let mut table = vec![0; 32];
    table[8..16].copy_from_slice(&(plan.sources.files.len() as u64).to_le_bytes());
    table[24..32].copy_from_slice(&(plan.sources.items.len() as u64).to_le_bytes());
    let sources = define(object, None, table, 8, &[(0, files), (16, items)])?;
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
        sources,
    })
}
