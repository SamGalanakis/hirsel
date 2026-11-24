use crate::package_content::{Id, Root};
use crate::package_manifest::SourceLocation;
use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

type Fields = [(&'static str, Option<Value>); 6];

/// What a binding names beside its major.
pub(super) enum Form<'a> {
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
    [
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
        ("path", path.map(Value::from)),
        ("workspace", None),
    ]
}

fn inherited() -> Fields {
    [
        ("major", None),
        ("root", None),
        ("url", None),
        ("archive", None),
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

/// Binds `id` in the manifest's own `[dependencies]`.
pub(super) fn binding(input: &[u8], id: &Id, form: &Form<'_>) -> Result<String, super::Failure> {
    let mut document = document(input)?;
    place(dependencies(&mut document)?, &id.name, published(id, form));
    Ok(document.to_string())
}

/// Declares `id` in the root's `[workspace.dependencies]`, any path spelled
/// relative to the root, and with `inherit` also binds it in the root
/// package's own `[dependencies]` as `{ workspace = true }`.
pub(super) fn shared(
    input: &[u8],
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
    place(declared, &id.name, published(id, form));
    if inherit {
        place(dependencies(&mut document)?, &id.name, inherited());
    }
    Ok(document.to_string())
}

/// Binds `name` in the manifest's `[dependencies]` as `{ workspace = true }`.
pub(super) fn inherit(input: &[u8], name: &str) -> Result<String, super::Failure> {
    let mut document = document(input)?;
    place(dependencies(&mut document)?, name, inherited());
    Ok(document.to_string())
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
