use super::Failure;
use crate::package_content::{Id, Root};
use crate::package_manifest::{Selections, SourceLocation};
use PathPart::{Field, Index, Key};
use hirsel_config_schema::{Document, Edit, Limits, PathPart, package_document, quote};

pub(crate) enum Form<'a> {
    Published(Root, Option<&'a SourceLocation>),
    Toolchain,
    Member(&'a str),
}

fn optional(value: Option<&str>) -> String {
    value.map_or_else(|| "None".into(), |v| format!("Some({})", quote(v)))
}

fn locator(source: Option<&SourceLocation>) -> (&'static str, String) {
    match source {
        None => ("Store", "Store".into()),
        Some(SourceLocation::Archive(p)) => ("Archive", format!("Archive({})", quote(p))),
        Some(SourceLocation::Url(u)) => ("Url", format!("Url({})", quote(u))),
        Some(SourceLocation::Registry(o)) => {
            ("Registry", format!("Registry({})", quote(&o.to_string())))
        }
        Some(SourceLocation::Git(g)) => (
            "Git",
            format!(
                "Git(url: {}, rev: {}, reference: {}, dir: {})",
                quote(&g.url),
                quote(&g.rev),
                optional(g.reference.as_deref()),
                optional(g.dir.as_deref())
            ),
        ),
    }
}

struct Editor {
    text: String,
}
impl Editor {
    fn new(bytes: &[u8]) -> Result<Self, Failure> {
        Ok(Self {
            text: std::str::from_utf8(bytes)?.into(),
        })
    }

    fn document(&self) -> Result<Document<'static>, Failure> {
        Ok(package_document(self.text.as_bytes(), &mut false)?)
    }
    fn member(&self) -> Result<Vec<PathPart>, Failure> {
        let doc = self.document()?;
        Ok(if doc.schema.root() == "Workspace" {
            vec![Field("root_package".into()), Index(0)]
        } else {
            vec![]
        })
    }
    fn apply(&mut self, edit: Option<Edit>) {
        if let Some(edit) = edit {
            self.text.replace_range(
                edit.span.start as usize..edit.span.end as usize,
                &edit.replacement,
            );
        }
    }
    fn set(&mut self, path: &[PathPart], part: PathPart, literal: &str) -> Result<(), Failure> {
        let doc = self.document()?;
        if let PathPart::Field(name) = &part
            && literal == "None"
        {
            let parent = doc.at(path).ok_or("missing record")?;
            if parent.get("fields").unwrap_or(parent)[name]["case"] == "None" {
                return Ok(());
            }
        }
        let edit = doc.insert(path, part, literal, Limits::default())?;
        self.apply(edit);
        Ok(())
    }
    fn field(&mut self, path: &[PathPart], name: &str, literal: &str) -> Result<(), Failure> {
        self.set(path, Field(name.into()), literal)
    }
    fn map(
        &mut self,
        mut parent: Vec<PathPart>,
        name: &str,
        optional: bool,
    ) -> Result<Vec<PathPart>, Failure> {
        let field = Field(name.into());
        parent.push(field.clone());
        if optional {
            if self
                .document()?
                .at(&parent)
                .is_none_or(|v| v["case"] == "None")
            {
                self.set(&parent[..parent.len() - 1], field, "Some([])")?;
            }
            parent.push(Index(0));
        }
        Ok(parent)
    }
    fn locator(
        &mut self,
        parent: &[PathPart],
        source: Option<&SourceLocation>,
    ) -> Result<(), Failure> {
        let mut path = parent.to_vec();
        path.push(Field("locator".into()));
        let (case, lit) = locator(source);
        let doc = self.document()?;
        if doc.at(&path).is_none_or(|v| v["case"] != case) {
            return self.field(parent, "locator", &lit);
        }
        match source {
            Some(SourceLocation::Git(g)) => {
                self.field(&path, "url", &quote(&g.url))?;
                self.field(&path, "rev", &quote(&g.rev))?;
                self.field(&path, "reference", &optional(g.reference.as_deref()))?;
                self.field(&path, "dir", &optional(g.dir.as_deref()))?;
            }
            Some(source) => {
                let value = match source {
                    SourceLocation::Archive(p) | SourceLocation::Url(p) => p.to_owned(),
                    SourceLocation::Registry(o) => o.to_string(),
                    SourceLocation::Git(_) => unreachable!(),
                };
                path.push(Index(0));
                let edit = doc.replace(&path, &quote(&value), Limits::default())?;
                self.apply(edit);
            }
            None => {}
        }
        Ok(())
    }
    fn binding(
        &mut self,
        shared: bool,
        alias: &str,
        id: &Id,
        form: &Form<'_>,
    ) -> Result<(), Failure> {
        let parent = if shared { vec![] } else { self.member()? };
        let map = self.map(parent, "dependencies", shared)?;
        let key = Key(alias.into());
        let mut row = map.clone();
        row.push(key.clone());
        let doc = self.document()?;
        let case = match form {
            Form::Published(..) => "Published",
            Form::Toolchain => "Toolchain",
            Form::Member(_) => "Path",
        };
        if doc.at(&row).is_some_and(|v| v["case"] == case) {
            self.field(&row, "package", &quote(&id.name))?;
            self.field(&row, "major", &id.major.to_string())?;
            match form {
                Form::Published(root, source) => {
                    self.field(&row, "root", &quote(&root.to_string()))?;
                    self.locator(&row, *source)?;
                }
                Form::Member(path) => self.field(&row, "path", &quote(path))?,
                Form::Toolchain => {}
            }
        } else {
            let mut authority = row.clone();
            authority.push(Field("authority".into()));
            let authority = doc.literal(&authority).unwrap_or("[]");
            let fields = match form {
                Form::Published(root, source) => format!(
                    ", root: {}, locator: {}",
                    quote(&root.to_string()),
                    locator(*source).1
                ),
                Form::Member(path) => format!(", path: {}", quote(path)),
                Form::Toolchain => String::new(),
            };
            self.set(
                &map,
                key,
                &format!(
                    "{case}(package: {}, major: {}{fields}, authority: {authority})",
                    quote(&id.name),
                    id.major
                ),
            )?;
        }
        Ok(())
    }
    fn inherit(&mut self, alias: &str) -> Result<(), Failure> {
        let map = self.map(self.member()?, "dependencies", false)?;
        self.set(&map, Key(alias.into()), "Inherit")
    }
}

/// Rebind path edges only after checking the proposed installed graph against
/// every original ceiling. The caller supplies an isolated copy of the package.
pub(crate) fn stage(directory: &std::path::Path) -> Result<(), Failure> {
    use crate::package_content::toolchain_major;
    use crate::package_dir::MANIFEST;
    use crate::package_files::open_directory;
    use crate::package_manifest::{Dependency, Manifest};
    use crate::package_resolve::Resolution;
    use crate::package_store::{PackageStore, Transport};
    use crate::package_workspace::{Workspace, read_manifest};
    use crate::product::bundled::{HELPER_PACKAGES, TOOLCHAIN};

    let original =
        read_manifest(&open_directory(directory)?, directory)?.ok_or("missing staged manifest")?;
    let path = directory.join(MANIFEST);
    let manifest = Manifest::parse(&original.bytes, &path)?;
    let package = manifest.package.ok_or("expected a standalone package")?;
    let id = package.id;
    let directory_name = directory.file_name().and_then(|s| s.to_str());
    let key = (id.name.as_str(), id.major);
    let bindings = TOOLCHAIN
        .iter()
        .find_map(|(name, deps)| (*name == key && directory_name == Some(name.0)).then_some(*deps))
        .or_else(|| {
            HELPER_PACKAGES.iter().find_map(|(dir, name, deps)| {
                (*name == key && directory_name == Some(*dir)).then_some(*deps)
            })
        })
        .ok_or("expected a standalone installed package in its named directory")?;
    let expected: std::collections::BTreeSet<_> = bindings
        .iter()
        .map(|(name, major)| Id {
            name: (*name).into(),
            major: *major,
        })
        .collect();
    if manifest.workspace.is_some() || manifest.selections.is_some() {
        return Err("expected a standalone installed package in its named directory".into());
    }
    let mut editor = Editor::new(&original.bytes)?;
    let mut ceilings = std::collections::BTreeMap::new();
    for (alias, dep) in package.dependencies {
        let bound = match dep {
            Dependency::Toolchain(bound) => bound,
            Dependency::Member(bound, relative, mask) if relative == format!("../{alias}") => {
                if bound.name != alias
                    || !expected.contains(&bound)
                    || toolchain_major(&alias) != Some(bound.major)
                {
                    return Err(format!("unlisted installed dependency {alias} -> {bound}").into());
                }
                ceilings.insert(alias.clone(), mask);
                let map = [Field("dependencies".into())];
                editor.set(
                    &map,
                    Key(alias.clone()),
                    &format!(
                        "Toolchain(package: {}, major: {}, authority: [])",
                        quote(&alias),
                        bound.major
                    ),
                )?;
                bound
            }
            _ => return Err(format!("unsupported installed binding {alias}").into()),
        };
        if bound.name != alias || !expected.contains(&bound) {
            return Err(format!("unexpected installed binding {alias} -> {bound}").into());
        }
    }
    let mut workspace = Workspace::staged(directory, &original, &editor.text)?;
    let member = workspace
        .members
        .get_mut(&id)
        .ok_or("staged package identity changed")?;
    if member.aliases.len() != expected.len()
        || member.edges.iter().any(|(bound, edge)| {
            !expected.contains(bound) || *edge != crate::package_workspace::Edge::Toolchain
        })
    {
        return Err("staged graph differs from the installed package inventory".into());
    }
    if !ceilings.is_empty() {
        member.ceilings = ceilings;
        let store = PackageStore::with_transport(None, Transport::offline());
        let resolution = Resolution::resolve(workspace, store, true)?;
        let checked = crate::package_audit::check(&resolution, Some(&id))?;
        if !checked.authority.is_empty() {
            return Err(checked.authority.join("\n").into());
        }
    }
    std::io::Write::write_all(&mut std::io::stdout().lock(), editor.text.as_bytes())?;
    Ok(())
}

pub(crate) fn binding(
    input: &[u8],
    alias: &str,
    id: &Id,
    form: &Form<'_>,
) -> Result<String, Failure> {
    let mut editor = Editor::new(input)?;
    editor.binding(false, alias, id, form)?;
    Ok(editor.text)
}
pub(crate) fn shared(
    input: &[u8],
    alias: &str,
    id: &Id,
    form: &Form<'_>,
    inherit: bool,
) -> Result<String, Failure> {
    let mut editor = Editor::new(input)?;
    editor.binding(true, alias, id, form)?;
    if inherit {
        editor.inherit(alias)?;
    }
    Ok(editor.text)
}
pub(super) fn inherit(input: &[u8], alias: &str) -> Result<String, Failure> {
    let mut editor = Editor::new(input)?;
    editor.inherit(alias)?;
    Ok(editor.text)
}
pub(crate) fn selections(
    input: &[u8],
    before: &Selections,
    after: &Selections,
) -> Result<String, Failure> {
    let mut editor = Editor::new(input)?;
    if before == after {
        return Ok(editor.text);
    }
    let map = editor.map(vec![], "selections", true)?;
    for id in before.keys().filter(|id| !after.contains_key(*id)) {
        let edit = editor
            .document()?
            .remove_key(&map, &id.to_string(), Limits::default())?;
        editor.apply(edit);
    }
    for (id, (root, source)) in after {
        let key = Key(id.to_string());
        let mut row = map.clone();
        row.push(key.clone());
        if !before.contains_key(id) {
            let literal = format!(
                "Selection(root: {}, locator: {})",
                quote(&root.to_string()),
                locator(source.as_ref()).1
            );
            editor.set(&map, key, &literal)?;
        } else if before.get(id) != after.get(id) {
            editor.field(&row, "root", &quote(&root.to_string()))?;
            editor.locator(&row, source.as_ref())?;
        }
    }
    Ok(editor.text)
}

pub(super) fn check_source(
    workspace: &super::Workspace,
    source: &SourceLocation,
    fetched: Root,
    selected_id: &crate::package_content::Id,
) -> Result<(), super::Failure> {
    use super::{Dependency, Edge, MANIFEST, Manifest, git, root_relative};
    let check = |alias: &str, root: Root, locator: &SourceLocation| -> Result<(), super::Failure> {
        if let (SourceLocation::Git(bound), SourceLocation::Git(new)) = (locator, source) {
            return git::check_moved(alias, bound, root, new, fetched);
        }
        if root != fetched && locator == source {
            let spelling = source.to_string();
            Err(format!(
                "{} is already bound to another root\n  bound: {root}\n  fetched: {fetched}\nto take the fetched root, run: hirsel update {alias}",
                spelling.split_once(':').unwrap().1
            )
            .into())
        } else {
            Ok(())
        }
    };
    for member in workspace.members.values() {
        for (alias, id) in &member.aliases {
            if let Edge::Published(root, Some(locator)) = &member.edges[id]
                && (!matches!(locator, SourceLocation::Registry(_)) || id == selected_id)
            {
                check(alias, *root, locator)?;
            }
        }
    }
    if let Some(root) = workspace.captured_manifest(&workspace.root)? {
        let manifest = Manifest::parse(&root.bytes, &workspace.directory.join(MANIFEST))?;
        if let Some(shared) = manifest.workspace {
            for (alias, dependency) in &shared.dependencies {
                if let Dependency::Published(id, root, Some(locator), _) = dependency
                    && (!matches!(locator, SourceLocation::Registry(_)) || id == selected_id)
                    && let Some(locator) = root_relative(locator)
                {
                    check(alias, *root, &locator)?;
                }
            }
        }
    }
    Ok(())
}

/// The binding a manifest writes: a member path, a published root, or a toolchain name.
pub(super) fn form<'a>(
    path: Option<&'a str>,
    root: Option<Root>,
    source: Option<&'a SourceLocation>,
) -> Form<'a> {
    match (path, root) {
        (Some(path), _) => Form::Member(path),
        (None, Some(root)) => Form::Published(root, source),
        (None, None) => Form::Toolchain,
    }
}

pub(super) fn spelling(id: &crate::package_content::Id, edge: &super::Edge) -> String {
    match edge {
        super::Edge::Published(root, _) => format!("{id} {root}"),
        super::Edge::Member => format!("{id} member"),
        super::Edge::Toolchain => format!("{id} toolchain"),
    }
}

pub(super) fn alias_taken(
    manifest: &std::path::Path,
    field: &str,
    alias: &str,
    existing: &crate::package_content::Id,
) -> super::Failure {
    format!(
        "{}: {field}.{alias} already binds {existing}; pass --as to choose another alias",
        manifest.display()
    )
    .into()
}
