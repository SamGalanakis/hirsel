use crate::package_content::{Id, Root};
use crate::package_manifest::SourceLocation;
use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

pub(super) fn binding(
    input: &[u8],
    id: &Id,
    root: Root,
    source: Option<&SourceLocation>,
) -> Result<String, super::Failure> {
    let text = std::str::from_utf8(input)?;
    let mut document = text.parse::<DocumentMut>()?;
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
    let dependencies = table
        .get_mut("dependencies")
        .and_then(Item::as_table_like_mut)
        .ok_or("dependencies is not a table")?;
    let fields = [
        ("major", Some(Value::from(i64::from(id.major)))),
        ("root", Some(Value::from(root.to_string()))),
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
        ("path", None),
        ("workspace", None),
    ];
    if let Some(existing) = dependencies
        .get_mut(&id.name)
        .and_then(Item::as_table_like_mut)
    {
        set_fields(existing, fields);
    } else {
        let sorted = dependencies.iter().map(|(key, _)| key).is_sorted();
        let mut binding = InlineTable::new();
        set_fields(&mut binding, fields);
        dependencies.insert(&id.name, Item::Value(Value::InlineTable(binding)));
        if sorted {
            dependencies.sort_values();
        }
    }
    Ok(document.to_string())
}

fn set_fields(table: &mut dyn TableLike, fields: [(&str, Option<Value>); 6]) {
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
