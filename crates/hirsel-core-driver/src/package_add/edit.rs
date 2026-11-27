use crate::package_content::{Id, Root};
use crate::package_manifest::SourceLocation;
use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

type Fields = [(&'static str, Option<Value>); 11];

/// What a binding names beside its major.
pub(crate) enum Form<'a> {
    Published(Root, Option<&'a SourceLocation>),
    Toolchain,
    Member(&'a str),
}

fn published(id: &Id, form: &Form<'_>) -> Fields {
    let (root, source, path) = match form {
        Form::Published(root, source) => (Some(*root), *source, None),
        Form::Toolchain => (None, None, None),
        Form::Member(path) => (None, None, Some(*path)),
    };
    let git = match source {
        Some(SourceLocation::Git(git)) => Some(git),
        _ => None,
    };
    // A toolchain binding is the one form without a package (packages.md section 2).
    let package = match form {
        Form::Toolchain => None,
        _ => Some(Value::from(id.name.as_str())),
    };
    [
        ("package", package),
        ("major", Some(Value::from(i64::from(id.major)))),
        ("root", root.map(|root| Value::from(root.to_string()))),
        (
            "url",
            match source {
                Some(SourceLocation::Url(url)) => Some(Value::from(url.as_str())),
                _ => None,
            },
        ),
        (
            "archive",
            match source {
                Some(SourceLocation::Archive(path)) => Some(Value::from(path.as_str())),
                _ => None,
            },
        ),
        ("git", git.map(|git| Value::from(git.url.as_str()))),
        ("rev", git.map(|git| Value::from(git.rev.as_str()))),
        (
            "ref",
            git.and_then(|git| git.reference.as_deref())
                .map(Value::from),
        ),
        (
            "dir",
            git.and_then(|git| git.dir.as_deref()).map(Value::from),
        ),
        ("path", path.map(Value::from)),
        ("workspace", None),
    ]
}

fn inherited() -> Fields {
    [
        ("package", None),
        ("major", None),
        ("root", None),
        ("url", None),
        ("archive", None),
        ("git", None),
        ("rev", None),
        ("ref", None),
        ("dir", None),
        ("path", None),
        ("workspace", Some(Value::from(true))),
    ]
}

fn document(input: &[u8]) -> Result<DocumentMut, super::Failure> {
    Ok(std::str::from_utf8(input)?.parse::<DocumentMut>()?)
}

fn dependencies(document: &mut DocumentMut) -> Result<&mut dyn TableLike, super::Failure> {
    let table = document.as_table_mut();
    if !table.contains_key("dependencies") {
        let mut dependencies = Table::new();
        if let Some(position) = table
            .get("package")
            .and_then(Item::as_table)
            .and_then(Table::position)
        {
            dependencies.set_position(position);
        }
        if table.get("package").is_some_and(Item::is_table) {
            dependencies.decor_mut().set_prefix("\n");
        }
        table.insert("dependencies", Item::Table(dependencies));
    }
    Ok(table
        .get_mut("dependencies")
        .and_then(Item::as_table_like_mut)
        .ok_or("dependencies is not a table")?)
}

fn place(dependencies: &mut dyn TableLike, name: &str, fields: Fields) {
    if let Some(existing) = dependencies.get_mut(name).and_then(Item::as_table_like_mut) {
        set_fields(existing, fields);
    } else {
        let sorted = dependencies.iter().map(|(key, _)| key).is_sorted();
        let mut binding = InlineTable::new();
        set_fields(&mut binding, fields);
        dependencies.insert(name, Item::Value(Value::InlineTable(binding)));
        if sorted {
            dependencies.sort_values();
        }
    }
}

/// Binds `id` under `alias` in the manifest's own `[dependencies]`.
pub(crate) fn binding(
    input: &[u8],
    alias: &str,
    id: &Id,
    form: &Form<'_>,
) -> Result<String, super::Failure> {
    let mut document = document(input)?;
    place(dependencies(&mut document)?, alias, published(id, form));
    Ok(document.to_string())
}

/// Declares `id` in the root's `[workspace.dependencies]`, any path spelled
/// relative to the root, and with `inherit` also binds it in the root
/// package's own `[dependencies]` as `{ workspace = true }`.
pub(crate) fn shared(
    input: &[u8],
    alias: &str,
    id: &Id,
    form: &Form<'_>,
    inherit: bool,
) -> Result<String, super::Failure> {
    let mut document = document(input)?;
    let workspace = document
        .as_table_mut()
        .get_mut("workspace")
        .and_then(Item::as_table_like_mut)
        .ok_or("workspace is not a table")?;
    if !workspace.contains_key("dependencies") {
        let mut dependencies = Table::new();
        dependencies.decor_mut().set_prefix("\n");
        workspace.insert("dependencies", Item::Table(dependencies));
    }
    let declared = workspace
        .get_mut("dependencies")
        .and_then(Item::as_table_like_mut)
        .ok_or("workspace.dependencies is not a table")?;
    place(declared, alias, published(id, form));
    if inherit {
        inherit_binding(&mut document, alias)?;
    }
    Ok(document.to_string())
}

/// Binds `alias` in the manifest's `[dependencies]` as `{ workspace = true }`.
pub(super) fn inherit(input: &[u8], alias: &str) -> Result<String, super::Failure> {
    let mut document = document(input)?;
    inherit_binding(&mut document, alias)?;
    Ok(document.to_string())
}

fn inherit_binding(document: &mut DocumentMut, alias: &str) -> Result<(), super::Failure> {
    let dependencies = dependencies(document)?;
    if let Some(binding) = dependencies
        .get_mut(alias)
        .and_then(Item::as_table_like_mut)
    {
        binding.remove("authority");
    }
    place(dependencies, alias, inherited());
    Ok(())
}

fn set_fields(table: &mut dyn TableLike, fields: Fields) {
    for (key, value) in fields {
        if let Some(mut value) = value {
            if let Some(old) = table.get_mut(key).and_then(Item::as_value_mut) {
                if old
                    .as_integer()
                    .zip(value.as_integer())
                    .is_some_and(|(left, right)| left == right)
                    || old
                        .as_str()
                        .zip(value.as_str())
                        .is_some_and(|(left, right)| left == right)
                    || old
                        .as_bool()
                        .zip(value.as_bool())
                        .is_some_and(|(left, right)| left == right)
                {
                    continue;
                }
                *value.decor_mut() = old.decor().clone();
                *old = value;
            } else {
                table.insert(key, Item::Value(value));
            }
        } else {
            table.remove(key);
        }
    }
}

pub(crate) fn selections(
    input: &[u8],
    before: &crate::package_manifest::Selections,
    after: &crate::package_manifest::Selections,
) -> Result<String, super::Failure> {
    let mut document = document(input)?;
    if before == after {
        return Ok(document.to_string());
    }
    let table = document.as_table_mut();
    if !table.contains_key("selections") {
        let mut selections = Table::new();
        selections.decor_mut().set_prefix("\n");
        table.insert("selections", Item::Table(selections));
    }
    let selections = table
        .get_mut("selections")
        .and_then(Item::as_table_like_mut)
        .ok_or("selections is not a table")?;
    for id in before.keys().filter(|id| !after.contains_key(*id)) {
        selections.remove(&id.to_string());
    }
    for (id, (root, locator)) in after {
        if before.get(id) != after.get(id) {
            let mut fields = published(id, &Form::Published(*root, locator.as_ref()));
            fields[0].1 = None;
            fields[1].1 = None;
            place(selections, &id.to_string(), fields);
        }
    }
    if selections.is_empty() {
        table.remove("selections");
    }
    Ok(document.to_string())
}
