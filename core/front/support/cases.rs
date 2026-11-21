//! Case discovery and diagnostic expectations shared by the runner and the catalogue build.
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

pub fn files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
        let mut entries = fs::read_dir(dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().is_some_and(|ext| ext == "hirsel") {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut paths = Vec::new();
    walk(dir, &mut paths)?;
    Ok(paths)
}

pub fn expectations(source: &str) -> impl Iterator<Item = &str> {
    source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("// expect: "))
}

impl fmt::Display for DiagnosticExpectation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("// expect: ")?;
        match self {
            Self::Refuse { rule, at } => write!(f, "refuse {rule} at {at}"),
            Self::Cause { rule, at, from } => write!(f, "cause {rule} at {at} from {from}"),
            Self::Compared {
                head,
                declared,
                required,
            } => {
                write!(f, "compared {head} | {declared} | {required}")
            }
            Self::Message { rule, at, text } => write!(f, "message {rule} at {at} | {text}"),
        }
    }
}

pub enum DiagnosticExpectation<'a> {
    Refuse {
        rule: &'a str,
        at: &'a str,
    },
    Cause {
        rule: &'a str,
        at: &'a str,
        from: String,
    },
    Compared {
        head: &'a str,
        declared: &'a str,
        required: &'a str,
    },
    Message {
        rule: &'a str,
        at: &'a str,
        text: String,
    },
}

impl<'a> DiagnosticExpectation<'a> {
    pub fn parse(text: &'a str, words: &[&'a str]) -> Option<Self> {
        Some(match words {
            ["refuse", rule, "at", at] => Self::Refuse { rule, at },
            ["cause", rule, "at", at, "from", from @ ..] => Self::Cause {
                rule,
                at,
                from: from.join(" "),
            },
            ["compared", ..] => {
                let [head, declared, required] = text.split(" | ").collect::<Vec<_>>()[..] else {
                    return None;
                };
                Self::Compared {
                    head: head.trim_start_matches("compared "),
                    declared,
                    required,
                }
            }
            ["message", rule, "at", at, "|", text @ ..] => Self::Message {
                rule,
                at,
                text: text.join(" "),
            },
            _ => return None,
        })
    }
}
