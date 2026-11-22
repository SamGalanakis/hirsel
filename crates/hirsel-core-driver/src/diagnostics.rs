//! Tool encoding of PROG-7 sites. Cached item-local anchors bind to current file bytes.
use super::*;
use hirsel_core_resolve::{
    CausalRole, DefinitionKind, ModulePath, NameScope, Namespace, RefusalDetail,
};
use hirsel_core_syntax::{Span, Symbol};

#[derive(Debug, Serialize)]
pub struct SourceSite {
    /// The file: its path relative to the request's manifest, or without a
    /// manifest its module (`app@1/main`).
    pub module: String,
    pub span: Span,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub line_text: Option<String>,
}
impl SourceSite {
    fn new(module: String, span: Span, bytes: Option<&[u8]>) -> Self {
        let (line, column, line_text) = bytes.map_or((None, None, None), |text| {
            let offset = (span.start as usize).min(text.len());
            let (line, column) = line_column(text, offset);
            let start = text[..offset]
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |n| n + 1);
            let end = text[offset..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(text.len(), |n| offset + n);
            let row = &text[start..end];
            let row = if end < text.len() {
                row.strip_suffix(b"\r").unwrap_or(row)
            } else {
                row
            };
            (
                Some(line),
                Some(column),
                Some(String::from_utf8_lossy(row).into_owned()),
            )
        });
        Self {
            module,
            span,
            line,
            column,
            line_text,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DiagnosticCause {
    pub site: SourceSite,
    pub role: CausalRole,
}
#[derive(Serialize)]
pub struct Diagnostic<'a> {
    pub rule: &'a str,
    pub primary: SourceSite,
    pub causal: Vec<DiagnosticCause>,
    pub declared: Option<&'a str>,
    pub required: Option<&'a str>,
    pub message: &'a str,
    pub fixes: Vec<Fix>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub missing_patterns: &'a [String],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Applicability {
    Safe,
    Maybe,
    Placeholder,
}

#[derive(Debug, Serialize)]
pub struct Fix {
    pub applicability: Applicability,
    pub edits: Vec<Edit>,
}

#[derive(Debug, Serialize)]
pub struct Edit {
    pub module: String,
    pub span: Span,
    pub text: String,
}
impl Edit {
    pub(crate) fn description(&self) -> String {
        if self.text.is_empty() {
            format!(
                "delete {} bytes {}..{}",
                self.module, self.span.start, self.span.end
            )
        } else if self.span.is_empty() {
            format!(
                "insert {:?} at {} byte {}",
                self.text, self.module, self.span.start
            )
        } else {
            format!(
                "replace {} bytes {}..{} with {:?}",
                self.module, self.span.start, self.span.end, self.text
            )
        }
    }
}

#[derive(Serialize)]
pub struct IncompleteDiagnostic<'a> {
    pub primary: SourceSite,
    pub causal: Vec<DiagnosticCause>,
    pub message: &'a str,
}
/// A resource refusal (PROG-6) names no rule: only its site and the limit it passes.
#[derive(Serialize)]
pub struct ResourceDiagnostic<'a> {
    pub primary: SourceSite,
    pub message: &'a str,
}
impl CheckProduct {
    pub(crate) fn source_site(&self, site: &Site) -> io::Result<SourceSite> {
        hirsel_core_ids::published::catch_io(|| self.source_site_inner(site))
    }
    fn source_site_inner(&self, site: &Site) -> io::Result<SourceSite> {
        let own = site
            .source
            .and_then(|source| self.files.iter().find(|f| f.file == source));
        let (base, name, image_source) = if let Some(file) = own {
            let base = file
                .syntax
                .items
                .get(site.item as usize)
                .map_or(0, |i| i.file_offset);
            (base, file.name.clone(), None)
        } else if let (Some(image), Some(at)) = (&self.image, item_at(site)) {
            let (name, source) = image.source_file(at.0)?;
            (image.file_offset(at)?, name, Some(source))
        } else {
            (0, None, None)
        };
        let bytes = own
            .map(|f| self.source_bytes(f))
            .or_else(|| image_source.as_deref().map(str::as_bytes));
        let id = self.index.module_path(site.module);
        Ok(SourceSite::new(
            name.unwrap_or_else(|| {
                format!(
                    "{}@{}/{}",
                    id.package.name,
                    id.package.version,
                    id.modules.join("/")
                )
            }),
            Span {
                start: base + site.span.start,
                end: base + site.span.end,
            },
            bytes,
        ))
    }
    fn source_bytes<'a>(&'a self, file: &'a FileProduct) -> &'a [u8] {
        self.unparsed_sources
            .iter()
            .find_map(|(id, bytes)| (*id == file.file).then_some(bytes.as_ref()))
            .unwrap_or(file.syntax.source.as_bytes())
    }
    pub fn machine_diagnostics(&self) -> io::Result<Vec<Diagnostic<'_>>> {
        self.diagnostics_for(self.diagnostics.iter())
    }
    pub(crate) fn render_refusal(&self, refusal: &Refusal) -> io::Result<String> {
        use std::fmt::Write;

        let mut text = format!(
            "{}: {} {}",
            self.render_site(&refusal.primary)?,
            refusal.rule,
            refusal.message
        );
        for cause in &refusal.causal {
            write!(
                text,
                "\n  {}: {}",
                cause.role.label(),
                self.render_site(&cause.site)?
            )
            .unwrap();
        }
        for edit in self.fixes(refusal)?.iter().flat_map(|fix| &fix.edits) {
            write!(text, "\n  {}", edit.description()).unwrap();
        }
        Ok(text)
    }
    pub(crate) fn render_site(&self, site: &Site) -> io::Result<String> {
        let location = self.source_site(site)?;
        let file = site
            .source
            .and_then(|source| self.files.iter().find(|f| f.file == source));
        Ok(match file {
            Some(file) => render_span(
                Path::new(&location.module),
                self.source_bytes(file),
                location.span,
            ),
            None => SourceFiles::none().render(&location),
        })
    }
    pub fn diagnostics_for<'a>(
        &self,
        rows: impl IntoIterator<Item = &'a Refusal>,
    ) -> io::Result<Vec<Diagnostic<'a>>> {
        rows.into_iter()
            .map(|r| {
                Ok(Diagnostic {
                    rule: &r.rule,
                    primary: self.source_site(&r.primary)?,
                    causal: r
                        .causal
                        .iter()
                        .map(|c| {
                            Ok(DiagnosticCause {
                                site: self.source_site(&c.site)?,
                                role: c.role,
                            })
                        })
                        .collect::<io::Result<_>>()?,
                    declared: r.declared.as_deref(),
                    required: r.required.as_deref(),
                    message: &r.message,
                    fixes: self.fixes(r)?,
                    missing_patterns: match r.detail.as_deref() {
                        Some(RefusalDetail::MissingPattern { pattern, .. }) => {
                            std::slice::from_ref(pattern)
                        }
                        _ => &[],
                    },
                })
            })
            .collect()
    }
    pub fn machine_incomplete(&self) -> io::Result<Vec<IncompleteDiagnostic<'_>>> {
        self.incomplete_for(self.incomplete.iter())
    }
    pub fn machine_resources(&self) -> io::Result<Vec<ResourceDiagnostic<'_>>> {
        self.resources_for(self.resources.iter())
    }
    pub fn resources_for<'a>(
        &self,
        rows: impl IntoIterator<Item = &'a ResourceRefusal>,
    ) -> io::Result<Vec<ResourceDiagnostic<'a>>> {
        rows.into_iter()
            .map(|r| {
                Ok(ResourceDiagnostic {
                    primary: self.source_site(&r.primary)?,
                    message: &r.message,
                })
            })
            .collect()
    }
    pub fn incomplete_for<'a>(
        &self,
        rows: impl IntoIterator<Item = &'a Incomplete>,
    ) -> io::Result<Vec<IncompleteDiagnostic<'a>>> {
        rows.into_iter()
            .map(|g| {
                Ok(IncompleteDiagnostic {
                    primary: self.source_site(&g.primary)?,
                    causal: g
                        .causal
                        .iter()
                        .map(|s| {
                            Ok(DiagnosticCause {
                                site: self.source_site(s)?,
                                role: CausalRole::DerivedHere,
                            })
                        })
                        .collect::<io::Result<_>>()?,
                    message: &g.message,
                })
            })
            .collect()
    }
}

impl CheckProduct {
    fn fixes(&self, r: &Refusal) -> io::Result<Vec<Fix>> {
        let Some(file) = self.files.iter().find(|f| Some(f.file) == r.primary.source) else {
            return Ok(vec![]);
        };
        let base = file
            .syntax
            .items
            .get(r.primary.item as usize)
            .map_or(0, |item| item.file_offset);
        let source = file.syntax.text();
        let span_text =
            |span: Span| source.get((base + span.start) as usize..(base + span.end) as usize);

        let edit = if r.rule == "LANG-8"
            && r.declared.as_deref() == Some("waits")
            && r.required.as_deref() == Some("not waits")
            && span_text(r.primary.span) == Some("waits")
        {
            Some((Applicability::Safe, r.primary.span, String::new()))
        } else if let Some(RefusalDetail::MissingPattern { pattern, close }) = r.detail.as_deref() {
            let close = Span {
                start: close.start,
                end: close.start,
            };
            Some((
                Applicability::Placeholder,
                close,
                format!("\n    {pattern} => fail(\"TODO: handle missing case\"),\n"),
            ))
        } else if let Some(RefusalDetail::MethodEdit {
            name,
            receiver,
            callee_end,
            call,
            arity,
        }) = r.detail.as_deref()
        {
            let Some(receiver_text) = span_text(*receiver) else {
                return Ok(vec![]);
            };
            // Retain the argument list and any trivia after the member name.
            let start = (base + callee_end) as usize;
            let end = (base + call.end) as usize;
            let Some(open) = file.syntax.tree.tokens.iter().find(|t| {
                t.span.start as usize >= start && (t.span.end as usize) < end && !t.kind.is_trivia()
            }) else {
                return Ok(vec![]);
            };
            let after_open = open.span.end as usize;
            let rest = &source[after_open..end];
            let separator = if *arity == 0 { "" } else { ", " };
            Some((
                Applicability::Maybe,
                *call,
                format!(
                    "{name}{}({receiver_text}{separator}{rest}",
                    &source[start..open.span.start as usize]
                ),
            ))
        } else if r.causal.iter().any(|c| c.role == CausalRole::OpenedHere)
            && let Some(close @ (")" | "]" | "}")) = r.required.as_deref()
        {
            let before = base + r.primary.span.start;
            let token = file.syntax.tree.tokens.iter().rev().find(|t| {
                t.span.end <= before
                    && !t.kind.is_trivia()
                    && !matches!(t.kind, hirsel_core_syntax::TokenKind::Eof)
            });
            token.and_then(|token| {
                let end = token.span.end;
                (r.declared.as_deref() == Some("end of file")
                    || source.get(end as usize..before as usize)?.contains('\n'))
                .then_some((
                    Applicability::Maybe,
                    Span {
                        start: end - base,
                        end: end - base,
                    },
                    close.to_owned(),
                ))
            })
        } else {
            hirsel_core_ids::published::catch_io(|| {
                Ok(
                    name_edit(&self.index, r).and_then(|(span, expected, text)| {
                        (expected.is_none() || span_text(span) == expected).then_some((
                            Applicability::Maybe,
                            span,
                            text,
                        ))
                    }),
                )
            })?
        };
        let Some((applicability, span, text)) = edit else {
            return Ok(vec![]);
        };
        let module = source_name(file.name.as_deref(), &self.index.module_path(file.module));
        Ok(vec![Fix {
            applicability,
            edits: vec![Edit {
                module,
                span: Span {
                    start: base + span.start,
                    end: base + span.end,
                },
                text,
            }],
        }])
    }
}

pub(crate) fn source_name(name: Option<&str>, module: &ModulePath) -> String {
    name.map_or_else(
        || {
            format!(
                "{}@{}/{}",
                module.package.name,
                module.package.version,
                module.modules.join("/")
            )
        },
        str::to_owned,
    )
}

pub(crate) fn resolve_method_edits(program: &hirsel_core_front::Program<'_>, rows: &mut [Refusal]) {
    use hirsel_core_front::{Def, Mode, TyKind};

    let index = program.index;
    for row in rows {
        let Some(RefusalDetail::OpaqueMethod {
            receiver_type,
            receiver,
            member,
            call,
            arity,
        }) = row.detail.as_deref()
        else {
            continue;
        };
        let Some(source) = row
            .primary
            .source
            .and_then(|id| program.modules.iter().find(|m| m.source == id))
        else {
            continue;
        };
        let path = &receiver_type.0;
        let item = source.syntax.item(row.primary.item as usize);
        let name = &item.source_text()[member.start as usize..member.end as usize];
        // Core declares its prelude types together, with their operations in
        // modules named for the types (List in the prelude, list.len in list).
        let modules = if path.package.name_is("core") && path.modules == ["core"] {
            vec![path.name.to_ascii_lowercase()]
        } else {
            path.modules.clone()
        };
        let Some(module) = index.module_at(&path.package, &modules) else {
            continue;
        };
        let Some(symbol) = index.symbols.find(name) else {
            continue;
        };
        let mut members =
            index.members_named(row.primary.module, module, symbol, Namespace::Value, false);
        let Some(id) = members.next() else {
            continue;
        };
        if members.next().is_some() || !index.get(id).public || index.get(id).site.module != module
        {
            continue;
        }
        let Some(Def::Fn(f)) = program.by_definition[id.0 as usize] else {
            continue;
        };
        let f = &program.fns[f as usize];
        let Some(first) = f.params.first() else {
            continue;
        };
        if f.params.len() != *arity + 1 || f.is_test || first.mode != Mode::View {
            continue;
        }
        let TyKind::Adt(d, types) = program.types.kind(first.ty) else {
            continue;
        };
        if &program.decls[d.index()].path != receiver_type
            || !types.iter().enumerate().all(
                |(i, ty)| matches!(program.types.kind(*ty), TyKind::Param(p) if *p as usize == i),
            )
        {
            continue;
        }
        // Find a written module path from this request's visible module heads.
        let qualified = [false, true].into_iter().find_map(|package_head| {
            let mut names = if package_head {
                let mut qualified = vec![path.package.name.as_str()];
                qualified.extend(modules.iter().map(String::as_str));
                qualified
            } else {
                modules.iter().map(String::as_str).collect()
            };
            let head = index.symbols.find(names.first()?)?;
            let mut resolved = index.module_head(row.primary.module, head)?;
            for segment in &names[1..] {
                resolved = index.child(resolved, index.symbols.find(segment)?)?;
            }
            if resolved != module {
                return None;
            }
            names.push(name);
            Some(names.join("."))
        });
        if let Some(name) = qualified {
            row.detail = Some(Box::new(RefusalDetail::MethodEdit {
                name,
                receiver: *receiver,
                callee_end: member.end,
                call: *call,
                arity: *arity,
            }));
        }
    }
}

fn name_edit<'a>(
    index: &NameIndex,
    refusal: &'a Refusal,
) -> Option<(Span, Option<&'a str>, String)> {
    let lookup = refusal.unresolved.as_deref()?;
    let from = refusal.primary.module;
    let module_at = |path: &ModulePath| index.module_at(&path.package, &path.modules);
    let best = match &lookup.scope {
        NameScope::ModuleMember {
            module,
            namespace,
            constructor,
        } => {
            if module.package.name_is("core")
                && module.modules == ["net"]
                && matches!(
                    lookup.written.as_str(),
                    "listen" | "connect" | "accept" | "recv_bounded" | "recv_ready" | "send_pair"
                )
                && let Some(host) = index.module_at(&module.package, &["host".into()])
                && let Some(head) = index.symbols.find("host")
                && index.module_head(from, head) == Some(host)
                && let Some(symbol) = index.symbols.find(&lookup.written)
                && visible_member(index, from, host, symbol, *namespace, *constructor).is_some()
            {
                return Some((
                    Span {
                        start: refusal.primary.span.start,
                        end: lookup.span.end,
                    },
                    None,
                    format!("host.{}", lookup.written),
                ));
            }
            let module = module_at(module)?;
            let names = index.member_names_in(module).filter(|&name| {
                visible_member(index, from, module, name, *namespace, *constructor).is_some()
            });
            closest_name(index, &lookup.written, names)
        }
        NameScope::ModulePrefix {
            module,
            children_of,
            namespace,
        } => {
            let module = module_at(module)?;
            let parent = module_at(children_of)?;
            let children = index.child_names(parent).chain(index.child_names(module));
            let constants = index.member_names_in(module).filter(|&name| {
                *namespace == Namespace::Value
                    && visible_member(index, from, module, name, Namespace::Value, false)
                        == Some(DefinitionKind::Constant)
            });
            let names = children
                .chain(constants)
                .filter(|&name| parent == module || index.child(parent, name) != Some(module));
            closest_name(index, &lookup.written, names)
        }
        NameScope::UnqualifiedPath {
            modules,
            namespace,
            constructor,
        } => {
            let std = crate::package("std");
            let root = index.module_at(&std, &[])?;
            if index.module_head(from, index.symbols.find("std")?) != Some(root) {
                return None;
            }
            let module = index.module_at(&std, modules)?;
            let member = index.symbols.find(&lookup.written)?;
            visible_member(index, from, module, member, *namespace, *constructor)?;
            let start = refusal.primary.span.start;
            return Some((Span { start, end: start }, None, "std.".into()));
        }
        NameScope::Field(path) | NameScope::Variant(path) => {
            let path = &path.0;
            let module = index.module_at(&path.package, &path.modules)?;
            let name = index.symbols.find(&path.name)?;
            let definition = index.member(module, name, Namespace::Type, false)?;
            let row = index.get(definition);
            if module != from
                && (row.opaque
                    || !row.public
                    || !index.reachable(from, index.module_package(module)))
            {
                return None;
            }
            if matches!(lookup.scope, NameScope::Field(_)) {
                closest_name(
                    index,
                    &lookup.written,
                    index.field_names(definition).iter().copied(),
                )
            } else {
                let names = index
                    .variant_names(definition)
                    .iter()
                    .copied()
                    .filter(|&name| {
                        index.member(from, name, Namespace::Value, false).is_none()
                            && index.prelude(name, Namespace::Value, false).is_none()
                    });
                closest_name(index, &lookup.written, names)
            }
        }
    }?;
    Some((
        lookup.span,
        Some(&lookup.written),
        index.symbols.text(best).into(),
    ))
}

fn visible_member(
    index: &NameIndex,
    from: ModuleId,
    module: ModuleId,
    name: Symbol,
    ns: Namespace,
    constructor: bool,
) -> Option<DefinitionKind> {
    let mut members = index.members_named(from, module, name, ns, false);
    let (id, ambiguous) = if let Some(id) = members.next() {
        (id, members.next().is_some())
    } else if constructor {
        let mut types = index.members_named(from, module, name, Namespace::Type, true);
        (types.next()?, types.next().is_some())
    } else {
        return None;
    };
    let row = index.get(id);
    (!ambiguous
        && (row.site.module == from || row.public && !(constructor && row.opaque))
        && (!constructor || row.kind != DefinitionKind::Type || index.variant_names(id).is_empty()))
    .then_some(row.kind)
}

fn closest_name(
    index: &NameIndex,
    written: &str,
    candidates: impl Iterator<Item = Symbol>,
) -> Option<Symbol> {
    let written: Vec<char> = written.chars().collect();
    let mut best_distance = (written.len() / 3).max(1);
    let mut best = None;
    let mut tied = false;
    let mut row = vec![0; written.len() + 1];
    for name in candidates {
        let candidate = index.symbols.text(name);
        if written.len().abs_diff(candidate.chars().count()) > best_distance {
            continue;
        }
        let distance = edit_distance(&written, candidate, &mut row);
        if distance == 0 || distance > best_distance {
            continue;
        }
        if best.is_none() || distance < best_distance {
            best_distance = distance;
            best = Some(name);
            tied = false;
        } else if best != Some(name) {
            tied = true;
        }
    }
    best.filter(|_| !tied)
}

fn edit_distance(written: &[char], candidate: &str, row: &mut [usize]) -> usize {
    for (i, cell) in row.iter_mut().enumerate() {
        *cell = i;
    }
    for (i, ch) in candidate.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &other) in written.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ch != other)).min(1 + above.min(row[j]));
            diagonal = above;
        }
    }
    row[written.len()]
}

/// The terminal form of PROG-7 sites: `path:line:column`, from the request's
/// own source files. A site whose file the request did not read (an image's
/// library source) keeps its module and byte span.
pub struct SourceFiles(Vec<(String, PathBuf, Arc<[u8]>)>);

impl SourceFiles {
    pub fn of(request: &CheckRequest) -> Self {
        let mut files = Vec::new();
        for package in &request.inputs.packages {
            for source in &package.sources {
                let Some(captured) = &source.captured else {
                    continue;
                };
                let name = captured.name.clone().unwrap_or_else(|| {
                    format!(
                        "{}@{}/{}",
                        package.package.name,
                        package.package.version,
                        source.modules.join("/")
                    )
                });
                files.push((name, source.file.clone(), Arc::clone(&captured.bytes)));
            }
        }
        Self(files)
    }

    pub fn none() -> Self {
        Self(Vec::new())
    }

    pub fn render(&self, site: &SourceSite) -> String {
        let Some((_, path, text)) = self.0.iter().find(|(name, _, _)| *name == site.module) else {
            return format!("{} {}..{}", site.module, site.span.start, site.span.end);
        };
        render_span(path, text, site.span)
    }
}

pub(crate) fn render_span(path: &Path, text: &[u8], span: Span) -> String {
    let (line, column) = line_column(text, span.start as usize);
    let (end_line, end_column) = line_column(text, span.end as usize);
    let path = path.display();
    if span.end <= span.start || (line, column) == (end_line, end_column) {
        format!("{path}:{line}:{column}")
    } else if line == end_line {
        format!("{path}:{line}:{column}-{end_column}")
    } else {
        format!("{path}:{line}:{column}-{end_line}:{end_column}")
    }
}

/// The 1-based line and column of byte `offset` (docs/spec/diagnostics.md,
/// "Terminal sites"). Lines end at LF; a CR immediately before that LF belongs
/// to the line end, not to a column. Columns count Unicode scalar values: every
/// byte except a UTF-8 continuation byte starts one, so an invalid byte counts
/// as one column.
pub fn line_column(text: &[u8], offset: usize) -> (usize, usize) {
    let prefix = &text[..offset.min(text.len())];
    let line_start = prefix
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |at| at + 1);
    let line = 1 + prefix.iter().filter(|&&b| b == b'\n').count();
    let mut columns = &prefix[line_start..];
    if columns.last() == Some(&b'\r') && text.get(prefix.len()) == Some(&b'\n') {
        columns = &columns[..columns.len() - 1];
    }
    let column = 1 + columns.iter().filter(|&&b| b & 0xc0 != 0x80).count();
    (line, column)
}
