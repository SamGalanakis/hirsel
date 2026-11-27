//! The reexport install names in Apple's YAML text stubs.
use std::io;

pub(super) fn reexports(text: &str) -> io::Result<Vec<String>> {
    let refuse = || io::Error::other("link recipe refused: unsupported Mach-O text stub reexports");
    if !text.starts_with("--- !tapi-tbd") && !text.starts_with("---\n") {
        return Err(refuse());
    }
    let mut result = Vec::new();
    let mut lines = text.lines().peekable();
    let mut libraries = false;
    while let Some(line) = lines.next() {
        let field = line.trim_start();
        if line == field {
            libraries = field.starts_with("reexported-libraries:");
            if libraries && field != "reexported-libraries:" {
                return Err(refuse());
            }
        }
        let Some(names) = (if libraries {
            field.strip_prefix("libraries:")
        } else {
            field.strip_prefix("re-exports:")
        }) else {
            continue;
        };
        let mut list = names
            .trim_start()
            .strip_prefix('[')
            .ok_or_else(refuse)?
            .to_owned();
        while !list.contains(']') {
            let next = lines
                .next()
                .filter(|line| line.starts_with(' '))
                .ok_or_else(refuse)?;
            list.push_str(next.trim());
        }
        let (names, tail) = list.split_once(']').ok_or_else(refuse)?;
        if !tail.trim().is_empty() {
            return Err(refuse());
        }
        for name in names
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            result.push(name.trim_matches(['\'', '"']).to_owned());
        }
    }
    Ok(result)
}
