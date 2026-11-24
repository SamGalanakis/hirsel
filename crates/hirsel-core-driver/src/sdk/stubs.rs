//! Stub paths: where each exported entry appears in the generated bindings and
//! in `hirsel call` (docs/spec/sdk.md section 4).
use super::{Describe, Error};
use serde_json::json;
use std::collections::BTreeMap;

pub(crate) fn entry_path(identity: &[u8]) -> Result<&str, Error> {
    // PROG-9: version 01 followed by UTF-8 D A S. D ends at the first '[' or '('.
    if identity.first() != Some(&1) {
        return Err("Malformed: entry identity version".into());
    }
    let text = std::str::from_utf8(&identity[1..])?;
    let (path, _) = text
        .split_once(['[', '('])
        .ok_or("Malformed: entry signature")?;
    Ok(path)
}

/// An entry's stub path (docs/spec/sdk.md section 4): its module path components
/// and then its name, with the package dropped and each component percent-decoded.
pub(crate) fn stub_path(identity: &[u8]) -> Result<Vec<String>, Error> {
    let mut components = entry_path(identity)?.split('/');
    components.next();
    let path = components
        .map(|c| Ok(String::from_utf8(decode_identity_component(c)?)?))
        .collect::<Result<Vec<_>, Error>>()?;
    if path.len() < 2 {
        return Err("Malformed: definition identity".into());
    }
    Ok(path)
}

/// Every exported entry's stub path, in Describe order. Paths must be distinct
/// and none may be a proper prefix of another (a function and a namespace).
pub(crate) fn stub_paths(describe: &Describe) -> Result<Vec<Vec<String>>, Error> {
    let paths = describe
        .exported
        .iter()
        .map(|entry| stub_path(&entry.identity))
        .collect::<Result<Vec<_>, Error>>()?;
    let mut order: Vec<usize> = (0..paths.len()).collect();
    order.sort_by(|&a, &b| paths[a].cmp(&paths[b]));
    // In sorted order, a path that prefixes any later path prefixes the next one.
    for pair in order.windows(2) {
        if paths[pair[1]].starts_with(&paths[pair[0]]) {
            let (first, second) = (pair[0].min(pair[1]), pair[0].max(pair[1]));
            return Err(duplicate_stub_name(
                &describe.exported[first].identity,
                &describe.exported[second].identity,
            ));
        }
    }
    Ok(paths)
}

fn decode_identity_component(component: &str) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut bytes = component.bytes();
    while let Some(b) = bytes.next() {
        out.push(if b == b'%' {
            let hi = bytes.next().ok_or("Malformed: percent escape")?;
            let lo = bytes.next().ok_or("Malformed: percent escape")?;
            u8::from_str_radix(std::str::from_utf8(&[hi, lo])?, 16)?
        } else {
            b
        });
    }
    Ok(out)
}

fn duplicate_stub_name(first: &[u8], second: &[u8]) -> Error {
    fn component_text(component: &str) -> String {
        let Ok(bytes) = decode_identity_component(component) else {
            return component.to_owned();
        };
        let mut text = String::new();
        for chunk in bytes.utf8_chunks() {
            text.push_str(chunk.valid());
            for &byte in chunk.invalid() {
                text.extend(byte.escape_ascii().map(char::from));
            }
        }
        text
    }

    fn path_text(identity: &[u8]) -> String {
        let path = entry_path(identity).expect("stub path already validated");
        let (_, path) = path.split_once('/').unwrap_or(("", path));
        let Some((module, entry)) = path.rsplit_once('/') else {
            return component_text(path);
        };
        let module = module.split('/').map(component_text).collect::<Vec<_>>();
        format!("{}::{}", module.join("."), component_text(entry))
    }

    format!(
        "DuplicateStubName({}, {})",
        path_text(first),
        path_text(second)
    )
    .into()
}

/// The TypeScript type of one `bind` namespace: nested object types keyed by
/// stub path components.
#[derive(Default)]
pub(super) struct StubTypes(BTreeMap<String, StubType>);

enum StubType {
    Stub(String),
    Namespace(StubTypes),
}

impl StubTypes {
    pub(super) fn insert(&mut self, path: &[String], ty: String) {
        let (name, module) = path.split_last().expect("stub path has a module");
        let mut at = self;
        for component in module {
            at = match at
                .0
                .entry(component.clone())
                .or_insert_with(|| StubType::Namespace(StubTypes::default()))
            {
                StubType::Namespace(inner) => inner,
                StubType::Stub(_) => unreachable!("stub paths are prefix-free"),
            };
        }
        at.0.insert(name.clone(), StubType::Stub(ty));
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn render(&self) -> String {
        let members = self
            .0
            .iter()
            .map(|(key, member)| match member {
                StubType::Stub(ty) => format!("{}: {ty}", json!(key)),
                StubType::Namespace(inner) => format!("{}: {}", json!(key), inner.render()),
            })
            .collect::<Vec<_>>();
        format!("{{ {} }}", members.join("; "))
    }
}
