//! Package formatting is prepare-then-write: source refusals never leave a partial reformat.
use crate::{Diagnostic, ParseMode, ResourceRefusal, Symbols, format, parse_bytes};
use std::{fs, io, path::{Path, PathBuf}};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatAction {
    Check,
    Write,
}
#[derive(Clone, Debug)]
pub struct FormatRefusal {
    pub path: PathBuf,
    pub diagnostics: Vec<Diagnostic>,
    pub resource_refusal: Option<ResourceRefusal>,
}
#[derive(Clone, Debug, Default)]
pub struct FormatReport {
    /// All discovered inputs, in path order.
    pub files: Vec<PathBuf>,
    /// Inputs that differ from canonical formatting, even when a refusal prevents writing.
    pub differing: Vec<PathBuf>,
    pub refused: Vec<FormatRefusal>,
}

/// Format every .hirsel source recursively, in deterministic path order.
/// File symlinks are sources; directory symlinks are not followed.
/// `mode` selects ordinary packages or core declarations. Diagnostic fixtures are
/// sources too: any exemptions belong to the caller, not the package formatter.
pub fn format_package(root: &Path, mode: ParseMode, action: FormatAction) -> io::Result<FormatReport> {
    fn discover(root: &Path, mode: ParseMode, out: &mut Vec<(PathBuf, ParseMode)>) -> io::Result<()> {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                discover(&path, mode, out)?;
            } else if path.extension().is_some_and(|e| e == "hirsel") && path.is_file() {
                out.push((path, mode));
            }
        }
        Ok(())
    }
    let mut inputs = vec![];
    discover(root, mode, &mut inputs)?;
    format_files(&inputs, action)
}

/// Parse and prepare every input before writing any. Check never modifies a file.
/// A parse/resource refusal returns a report and writes nothing; an I/O failure
/// returns Err. Write-time I/O errors may leave earlier successfully written files.
/// Repeated paths are formatted once; giving two modes for one path is invalid.
pub fn format_files(inputs: &[(PathBuf, ParseMode)], action: FormatAction) -> io::Result<FormatReport> {
    let mut inputs = inputs.to_vec();
    inputs.sort_by(|a,b| a.0.cmp(&b.0));
    for pair in inputs.windows(2) {
        if pair[0].0 == pair[1].0 && pair[0].1 != pair[1].1 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "conflicting parse modes for one source"));
        }
    }
    inputs.dedup_by(|a,b| a.0 == b.0);
    let mut report = FormatReport::default();
    let mut prepared = vec![];
    let mut symbols = Symbols::default();
    for (path, mode) in inputs {
        let bytes = fs::read(&path)?;
        let parsed = parse_bytes(&bytes, mode, &mut symbols);
        match format(&parsed) {
            Some(text) if text.as_bytes() != bytes => {
                report.differing.push(path.clone());
                prepared.push((path.clone(), text));
            }
            Some(_) => {}
            None => report.refused.push(FormatRefusal {
                path: path.clone(),
                diagnostics: parsed.diagnostics,
                resource_refusal: parsed.resource_refusal,
            }),
        }
        report.files.push(path);
    }
    if action == FormatAction::Write && report.refused.is_empty() {
        for (path, text) in prepared {
            fs::write(path, text)?;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("hirsel-format-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir_all(root.join("nested")).unwrap();
            Self(root)
        }
        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); }
    }
    #[test]
    fn package_check_lists_differences_and_write_is_canonical() {
        let dir = Directory::new();
        let a = dir.write("a.hirsel", b"fn a(){1}");
        let b = dir.write("nested/b.hirsel", b"fn b(){}\n");
        dir.write("ignored.txt", b"not source");
        let report = format_package(&dir.0, ParseMode::Source, FormatAction::Check).unwrap();
        assert_eq!(report.files, [a.clone(),b.clone()]);
        assert_eq!(report.differing, [a.clone(),b.clone()]);
        assert!(report.refused.is_empty());
        assert_eq!(fs::read(&a).unwrap(), b"fn a(){1}");
        let written = format_package(&dir.0, ParseMode::Source, FormatAction::Write).unwrap();
        assert_eq!(written.differing, report.differing);
        assert_eq!(fs::read_to_string(a).unwrap(), "fn a() {\n    1\n}\n");
        let clean = format_package(&dir.0, ParseMode::Source, FormatAction::Check).unwrap();
        assert!(clean.differing.is_empty());
        assert!(clean.refused.is_empty());
    }
    #[test]
    fn refused_source_and_invalid_utf8_leave_every_input_unchanged() {
        for bad in [b"fn broken(".as_slice(), b"\xff".as_slice()] {
            let dir = Directory::new();
            let a = dir.write("a.hirsel", b"fn a(){1}");
            let b = dir.write("nested/b.hirsel", bad);
            let report = format_package(&dir.0, ParseMode::Source, FormatAction::Write).unwrap();
            assert_eq!(report.differing, [a.clone()]);
            assert_eq!(report.refused.len(), 1);
            assert_eq!(report.refused[0].path, b);
            assert!(!report.refused[0].diagnostics.is_empty());
            assert_eq!(fs::read(a).unwrap(), b"fn a(){1}");
            assert_eq!(fs::read(b).unwrap(), bad);
        }
    }
    #[test]
    fn core_signature_mode_and_io_errors_are_explicit() {
        let dir = Directory::new();
        let a = dir.write("a.hirsel", b"pub fn f(x:u64)->u64");
        assert_eq!(format_package(&dir.0, ParseMode::Source, FormatAction::Write).unwrap().refused.len(), 1);
        assert!(format_package(&dir.0, ParseMode::Signatures, FormatAction::Write).unwrap().refused.is_empty());
        assert_eq!(fs::read_to_string(&a).unwrap(), "pub fn f(x: u64) -> u64\n");
        assert!(format_files(&[(a.clone(),ParseMode::Source),(a,ParseMode::Signatures)],FormatAction::Write).is_err());
        let a = dir.write("a.hirsel", b"fn a(){1}");
        assert!(format_files(&[(a.clone(),ParseMode::Source),(dir.0.join("z.hirsel"),ParseMode::Source)],FormatAction::Write).is_err());
        assert_eq!(fs::read(a).unwrap(), b"fn a(){1}");
    }
    #[cfg(unix)]
    #[test]
    fn directory_symlinks_are_not_traversed_and_file_symlinks_are_sources() {
        use std::os::unix::fs::symlink;
        let dir = Directory::new();
        let file = dir.write("a.hirsel", b"fn a(){1}");
        symlink(&dir.0,dir.0.join("nested/cycle")).unwrap();
        let link = dir.0.join("b.hirsel");
        symlink(&file,&link).unwrap();
        let report = format_package(&dir.0,ParseMode::Source,FormatAction::Check).unwrap();
        assert_eq!(report.files,[file,link]);
    }
}
