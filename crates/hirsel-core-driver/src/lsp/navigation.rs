use super::documents::{Encoding, Text, file_uri};
use hirsel_core_syntax::{Item, Span};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub(super) struct Location {
    pub(super) path: PathBuf,
    pub(super) selection: Span,
    pub(super) declaration: Span,
}

pub(super) fn definition(
    from: &Text,
    span: Span,
    target: &Location,
    origins: &BTreeMap<PathBuf, Text>,
    immutable: &BTreeSet<PathBuf>,
    links: bool,
    encoding: Encoding,
) -> Option<Value> {
    let text = origins.get(&target.path)?;
    let selection = range(text, target.selection, encoding)?;
    let declaration = range(text, target.declaration, encoding)?;
    if immutable.contains(&target.path)
        && let Err(error) = materialize(&target.path, text)
    {
        eprintln!("hirsel lsp: {error}");
        return None;
    }
    let uri = file_uri(&target.path);
    Some(if links {
        json!([{
            "originSelectionRange": range(from, span, encoding)?,
            "targetUri": uri,
            "targetRange": declaration,
            "targetSelectionRange": selection,
        }])
    } else {
        json!([{"uri": uri, "range": selection}])
    })
}

fn materialize(path: &Path, text: &Text) -> io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(path)
    {
        Ok(mut file) => {
            if let Err(error) = file.write_all(&text.bytes) {
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if std::fs::read(path)? == text.bytes {
                Ok(())
            } else {
                Err(io::Error::other(
                    "editor source materialization was modified",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) fn range(text: &Text, span: Span, encoding: Encoding) -> Option<Value> {
    Some(json!({
        "start": text.position(span.start, encoding)?,
        "end": text.position(span.end, encoding)?,
    }))
}

pub(super) fn absolute(item: Item<'_>, span: Span) -> Span {
    Span {
        start: item.file_offset + span.start,
        end: item.file_offset + span.end,
    }
}
