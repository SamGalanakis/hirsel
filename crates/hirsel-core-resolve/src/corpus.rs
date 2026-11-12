//! Explicit source inputs for acceptance and stage measurements. No ambient old library.
use crate::*;
use hirsel_core_ids::PackageId;
use hirsel_core_syntax::{ParseMode, ParseOutput, TokenKind, lex, parse};
use std::path::PathBuf;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CorpusManifest {
    pub core: PackageId,
    pub packages: Vec<PackageManifest>,
}
#[derive(Clone, Debug)]
pub struct CorpusSource {
    pub module: ModuleId,
    pub file: PathBuf,
    pub text: String,
    pub mode: ParseMode,
}
#[derive(Clone, Debug)]
pub struct Corpus {
    pub manifest: CorpusManifest,
    pub sources: Vec<CorpusSource>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct LocatedRefusal {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
    pub excerpt: String,
    pub refusal: Refusal,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct FileResult {
    pub file: PathBuf,
    pub module: ModuleId,
    pub parsed: bool,
    pub resolved: bool,
    pub roundtrip: bool,
    pub items: usize,
    pub names: usize,
    pub reads: usize,
    pub resource_refusal: bool,
    pub diagnostics: Vec<LocatedRefusal>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct CorpusReport {
    pub files: Vec<FileResult>,
    pub bytes: usize,
    pub code_lines: usize,
}
impl CorpusManifest {
    pub fn load(self) -> std::io::Result<Corpus> {
        let mut sources = vec![];
        for package in &self.packages {
            for source in &package.sources {
                sources.push(CorpusSource {
                    module: ModuleId {
                        package: package.package.clone(),
                        modules: source.modules.clone(),
                    },
                    file: source.file.clone(),
                    text: std::fs::read_to_string(&source.file)?,
                    mode: if source.signatures {
                        ParseMode::Signatures
                    } else {
                        ParseMode::Source
                    },
                });
            }
        }
        Ok(Corpus {
            manifest: self,
            sources,
        })
    }
}
impl Corpus {
    pub fn parse(&self) -> Vec<ParseOutput> {
        self.sources
            .iter()
            .map(|s| parse(&s.text, s.mode))
            .collect()
    }
    pub fn index(&self, parsed: &[ParseOutput]) -> NameIndex {
        let mut environment = Environment::default();
        for package in &self.manifest.packages {
            for dependency in &package.dependencies {
                environment
                    .dependencies
                    .entry(package.package.clone())
                    .or_default()
                    .insert(
                        dependency.name.clone(),
                        ModuleId {
                            package: dependency.clone(),
                            modules: vec![],
                        },
                    );
            }
        }
        let mut index = NameIndex::new(environment);
        for (source, output) in self.sources.iter().zip(parsed) {
            index.add_module(&source.module);
            for (i, item) in output.syntax.items.iter().enumerate() {
                index.index_item(&source.module, i as u32, item);
            }
        }
        index.configure_core(self.manifest.core.clone());
        index
    }
    pub fn analyze(&self, parsed: &[ParseOutput], index: &NameIndex) -> CorpusReport {
        let mut files = vec![];
        for (source, p) in self.sources.iter().zip(parsed) {
            let mut result = FileResult {
                file: source.file.clone(),
                module: source.module.clone(),
                parsed: p.diagnostics.is_empty() && p.resource_refusal.is_none(),
                resolved: false,
                roundtrip: p.syntax.text() == source.text,
                items: p.syntax.items.len(),
                names: 0,
                reads: 0,
                resource_refusal: p.resource_refusal.is_some(),
                diagnostics: vec![],
            };
            let mut record = |refusal: Refusal| {
                let site = &refusal.primary;
                let base = if site.item == u32::MAX {
                    0
                } else {
                    p.syntax.items[site.item as usize].file_offset
                };
                let start = (base + site.span.start) as usize;
                let before = &source.text[..start.min(source.text.len())];
                let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
                let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
                let excerpt = source
                    .text
                    .lines()
                    .nth(line - 1)
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                result.diagnostics.push(LocatedRefusal {
                    file: source.file.clone(),
                    line,
                    column,
                    excerpt,
                    refusal,
                });
            };
            for d in &p.diagnostics {
                record(Refusal::syntax(source.module.clone(), d));
            }
            for d in &index.diagnostics {
                if d.primary.module == source.module {
                    record(d.clone());
                }
            }
            for (i, item) in p.syntax.items.iter().enumerate() {
                let r = resolve(index, &source.module, i as u32, item);
                result.names += r.names.len();
                result.reads += r.reads.len();
                result.resource_refusal |= r.resource_refusal.is_some();
                for d in r.diagnostics {
                    record(d);
                }
            }
            result.resolved =
                result.parsed && !result.resource_refusal && result.diagnostics.is_empty();
            files.push(result);
        }
        CorpusReport {
            files,
            bytes: self.sources.iter().map(|s| s.text.len()).sum(),
            code_lines: self.sources.iter().map(|s| code_lines(&s.text)).sum(),
        }
    }
    /// Remove comments and compress trivia gaps, retaining every statement newline and
    /// each literal's spelling. Used only to form the requested code-dense measurement input.
    pub fn without_comments(mut self) -> Self {
        for source in &mut self.sources {
            let mut out = String::new();
            let mut end = 0;
            for token in lex(&source.text).tokens {
                if matches!(token.kind, TokenKind::Trivia(_) | TokenKind::Eof) {
                    continue;
                }
                let start = token.span.start as usize;
                if !out.is_empty() {
                    out.push(if source.text[end..start].contains('\n') {
                        '\n'
                    } else {
                        ' '
                    });
                }
                out.push_str(&source.text[start..token.span.end as usize]);
                end = token.span.end as usize;
            }
            source.text = out;
        }
        self
    }
    /// Select accepted definitions, retaining other declarations from a module that has
    /// one refusal. Repeat after removals until no dependent definition is refused.
    /// This is only corpus preparation for the requested clean measurement scenario.
    pub fn cleaned(mut self) -> Self {
        loop {
            let parsed = self.parse();
            let index = self.index(&parsed);
            let report = self.analyze(&parsed, &index);
            if report.files.iter().all(|f| f.resolved) {
                return self;
            }
            let mut removed = 0;
            let mut next = vec![];
            for ((source, p), result) in self.sources.into_iter().zip(parsed).zip(report.files) {
                let mut bad = std::collections::BTreeSet::new();
                for d in result.diagnostics {
                    let site = d.refusal.primary;
                    if site.item != u32::MAX {
                        bad.insert(site.item as usize);
                    } else {
                        for (i, item) in p.syntax.items.iter().enumerate() {
                            let end = item.file_offset + item.text.len() as u32;
                            if site.span.start >= item.file_offset
                                && (site.span.start < end || i + 1 == p.syntax.items.len())
                            {
                                bad.insert(i);
                                break;
                            }
                        }
                    }
                }
                let mut text = String::new();
                for (i, item) in p.syntax.items.into_iter().enumerate() {
                    if bad.contains(&i)
                        || item.tree.node(item.root).kind == hirsel_core_syntax::NodeKind::Error
                        || result.resource_refusal
                    {
                        removed += 1;
                    } else {
                        text.push_str(&item.text);
                        text.push('\n');
                    }
                }
                if !text.trim().is_empty() {
                    next.push(CorpusSource { text, ..source });
                }
            }
            self.sources = next;
            if removed == 0 {
                return self;
            }
        }
    }
}
pub fn code_lines(text: &str) -> usize {
    let mut line = 0;
    let mut cursor = 0;
    let mut last = None;
    let mut count = 0;
    for token in lex(text).tokens {
        let start = token.span.start as usize;
        let end = token.span.end as usize;
        line += text[cursor..start].bytes().filter(|b| *b == b'\n').count();
        let n = text[start..end].bytes().filter(|b| *b == b'\n').count();
        if !matches!(token.kind, TokenKind::Trivia(_) | TokenKind::Eof) {
            for at in line..=line + n {
                if last != Some(at) {
                    count += 1;
                    last = Some(at);
                }
            }
        }
        line += n;
        cursor = end;
    }
    count
}
