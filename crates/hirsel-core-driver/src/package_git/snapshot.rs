//! A commit's tree read as objects in memory (GIT-3): paths are looked up one component at a
//! time by exact bytes, links are followed within the walk's containing directory, and a
//! directory's identity is the names of the walk taken to it.

use super::hash::Oid;
use super::pack::{Kind, Pack};
use super::{Failure, quote_text};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

const LINKS: usize = 40;
const TARGET_LIMIT: usize = 4096;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    File,
    Tree,
    Link,
    Submodule,
}

struct TreeEntry {
    name: Vec<u8>,
    mode: Mode,
    id: Oid,
}

/// A directory of the snapshot: the names from the top and the tree at each step.
#[derive(Clone)]
pub(crate) struct Dir {
    pub(crate) names: Vec<String>,
    trees: Vec<Oid>,
}

impl Dir {
    /// The directory's path from the top of the tree, `/`-separated, empty at the top.
    pub(crate) fn path(&self) -> String {
        self.names.join("/")
    }

    /// The directory `depth` names from the top on this walk.
    pub(crate) fn ancestor(&self, depth: usize) -> Dir {
        Dir {
            names: self.names[..depth].to_vec(),
            trees: self.trees[..=depth].to_vec(),
        }
    }
}

impl PartialEq for Dir {
    fn eq(&self, other: &Self) -> bool {
        self.names == other.names
    }
}

/// Why a walk did not reach what it wanted.
pub(crate) enum Missed {
    Outside,
    Missing,
    WrongType,
    Submodule(String),
    Pack(Failure),
}

impl Missed {
    /// The walk's failure in section 9's words, as a `WalkError` explains a host walk.
    pub(crate) fn explain(self, boundary: &str, object: &str) -> Result<String, Failure> {
        Ok(match self {
            Self::Outside => format!("resolves outside the {boundary}"),
            Self::Missing => "does not exist".into(),
            Self::WrongType => format!("does not resolve to {object}"),
            Self::Submodule(entry) => {
                format!("reaches submodule \"{entry}\", and hirsel does not fetch submodules")
            }
            Self::Pack(failure) => return Err(failure),
        })
    }
}

impl From<Failure> for Missed {
    fn from(failure: Failure) -> Self {
        Self::Pack(failure)
    }
}

pub(crate) struct Snapshot {
    pack: Arc<Pack>,
    top: Dir,
    trees: RefCell<HashMap<Oid, Rc<Vec<TreeEntry>>>>,
}

fn bad(text: String) -> Failure {
    Failure::Pack(text)
}

enum Step {
    Name(String),
    Parent,
    Directory,
}

fn steps(path: &str, pending: &mut Vec<Step>) {
    if path.ends_with('/') || path.ends_with("/.") || path == "." {
        pending.push(Step::Directory);
    }
    for component in path.split('/').rev() {
        match component {
            "" => {}
            "." => pending.push(Step::Directory),
            ".." => pending.push(Step::Parent),
            name => pending.push(Step::Name(name.to_string())),
        }
    }
}

enum Object {
    Tree(Oid),
    Blob(Oid),
}

impl Snapshot {
    /// Opens commit `rev` of `pack`: the commit and its tree, checked as GIT-9 step 4 says.
    pub(crate) fn open(pack: Arc<Pack>, rev: &str) -> Result<Self, Failure> {
        let id = Oid::parse(rev).expect("a binding's rev is a full commit id");
        if pack.header(&id).is_none() {
            return Err(bad(format!("commit {rev} is not in the pack")));
        }
        let commit = pack.read(&id, Kind::Commit)?;
        let malformed = || bad(format!("object {rev} is not a well-formed commit"));
        let line = commit
            .strip_prefix(b"tree ")
            .and_then(|rest| rest.split(|b| *b == b'\n').next())
            .ok_or_else(malformed)?;
        let tree = std::str::from_utf8(line)
            .ok()
            .and_then(Oid::parse)
            .filter(|tree| tree.as_bytes().len() == pack.format().len())
            .ok_or_else(malformed)?;
        if commit.len() == 5 + line.len() || commit[5 + line.len()] != b'\n' {
            return Err(malformed());
        }
        Ok(Self {
            pack,
            top: Dir {
                names: Vec::new(),
                trees: vec![tree],
            },
            trees: RefCell::default(),
        })
    }

    pub(crate) fn top(&self) -> Dir {
        self.top.clone()
    }

    fn tree(&self, id: Oid) -> Result<Rc<Vec<TreeEntry>>, Failure> {
        if let Some(tree) = self.trees.borrow().get(&id) {
            return Ok(tree.clone());
        }
        let bytes = self.pack.read(&id, Kind::Tree)?;
        let malformed = || bad(format!("object {id} is not a well-formed tree"));
        let length = self.pack.format().len();
        let mut entries: Vec<TreeEntry> = Vec::new();
        let mut rest = &bytes[..];
        while !rest.is_empty() {
            let space = rest.iter().position(|b| *b == b' ').ok_or_else(malformed)?;
            let mode = &rest[..space];
            rest = &rest[space + 1..];
            let nul = rest.iter().position(|b| *b == 0).ok_or_else(malformed)?;
            let name = rest[..nul].to_vec();
            rest = &rest[nul + 1..];
            if name.is_empty() || rest.len() < length {
                return Err(malformed());
            }
            let entry = Oid::from_bytes(&rest[..length]);
            rest = &rest[length..];
            let mode = match mode {
                b"100644" | b"100755" | b"100664" => Mode::File,
                b"40000" | b"040000" => Mode::Tree,
                b"120000" => Mode::Link,
                b"160000" => Mode::Submodule,
                other => {
                    return Err(bad(format!("tree {id} has mode {}", quote_text(other))));
                }
            };
            if entries.iter().any(|seen| seen.name == name) {
                return Err(bad(format!(
                    "tree {id} names \"{}\" twice",
                    quote_text(&name)
                )));
            }
            entries.push(TreeEntry {
                name,
                mode,
                id: entry,
            });
        }
        let entries = Rc::new(entries);
        self.trees.borrow_mut().insert(id, entries.clone());
        Ok(entries)
    }

    /// Walks `path` from `from`, never leaving `within` (an ancestor of `from` or `from`
    /// itself), and returns where it ends.
    fn walk(
        &self,
        within: &Dir,
        from: &Dir,
        path: &str,
    ) -> Result<(Vec<String>, Vec<Object>), Missed> {
        let floor = within.names.len();
        let mut names = from.names.clone();
        let mut open: Vec<Object> = from.trees.iter().map(|id| Object::Tree(*id)).collect();
        if path.is_empty() || path.starts_with('/') {
            return Err(Missed::Outside);
        }
        let mut pending = Vec::new();
        steps(path, &mut pending);
        let mut links = 0;
        while let Some(step) = pending.pop() {
            let Some(Object::Tree(current)) = open.last() else {
                return Err(Missed::WrongType);
            };
            let current = *current;
            match step {
                Step::Directory => {}
                Step::Parent => {
                    if names.len() == floor {
                        return Err(Missed::Outside);
                    }
                    open.pop();
                    names.pop();
                }
                Step::Name(name) => {
                    let tree = self.tree(current)?;
                    let entry = tree
                        .iter()
                        .find(|entry| entry.name == name.as_bytes())
                        .ok_or(Missed::Missing)?;
                    match entry.mode {
                        Mode::Submodule => return Err(Missed::Submodule(quote_text(&entry.name))),
                        Mode::Link => {
                            links += 1;
                            if links > LINKS {
                                return Err(Missed::WrongType);
                            }
                            let target = self.pack.read(&entry.id, Kind::Blob)?;
                            if target.is_empty() || target.len() > TARGET_LIMIT {
                                return Err(Missed::Outside);
                            }
                            let target =
                                std::str::from_utf8(&target).map_err(|_| Missed::Outside)?;
                            if target.starts_with('/') {
                                return Err(Missed::Outside);
                            }
                            steps(target, &mut pending);
                        }
                        Mode::Tree => {
                            names.push(name);
                            open.push(Object::Tree(entry.id));
                        }
                        Mode::File => {
                            names.push(name);
                            open.push(Object::Blob(entry.id));
                        }
                    }
                }
            }
        }
        Ok((names, open))
    }

    /// The bytes of the regular file `path` names from `from`, inside `within`.
    pub(crate) fn file(
        &self,
        within: &Dir,
        from: &Dir,
        path: &str,
    ) -> Result<Arc<Vec<u8>>, Missed> {
        let (_, mut open) = self.walk(within, from, path)?;
        match open.pop() {
            Some(Object::Blob(id)) => Ok(self.pack.read(&id, Kind::Blob)?),
            _ => Err(Missed::WrongType),
        }
    }

    /// The directory `path` names from `from`, inside `within`.
    pub(crate) fn dir(&self, within: &Dir, from: &Dir, path: &str) -> Result<Dir, Missed> {
        let (names, open) = self.walk(within, from, path)?;
        let trees = open
            .into_iter()
            .map(|object| match object {
                Object::Tree(id) => Ok(id),
                Object::Blob(_) => Err(Missed::WrongType),
            })
            .collect::<Result<_, _>>()?;
        Ok(Dir { names, trees })
    }

    /// The bytes of `hirsel.toml` in `dir`, `None` when it has none.
    pub(crate) fn manifest(&self, dir: &Dir) -> Result<Option<Arc<Vec<u8>>>, Missed> {
        match self.file(dir, dir, crate::package_dir::MANIFEST) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Missed::Missing) => Ok(None),
            Err(other) => Err(other),
        }
    }
}
