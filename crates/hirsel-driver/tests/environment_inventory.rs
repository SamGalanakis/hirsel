//! The program record key P is an explicit environment inventory
//! (crates/hirsel-driver/src/program_record.rs, `VARIABLES` and `PREFIXES`).
//! Bug law: a `HIRSEL_*` variable the compiler reads must have a class. A new
//! read with no row bypasses B whenever it is set (no lane ever hits), and a
//! name the inventory calls host-only that the compiler starts to read is left
//! out of P, so a record made under one value answers another. This audit
//! reads the sources of `hirsel`'s normal dependency closure, outside
//! `#[cfg(test)]` code, and fails on either.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

hirsel_guard::install!();

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// (name, class, host-only reader) rows and the prefix rows of the inventory.
/// A host-only row's reader is documentation: a stale one changes nothing the
/// compiler produces, so no test holds it.
fn inventory() -> (BTreeMap<String, (String, Option<String>)>, Vec<String>) {
    let source = std::fs::read_to_string(root().join("crates/hirsel-driver/src/program_record.rs"))
        .expect("the inventory's source is test data");
    let row = |line: &str| -> Option<(String, String, Option<String>)> {
        let line = line.trim().strip_prefix("(\"")?;
        let (name, rest) = line.split_once("\", Class::")?;
        let rest = rest.strip_suffix("),")?;
        Some(match rest.split_once("(\"") {
            Some((class, reader)) => (
                name.into(),
                class.into(),
                Some(reader.strip_suffix("\")")?.into()),
            ),
            None => (name.into(), rest.into(), None),
        })
    };
    let table = |start: &str| {
        let body = &source[source.find(start).expect(start)..];
        let body = &body[..body.find("];").expect("table end")];
        body.lines().filter_map(row).collect::<Vec<_>>()
    };
    let mut names = BTreeMap::new();
    for (name, class, reader) in table("const VARIABLES") {
        assert!(
            names.insert(name.clone(), (class, reader)).is_none(),
            "{name} has two rows"
        );
    }
    let prefixes = table("const PREFIXES")
        .into_iter()
        .map(|row| row.0)
        .collect();
    assert!(
        names.len() > 50,
        "the inventory parse found {} rows",
        names.len()
    );
    (names, prefixes)
}

/// The source directories of `hirsel-cli` and its normal dependencies.
fn compiler_crates() -> Vec<PathBuf> {
    // Buck2 provides the prepared metadata; under Cargo it may not exist yet.
    let prepared = root().join("tools/buck2/workspace-metadata.json");
    let metadata = match std::fs::read(&prepared) {
        Ok(bytes) => bytes,
        Err(_) => {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let output = std::process::Command::new(cargo)
                .args([
                    "metadata",
                    "--format-version",
                    "1",
                    "--no-deps",
                    "--offline",
                ])
                .current_dir(root())
                .output()
                .expect("run cargo metadata");
            assert!(output.status.success(), "cargo metadata failed");
            output.stdout
        }
    };
    let metadata: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
    let packages: BTreeMap<&str, &serde_json::Value> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|package| (package["name"].as_str().unwrap(), package))
        .collect();
    let mut paths = BTreeMap::from([("hirsel-cli", "crates/hirsel-cli".to_owned())]);
    let mut todo = vec!["hirsel-cli"];
    while let Some(name) = todo.pop() {
        for dependency in packages[name]["dependencies"].as_array().unwrap() {
            let child = dependency["name"].as_str().unwrap();
            if dependency["kind"].is_null()
                && packages.contains_key(child)
                && !paths.contains_key(child)
            {
                paths.insert(child, dependency["path"].as_str().unwrap().to_owned());
                todo.push(child);
            }
        }
    }
    paths
        .values()
        .map(|path| root().join(path).join("src"))
        .collect()
}

/// Every `.rs` file under `dir`.
fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The braces of `text` that are code (outside comments, strings, raw
/// strings and char literals), each with its 0-based line: +1 opens, -1 closes.
fn braces(text: &str) -> Vec<(usize, i64)> {
    let bytes = text.as_bytes();
    let mut out = vec![];
    let (mut i, mut line) = (0, 0);
    let advance = |i: &mut usize, line: &mut usize, n: usize| {
        for _ in 0..n {
            if bytes.get(*i) == Some(&b'\n') {
                *line += 1;
            }
            *i += 1;
        }
    };
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let mut nest = 0;
                loop {
                    if i >= bytes.len() {
                        break;
                    }
                    if bytes[i..].starts_with(b"/*") {
                        nest += 1;
                        advance(&mut i, &mut line, 2);
                    } else if bytes[i..].starts_with(b"*/") {
                        nest -= 1;
                        advance(&mut i, &mut line, 2);
                        if nest == 0 {
                            break;
                        }
                    } else {
                        advance(&mut i, &mut line, 1);
                    }
                }
            }
            b'r' if (bytes.get(i + 1) == Some(&b'#') || bytes.get(i + 1) == Some(&b'"'))
                && (i == 0
                    || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')
                    || (bytes[i - 1] == b'b'
                        && (i == 1
                            || !(bytes[i - 2].is_ascii_alphanumeric()
                                || bytes[i - 2] == b'_')))) =>
            {
                let hashes = bytes[i + 1..].iter().take_while(|&&b| b == b'#').count();
                if bytes.get(i + 1 + hashes) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                advance(&mut i, &mut line, 2 + hashes);
                let mut close = vec![b'"'];
                close.extend(std::iter::repeat_n(b'#', hashes));
                while i < bytes.len() && !bytes[i..].starts_with(&close) {
                    advance(&mut i, &mut line, 1);
                }
                advance(&mut i, &mut line, close.len());
            }
            b'"' => {
                advance(&mut i, &mut line, 1);
                while i < bytes.len() && bytes[i] != b'"' {
                    let step = if bytes[i] == b'\\' { 2 } else { 1 };
                    advance(&mut i, &mut line, step);
                }
                i += 1;
            }
            b'\'' => {
                // A char literal ('x', '\n', '{'); a lifetime has no closing quote.
                let end = bytes[i + 1..]
                    .iter()
                    .take(5)
                    .enumerate()
                    .skip(1)
                    .find(|(k, b)| **b == b'\'' && !(*k == 1 && bytes[i + 1] == b'\\'))
                    .map(|(k, _)| k);
                match end {
                    Some(end) if bytes[i + 1] == b'\\' || end == 1 || !bytes[i + 1].is_ascii() => {
                        i += end + 2;
                    }
                    _ => i += 1,
                }
            }
            b'{' => {
                out.push((line, 1));
                i += 1;
            }
            b'}' => {
                out.push((line, -1));
                i += 1;
            }
            b'\n' => advance(&mut i, &mut line, 1),
            _ => i += 1,
        }
    }
    out
}

/// The files a `mod name;` (or `#[path = "..."] mod name;`) in `file` may load.
fn module_files(file: &Path, name: &str, path: Option<&str>) -> Vec<PathBuf> {
    let parent = file.parent().unwrap();
    if let Some(path) = path {
        return vec![parent.join(path)];
    }
    let stem = file.file_stem().unwrap();
    let dir = if stem == "lib" || stem == "mod" || stem == "main" {
        parent.to_owned()
    } else {
        file.with_extension("")
    };
    // A `#[path]` file not named mod.rs resolves its children by either rule.
    [dir, parent.to_owned()]
        .iter()
        .flat_map(|dir| {
            [
                dir.join(format!("{name}.rs")),
                dir.join(name).join("mod.rs"),
            ]
        })
        .collect()
}

/// One out-of-line `mod` declaration: its name, its `#[path]`, and whether a
/// `#[cfg(test)]` attribute gates it.
fn declarations(text: &str) -> Vec<(String, Option<String>, bool)> {
    let mut out = vec![];
    let (mut test, mut path) = (false, None);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("#[cfg(all(test") {
            test = true;
        } else if let Some(rest) = trimmed.strip_prefix("#[path = \"") {
            path = rest.strip_suffix("\"]").map(str::to_owned);
        } else if trimmed.starts_with("#[") || trimmed.is_empty() {
        } else {
            let declared = trimmed
                .strip_prefix("pub ")
                .or_else(|| trimmed.strip_prefix("pub(crate) "))
                .or_else(|| trimmed.strip_prefix("pub(super) "))
                .unwrap_or(trimmed)
                .strip_prefix("mod ")
                .and_then(|rest| rest.strip_suffix(';'));
            if let Some(name) = declared {
                out.push((name.to_owned(), path.take(), test));
            }
            (test, path) = (false, None);
        }
    }
    out
}

/// The files compiled only under `#[cfg(test)]`: modules a `#[cfg(test)]`
/// declaration loads, and every module those load.
fn test_files(all: &[PathBuf]) -> BTreeSet<PathBuf> {
    let texts: BTreeMap<&PathBuf, String> = all
        .iter()
        .map(|file| (file, std::fs::read_to_string(file).unwrap()))
        .collect();
    let mut test = BTreeSet::new();
    loop {
        let before = test.len();
        for (file, text) in &texts {
            let gated = test.contains(*file);
            for (name, path, cfg_test) in declarations(text) {
                if gated || cfg_test {
                    for child in module_files(file, &name, path.as_deref()) {
                        if texts.contains_key(&child) {
                            test.insert(child);
                        }
                    }
                }
            }
        }
        if test.len() == before {
            return test;
        }
    }
}

/// The production lines of `file`: those outside inline `#[cfg(test)]` items.
fn production(file: &Path, text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut delta = vec![0i64; lines.len() + 1];
    let mut opens = vec![false; lines.len() + 1];
    for (line, change) in braces(text) {
        delta[line] += change;
        if change > 0 {
            opens[line] = true;
        }
    }
    let total: i64 = delta.iter().sum();
    assert_eq!(
        total,
        0,
        "{}: unbalanced braces; the scan miscounted",
        file.display()
    );
    let mut kept = vec![];
    let mut test_attribute = false;
    let mut depth: i64 = 0;
    // Brace depth at which a `#[cfg(test)]` block began.
    let mut in_test: Option<i64> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if in_test.is_none() {
            if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("#[cfg(all(test") {
                test_attribute = true;
            } else if test_attribute && !trimmed.starts_with("#[") && !trimmed.is_empty() {
                test_attribute = false;
                if opens[index] {
                    in_test = Some(depth);
                }
            }
        }
        depth += delta[index];
        match in_test {
            Some(start) => {
                if depth <= start {
                    in_test = None;
                }
            }
            None => kept.push((index + 1, (*line).to_owned())),
        }
    }
    kept
}

/// Every `"HIRSEL_*"` literal in production code of the compiler's crates,
/// with its first site.
fn compiler_reads() -> BTreeMap<String, String> {
    let mut all = vec![];
    for dir in compiler_crates() {
        files(&dir, &mut all);
    }
    all.sort();
    let test = test_files(&all);
    let mut reads = BTreeMap::new();
    for file in all.iter().filter(|file| !test.contains(*file)) {
        let text = std::fs::read_to_string(file).unwrap();
        for (line, content) in production(file, &text) {
            let mut rest = content.as_str();
            while let Some(at) = rest.find("\"HIRSEL_") {
                let tail = &rest[at + 1..];
                let end = tail.find('"').unwrap_or(tail.len());
                let name = &tail[..end];
                // The inventory's own rows name variables; they do not read them.
                let row = tail[end..].starts_with("\", Class::");
                if !row
                    && name
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                {
                    let site = format!(
                        "{}:{line}",
                        file.strip_prefix(root()).unwrap_or(file).display()
                    );
                    reads.entry(name.to_owned()).or_insert(site);
                }
                rest = &tail[end..];
            }
        }
    }
    reads
}

#[test]
fn every_environment_variable_the_compiler_reads_has_a_class() {
    let (names, prefixes) = inventory();
    let reads = compiler_reads();
    let mut unclassified = vec![];
    let mut host_read = vec![];
    for (name, site) in &reads {
        // `program_record`'s own "unknown `HIRSEL_*` bypasses" test.
        if name == "HIRSEL_" {
            continue;
        }
        match names.get(name) {
            Some((class, _)) if class == "HostOnly" => host_read.push(format!("{name} at {site}")),
            Some(_) => {}
            None if prefixes
                .iter()
                .any(|prefix| name.starts_with(prefix.as_str())) => {}
            None => unclassified.push(format!("{name} at {site}")),
        }
    }
    assert!(
        unclassified.is_empty(),
        "the compiler reads these variables, which have no class in program_record.rs \
         `VARIABLES`: {unclassified:#?}"
    );
    assert!(
        host_read.is_empty(),
        "these host-only variables are read by the compiler, so they may change what it \
         produces; give each a class that keys P: {host_read:#?}"
    );
    // The scan sees production code: these reads are known to be there.
    for known in [
        "HIRSEL_VERIFY_CACHE",
        "HIRSEL_ID_SALT",
        "HIRSEL_RUNTIME_MAP",
    ] {
        assert!(reads.contains_key(known), "the scan missed {known}");
    }
}
