//! Build-time extraction of the specification's rule headings and first prose paragraphs.
use std::{collections::BTreeMap, fs, io, path::Path};

#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub spec_file: String,
    pub anchor: String,
    pub statement: String,
}
pub fn rule_id(text: &str) -> bool {
    text.split_once('-').is_some_and(|(prefix, number)| {
        !prefix.is_empty()
            && prefix.bytes().all(|b| b.is_ascii_uppercase())
            && !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
    })
}
pub fn anchor(heading: &str) -> String {
    heading
        .chars()
        .flat_map(char::to_lowercase)
        .filter_map(|c| {
            if c.is_whitespace() {
                Some('-')
            } else if c.is_alphanumeric() || matches!(c, '-' | '_') {
                Some(c)
            } else {
                None
            }
        })
        .collect()
}
pub fn extract(file: &str, text: &str) -> Result<Vec<Entry>, String> {
    let mut entries = vec![];
    let mut current: Option<Entry> = None;
    let mut fence = None;
    let mut paragraph = vec![];
    let mut complete = false;
    for line in text.lines().chain(std::iter::once("### end")) {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let marker = &trimmed[..3];
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if line.starts_with("### ") {
            if let Some(mut entry) = current.take() {
                entry.statement = paragraph.join(" ");
                if entry.statement.is_empty() {
                    return Err(format!("{file}: {} has no prose statement", entry.id));
                }
                entries.push(entry);
            }
            paragraph.clear();
            complete = false;
            if let Some((id, title)) = line[4..].split_once(": ").filter(|(id, _)| rule_id(id)) {
                current = Some(Entry {
                    id: id.into(),
                    title: title.into(),
                    spec_file: file.into(),
                    anchor: anchor(&line[4..]),
                    statement: String::new(),
                });
            }
            continue;
        }
        if current.is_none() || complete {
            continue;
        }
        if trimmed.is_empty() {
            complete = !paragraph.is_empty();
        } else if !trimmed.starts_with('#') {
            paragraph.push(trimmed);
        }
    }
    if fence.is_some() {
        return Err(format!("{file}: unclosed code fence"));
    }
    Ok(entries)
}
pub fn read(spec: &Path) -> io::Result<Vec<Entry>> {
    let mut files = fs::read_dir(spec)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    files.sort();
    let mut all = BTreeMap::new();
    for path in files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
    {
        let file = format!(
            "docs/spec/{}",
            path.file_name()
                .unwrap()
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF8 spec filename"))?
        );
        for entry in extract(&file, &fs::read_to_string(path)?).map_err(io::Error::other)? {
            if all.contains_key(&entry.id) {
                return Err(io::Error::other(format!(
                    "duplicate spec rule {}",
                    entry.id
                )));
            }
            all.insert(entry.id.clone(), entry);
        }
    }
    if all.is_empty() {
        return Err(io::Error::other("spec has no rule headings"));
    }
    Ok(all.into_values().collect())
}
