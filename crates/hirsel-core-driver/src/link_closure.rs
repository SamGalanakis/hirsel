//! The files a link recipe makes the link editor read, and the paths whose
//! absence its searches rely on. Every argument of a recipe is parsed by one
//! table ([`OPTIONS`]); an argument the table does not know refuses the
//! recipe, so no flag reaches ld.lld with an operand the key does not hold.
//!
//! The closure follows ld.lld's rules (lld/ELF Driver.cpp and
//! ScriptParser.cpp): every `-L` directory applies to every library,
//! wherever it stands; `-l` takes the first `lib<name>.so` (unless static)
//! or `lib<name>.a` along them, `-l:<file>` the first `<file>`; a file that
//! is neither ELF, an archive nor bitcode is a linker script, whose
//! `INPUT`, `GROUP`, `AS_NEEDED` and `INCLUDE` operands are read in turn: an
//! absolute name as is (under the sysroot when the script is), `=` under
//! the sysroot, `-l` as a library, and a relative name beside the script,
//! then in the working directory, then along the search directories. The
//! link runs in an empty directory of its own, so the working directory
//! holds nothing. Each path a search tried before its match is recorded as
//! absent: a file that later appears there would be read instead, so the
//! recipe is stale once one does.
use std::io;
use std::path::{Component, Path, PathBuf};

/// How an option uses its operand.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// No operand.
    Flag,
    /// An operand that names no file.
    Value,
    /// A file read as data: its bytes are keyed and it names nothing.
    Read,
    /// A positional input: an object, archive or shared object, or a linker
    /// script, which is followed.
    Input,
    /// A linker script given with `-T`: searched like a script's relative
    /// name, keyed and followed.
    Script,
    Library,
    SearchDirectory,
    Sysroot,
}

/// Every option a recipe may carry, by name without its dashes (ld.lld
/// accepts `-name`, `--name`, `--name=value` and `--name value` alike). The
/// one-letter options with a joined operand (`-Ldir`, `-lc`, `-zrelro`) are
/// in [`SHORT`]. Options that write a file (`--Map`, `--reproduce`,
/// `--dependency-file`) and options that load code (`--load-pass-plugins`)
/// are not here: a recipe naming one is refused.
const OPTIONS: &[(&str, Kind)] = &[
    ("(", Kind::Flag),
    (")", Kind::Flag),
    ("Bdynamic", Kind::Flag),
    ("Bstatic", Kind::Flag),
    ("Bsymbolic", Kind::Flag),
    ("Bsymbolic-functions", Kind::Flag),
    ("EB", Kind::Flag),
    ("EL", Kind::Flag),
    ("X", Kind::Flag),
    ("allow-shlib-undefined", Kind::Flag),
    ("as-needed", Kind::Flag),
    ("build-id", Kind::Flag),
    ("call_shared", Kind::Flag),
    ("color-diagnostics", Kind::Flag),
    ("disable-new-dtags", Kind::Flag),
    ("discard-all", Kind::Flag),
    ("discard-locals", Kind::Flag),
    ("dn", Kind::Flag),
    ("dy", Kind::Flag),
    ("dynamic-linker", Kind::Value),
    ("eh-frame-hdr", Kind::Flag),
    ("enable-new-dtags", Kind::Flag),
    ("end-group", Kind::Flag),
    ("export-dynamic", Kind::Flag),
    ("E", Kind::Flag),
    ("fatal-warnings", Kind::Flag),
    ("fix-cortex-a53-843419", Kind::Flag),
    ("gc-sections", Kind::Flag),
    ("hash-style", Kind::Value),
    ("defsym", Kind::Value),
    ("dynamic-list", Kind::Read),
    ("export-dynamic-symbol", Kind::Value),
    ("entry", Kind::Value),
    ("library", Kind::Library),
    ("library-path", Kind::SearchDirectory),
    ("m", Kind::Value),
    ("no-allow-shlib-undefined", Kind::Flag),
    ("no-as-needed", Kind::Flag),
    ("no-eh-frame-hdr", Kind::Flag),
    ("no-gc-sections", Kind::Flag),
    ("no-pie", Kind::Flag),
    ("no-relax", Kind::Flag),
    ("no-undefined", Kind::Flag),
    ("no-whole-archive", Kind::Flag),
    ("non_shared", Kind::Flag),
    ("nostdlib", Kind::Flag),
    ("pic-veneer", Kind::Flag),
    ("pie", Kind::Flag),
    ("pop-state", Kind::Flag),
    ("push-state", Kind::Flag),
    ("relax", Kind::Flag),
    ("rpath", Kind::Value),
    ("rpath-link", Kind::Value),
    ("script", Kind::Script),
    ("shared", Kind::Flag),
    ("soname", Kind::Value),
    ("start-group", Kind::Flag),
    ("static", Kind::Flag),
    ("strip-all", Kind::Flag),
    ("strip-debug", Kind::Flag),
    ("sysroot", Kind::Sysroot),
    ("undefined", Kind::Value),
    ("version-script", Kind::Read),
    ("whole-archive", Kind::Flag),
    ("wrap", Kind::Value),
    ("s", Kind::Flag),
    ("S", Kind::Flag),
    ("x", Kind::Flag),
];
/// One-letter options whose operand may be joined (`-Ldir`) or follow.
const SHORT: &[(char, Kind)] = &[
    ('L', Kind::SearchDirectory),
    ('l', Kind::Library),
    ('T', Kind::Script),
    ('z', Kind::Value),
    ('m', Kind::Value),
    ('h', Kind::Value),
    ('u', Kind::Value),
    ('e', Kind::Value),
    ('O', Kind::Value),
];
/// Options whose operand only ever follows, never a separate word: `-O1`,
/// `--build-id=sha1`.
fn joined_only(name: &str) -> bool {
    matches!(name, "O" | "build-id")
}

/// The files a recipe's link reads and the paths its searches found absent,
/// each sorted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Closure {
    pub files: Vec<PathBuf>,
    /// The binary inputs, including those named by a linker script.
    pub binaries: Vec<PathBuf>,
    pub absent: Vec<PathBuf>,
}

/// The closure of `args`, with `support` (libraries the link appends at the
/// input marker) read where the marker stands. The marker's own inputs are
/// the request's objects, keyed by the request.
pub(crate) fn closure(
    args: &[String],
    marker: &str,
    support: &[&str],
    require_libraries: bool,
) -> io::Result<Closure> {
    let refuse = |what: String| io::Error::other(format!("link recipe refused: {what}"));
    // First pass: the options, the search directories and the sysroot,
    // which hold for every library wherever they stand.
    let mut words: Vec<(Kind, &str)> = Vec::new();
    let mut sysroot: Option<PathBuf> = None;
    let mut directories: Vec<&str> = Vec::new();
    let expanded = args.iter().flat_map(|arg| {
        if arg == marker {
            support.iter().map(|s| s.to_string()).collect::<Vec<_>>()
        } else {
            vec![arg.clone()]
        }
    });
    let expanded: Vec<String> = expanded.collect();
    let mut i = 0;
    while i < expanded.len() {
        let arg = expanded[i].as_str();
        i += 1;
        if arg.starts_with('@') {
            return Err(refuse(format!("response file {arg}")));
        }
        let Some(body) = arg.strip_prefix('-') else {
            words.push((Kind::Input, arg));
            continue;
        };
        if arg == "-" {
            return Err(refuse("standard input".into()));
        }
        let body = body.strip_prefix('-').unwrap_or(body);
        let (name, joined) = match body.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (body, None),
        };
        let (kind, joined) = match OPTIONS.iter().find(|(n, _)| *n == name) {
            Some((_, kind)) => (*kind, joined),
            None => {
                let mut chars = body.chars();
                let first = chars.next().unwrap_or('-');
                match SHORT.iter().find(|(c, _)| *c == first) {
                    // `-lc`, `-zrelro`, `-melf_x86_64`: the rest is the operand.
                    Some((_, kind)) if !arg.starts_with("--") && body.len() > 1 => {
                        (*kind, Some(chars.as_str()))
                    }
                    Some((_, kind)) if body.len() == 1 => (*kind, joined),
                    _ => return Err(refuse(format!("unknown link option {arg}"))),
                }
            }
        };
        let operand = match (kind, joined) {
            (Kind::Flag, Some(_)) if !joined_only(name) => {
                return Err(refuse(format!("{arg} takes no operand")));
            }
            (Kind::Flag, _) => None,
            (_, Some(value)) => Some(value),
            (_, None) if joined_only(name) => None,
            (_, None) => {
                let value = expanded
                    .get(i)
                    .ok_or_else(|| refuse(format!("{arg} has no operand")))?;
                i += 1;
                Some(value.as_str())
            }
        };
        match (kind, operand) {
            (Kind::SearchDirectory, Some(dir)) => directories.push(dir),
            (Kind::Sysroot, Some(root)) => sysroot = Some(PathBuf::from(root)),
            (Kind::Flag, _) => words.push((Kind::Flag, name)),
            (Kind::Read | Kind::Input | Kind::Script | Kind::Library, Some(operand)) => {
                words.push((kind, operand))
            }
            _ => {}
        }
    }
    // Relative names would resolve in the link's empty directory.
    let mut search = Vec::with_capacity(directories.len());
    for dir in directories {
        let dir = match (dir.strip_prefix('='), &sysroot) {
            (Some(rest), Some(root)) => root.join(rest.trim_start_matches('/')),
            _ => PathBuf::from(dir),
        };
        if !dir.is_absolute() {
            return Err(refuse(format!(
                "relative search directory {}",
                dir.display()
            )));
        }
        search.push(dir);
    }
    let mut walk = Walk {
        search,
        sysroot,
        files: Vec::new(),
        binaries: Vec::new(),
        absent: Vec::new(),
        stack: Vec::new(),
        require_libraries,
    };
    let mut statics = vec![false];
    for (kind, word) in words {
        match kind {
            Kind::Flag => match word {
                "Bstatic" | "static" | "dn" | "non_shared" => *statics.last_mut().unwrap() = true,
                "Bdynamic" | "dy" | "call_shared" => *statics.last_mut().unwrap() = false,
                "push-state" => statics.push(*statics.last().unwrap()),
                "pop-state" if statics.len() > 1 => {
                    statics.pop();
                }
                _ => {}
            },
            Kind::Read | Kind::Input => {
                let path = Path::new(word);
                if !path.is_absolute() {
                    return Err(refuse(format!("relative link input {word}")));
                }
                if kind == Kind::Read {
                    walk.read(path)?;
                } else {
                    walk.input(path, false)?;
                }
            }
            Kind::Script => match walk.script_name(word)? {
                Some(path) => walk.input(&path, true)?,
                None => return Err(missing(word)),
            },
            Kind::Library => {
                if let Some(path) = walk.library(word, *statics.last().unwrap())? {
                    walk.input(&path, false)?;
                }
            }
            _ => {}
        }
    }
    walk.files.sort();
    walk.files.dedup();
    walk.binaries.sort();
    walk.binaries.dedup();
    walk.absent.sort();
    walk.absent.dedup();
    Ok(Closure {
        files: walk.files,
        binaries: walk.binaries,
        absent: walk.absent,
    })
}

fn missing(input: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("missing recipe inputs: {input}"),
    )
}

struct Walk {
    search: Vec<PathBuf>,
    sysroot: Option<PathBuf>,
    files: Vec<PathBuf>,
    binaries: Vec<PathBuf>,
    absent: Vec<PathBuf>,
    /// The scripts being read, so an `INCLUDE` cycle ends.
    stack: Vec<PathBuf>,
    require_libraries: bool,
}

impl Walk {
    /// The first of `candidates` that exists; those before it are recorded
    /// absent.
    fn first(&mut self, candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
        for candidate in candidates {
            if candidate.exists() {
                return Some(candidate);
            }
            self.absent.push(candidate);
        }
        None
    }

    fn library(&mut self, name: &str, statics: bool) -> io::Result<Option<PathBuf>> {
        let search = self.search.clone();
        let candidates: Vec<PathBuf> = match name.strip_prefix(':') {
            Some(file) => search.iter().map(|dir| dir.join(file)).collect(),
            None => search
                .iter()
                .flat_map(|dir| {
                    let shared = (!statics).then(|| dir.join(format!("lib{name}.so")));
                    shared.into_iter().chain([dir.join(format!("lib{name}.a"))])
                })
                .collect(),
        };
        let found = self.first(candidates);
        if found.is_none() && self.require_libraries {
            let files = match name.strip_prefix(':') {
                Some(file) => file.to_owned(),
                None if statics => format!("lib{name}.a"),
                None => format!("lib{name}.so or lib{name}.a"),
            };
            return Err(missing(files));
        }
        Ok(found)
    }

    /// A `-T` or `INCLUDE` name: as given (absolute, or relative to the
    /// empty working directory), else along the search directories.
    fn script_name(&mut self, name: &str) -> io::Result<Option<PathBuf>> {
        let path = Path::new(name);
        if path.is_absolute() {
            return Ok(Some(path.to_owned()));
        }
        let search = self.search.clone();
        Ok(self.first(search.iter().map(|dir| dir.join(name))))
    }

    /// Key `path`, a file read as data.
    fn read(&mut self, path: &Path) -> io::Result<()> {
        if !path.is_file() {
            return Err(missing(path.display()));
        }
        self.files.push(path.to_owned());
        Ok(())
    }

    /// Key `path`, and follow it when it is a linker script (`script`: it
    /// is one whatever its first bytes).
    fn input(&mut self, path: &Path, script: bool) -> io::Result<()> {
        let refuse = |what: String| io::Error::other(format!("link recipe refused: {what}"));
        self.read(path)?;
        let mut head = [0u8; 8];
        let read = {
            use std::io::Read;
            let mut file = std::fs::File::open(path)?;
            let mut n = 0;
            while n < head.len() {
                match file.read(&mut head[n..])? {
                    0 => break,
                    k => n += k,
                }
            }
            n
        };
        let head = &head[..read];
        if head.starts_with(b"!<thin>\n") {
            return Err(refuse(format!(
                "thin archive {}, whose members are other files",
                path.display()
            )));
        }
        let binary = head.starts_with(b"\x7fELF")
            || head.starts_with(b"!<arch>\n")
            || head.starts_with(b"BC\xc0\xde")
            || head.starts_with(b"\xde\xc0\x17\x0b");
        if binary && !script {
            self.binaries.push(path.to_owned());
            return Ok(());
        }
        if self.stack.iter().any(|p| p == path) {
            return Ok(());
        }
        let text = std::fs::read(path)?;
        let text = String::from_utf8_lossy(&text);
        self.stack.push(path.to_owned());
        let result = self.script(path, &text);
        self.stack.pop();
        result
    }

    fn script(&mut self, path: &Path, text: &str) -> io::Result<()> {
        let refuse = |what: String| {
            io::Error::other(format!(
                "link recipe refused: linker script {} {what}",
                path.display()
            ))
        };
        let tokens = tokens(text);
        let under_sysroot = self.sysroot.as_ref().is_some_and(|root| {
            !root.as_os_str().is_empty() && normal(path).starts_with(normal(root))
        });
        let mut i = 0;
        while i < tokens.len() {
            match tokens[i].as_str() {
                "SEARCH_DIR" => return Err(refuse("adds a search directory".into())),
                "INCLUDE" => {
                    let name = tokens
                        .get(i + 1)
                        .ok_or_else(|| refuse("ends inside INCLUDE".into()))?
                        .clone();
                    match self.script_name(&name)? {
                        Some(found) => self.input(&found, true)?,
                        None => return Err(missing(name)),
                    }
                    i += 2;
                }
                "INPUT" | "GROUP" if tokens.get(i + 1).map(String::as_str) == Some("(") => {
                    let mut depth = 0;
                    i += 1;
                    while i < tokens.len() {
                        match tokens[i].as_str() {
                            "(" => depth += 1,
                            ")" => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            "AS_NEEDED" | "," => {}
                            name => self.script_file(path, name, under_sysroot)?,
                        }
                        i += 1;
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
        Ok(())
    }

    /// One `INPUT` or `GROUP` operand of the script at `script`.
    fn script_file(&mut self, script: &Path, name: &str, under_sysroot: bool) -> io::Result<()> {
        let found = if let Some(library) = name.strip_prefix("-l") {
            self.library(library, false)?
        } else if name.starts_with('/') {
            match (&self.sysroot, under_sysroot) {
                (Some(root), true) => Some(root.join(name.trim_start_matches('/'))),
                _ => Some(PathBuf::from(name)),
            }
        } else if let Some(rest) = name.strip_prefix('=') {
            Some(match &self.sysroot {
                Some(root) => root.join(rest.trim_start_matches('/')),
                None => PathBuf::from(rest),
            })
        } else {
            let beside = script.parent().map(|dir| dir.join(name));
            let search = self.search.clone();
            self.first(
                beside
                    .into_iter()
                    .chain(search.iter().map(|dir| dir.join(name))),
            )
        };
        match found {
            Some(found) => self.input(&found, false),
            None if self.require_libraries => Err(missing(name)),
            None => Ok(()),
        }
    }
}

/// `path` with `.` and `..` resolved lexically.
fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// A linker script's tokens: words, quoted strings and the punctuation
/// `(`, `)`, `,`, `;`, `{`, `}`; comments (`/* */` and `#` to the end of the
/// line) are dropped.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<String>| {
        if !word.is_empty() {
            out.push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'*') => {
                flush(&mut word, &mut out);
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            '#' if word.is_empty() => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                flush(&mut word, &mut out);
                let mut quoted = String::new();
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                    quoted.push(c);
                }
                out.push(quoted);
            }
            '(' | ')' | ',' | ';' | '{' | '}' => {
                flush(&mut word, &mut out);
                out.push(c.to_string());
            }
            c if c.is_whitespace() => flush(&mut word, &mut out),
            c => word.push(c),
        }
    }
    flush(&mut word, &mut out);
    out
}
