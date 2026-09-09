//! Filesystem skills: metadata in guidance, instructions loaded on invocation.
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::Deserialize;

const MAX_SKILL_BYTES: u64 = 256 * 1024;

#[derive(Clone, Default)]
pub struct Skills {
    roots: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub disable_model_invocation: bool,
}

#[derive(Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: bool,
}

impl Skills {
    /// Earlier roots win name collisions. Explicit roots override defaults.
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    pub fn for_host(data_dir: &Path) -> anyhow::Result<Self> {
        let mut roots: Vec<PathBuf> = std::env::var_os("HIRSEL_SKILL_DIRS")
            .map(|value| {
                std::env::split_paths(&value)
                    .filter(|p| !p.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default();
        roots.push(data_dir.join("skills"));
        roots.push(std::env::current_dir()?.join(".agents/skills"));
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home).join(".agents/skills"));
        }
        Ok(Self::new(roots))
    }

    /// Scan afresh so installed and edited skills are visible on the next turn.
    pub fn discover(&self) -> Vec<Skill> {
        let mut found = BTreeMap::new();
        let mut visited = HashSet::new();
        for root in &self.roots {
            Self::scan(root, &mut visited, &mut found, 0);
        }
        found.into_values().collect()
    }

    fn scan(
        dir: &Path,
        visited: &mut HashSet<PathBuf>,
        found: &mut BTreeMap<String, Skill>,
        depth: usize,
    ) {
        if depth > 16 {
            return;
        }
        let Ok(real) = dir.canonicalize() else { return };
        if !visited.insert(real.clone()) {
            return;
        }
        let file = real.join("SKILL.md");
        if file.is_file() {
            match read_skill(&file) {
                Ok((skill, _)) => {
                    if let Some(existing) = found.get(&skill.name) {
                        tracing::warn!(name=%skill.name, kept=%existing.path.display(), skipped=%file.display(), "duplicate skill name; using first root");
                    } else {
                        found.insert(skill.name.clone(), skill);
                    }
                }
                Err(error) => {
                    tracing::warn!(path=%file.display(), %error, "skipping invalid skill")
                }
            }
            return;
        }
        let Ok(entries) = std::fs::read_dir(&real) else {
            return;
        };
        let mut dirs = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && !p
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            })
            .collect::<Vec<_>>();
        dirs.sort();
        for child in dirs {
            Self::scan(&child, visited, found, depth + 1);
        }
    }

    pub fn guidance(&self) -> String {
        let skills = self.discover();
        let visible = skills
            .iter()
            .filter(|s| !s.disable_model_invocation)
            .collect::<Vec<_>>();
        if visible.is_empty() {
            return String::new();
        }
        let mut text = String::from(
            "\n\n## Available skills\nSkills are instructions, not separate tools. When a task matches a description, use shell.run to read that SKILL.md before following it. Resolve references and scripts relative to the skill's directory. The Owner can invoke a skill explicitly with /skill:name followed by their request.\n<available_skills>\n",
        );
        for skill in visible {
            text.push_str(&format!("<skill><name>{}</name><description>{}</description><location>{}</location></skill>\n", escape(&skill.name), escape(&skill.description), escape(&skill.path.display().to_string())));
        }
        text.push_str("</available_skills>\n");
        text
    }

    /// Return the original input unless it starts with an explicit skill command.
    /// Unknown/broken commands fail before a message is accepted.
    pub fn expand(&self, text: &str) -> anyhow::Result<String> {
        let Some(command) = text.trim_start().strip_prefix("/skill:") else {
            return Ok(text.to_owned());
        };
        let end = command.find(char::is_whitespace).unwrap_or(command.len());
        let name = &command[..end];
        let args = command[end..].trim_start();
        let skill = self
            .discover()
            .into_iter()
            .find(|s| s.name == name)
            .with_context(|| {
                format!(
                    "Unknown skill '{name}'. Install a SKILL.md under a configured skill directory."
                )
            })?;
        let (current, body) = read_skill(&skill.path)?;
        ensure!(
            current.name == name,
            "Skill changed while loading; invoke it again"
        );
        let base = skill.path.parent().context("skill has no directory")?;
        Ok(format!(
            "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>\n\n{}",
            escape(name),
            escape(&skill.path.display().to_string()),
            base.display(),
            body.trim(),
            args
        ))
    }
}

fn read_skill(path: &Path) -> anyhow::Result<(Skill, String)> {
    let file =
        std::fs::File::open(path).with_context(|| format!("read skill {}", path.display()))?;
    ensure!(file.metadata()?.is_file(), "skill must be a regular file");
    let mut text = String::new();
    file.take(MAX_SKILL_BYTES + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= MAX_SKILL_BYTES,
        "skill exceeds 256 KiB"
    );
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut lines = text.split_inclusive('\n');
    ensure!(
        lines.next().is_some_and(|line| line.trim_end() == "---"),
        "skill requires YAML frontmatter"
    );
    let mut metadata = String::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        metadata.push_str(line);
    }
    ensure!(closed, "skill frontmatter has no closing delimiter");
    let header: Frontmatter =
        serde_yaml_ng::from_str(&metadata).context("invalid skill frontmatter")?;
    ensure!(
        !header.name.is_empty()
            && header.name.len() <= 64
            && header
                .name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            && !header.name.starts_with('-')
            && !header.name.ends_with('-')
            && !header.name.contains("--"),
        "invalid skill name"
    );
    ensure!(
        !header.description.trim().is_empty() && header.description.chars().count() <= 1024,
        "skill requires a description of 1-1024 characters"
    );
    let body = lines.collect::<String>();
    ensure!(!body.trim().is_empty(), "skill instructions are empty");
    Ok((
        Skill {
            name: header.name,
            description: header.description.trim().to_owned(),
            path: path.to_owned(),
            disable_model_invocation: header.disable_model_invocation,
        },
        body,
    ))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, folder: &str, name: &str, extra: &str, body: &str) -> PathBuf {
        let path = root.join(folder).join("SKILL.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("---\nname: {name}\ndescription: >\n  Review <code>\n  carefully.\n{extra}---\n{body}")).unwrap();
        path
    }

    #[test]
    fn guidance_is_metadata_only_and_explicit_invocation_loads_body_and_arguments() {
        let root = tempfile::tempdir().unwrap();
        let path = write(
            root.path(),
            "group/review",
            "review",
            "",
            "Read references/checklist.md.",
        );
        let skills = Skills::new(vec![root.path().to_owned()]);
        let guidance = skills.guidance();
        assert!(guidance.contains("Review &lt;code&gt; carefully."));
        assert!(!guidance.contains("Read references/checklist.md"));
        let expanded = skills.expand("/skill:review\ncheck the diff").unwrap();
        assert!(expanded.contains("Read references/checklist.md."));
        assert!(expanded.contains(&format!(
            "References are relative to {}.",
            path.parent().unwrap().display()
        )));
        assert!(expanded.ends_with("check the diff"));
        assert!(!expanded.contains("description:"));
        assert_eq!(
            skills.expand("explain /skill:review").unwrap(),
            "explain /skill:review"
        );
        assert!(skills.expand("/skill:missing").is_err());
    }

    #[test]
    fn explicit_only_skills_are_hidden_from_guidance_but_still_invocable() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "manual",
            "manual",
            "disable-model-invocation: true\n",
            "Manual instructions",
        );
        let skills = Skills::new(vec![root.path().to_owned()]);
        assert!(skills.guidance().is_empty());
        assert!(
            skills
                .expand("/skill:manual")
                .unwrap()
                .contains("Manual instructions")
        );
    }

    #[test]
    fn roots_have_stable_precedence_and_edits_are_loaded_without_restart() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "first/review", "review", "", "First body");
        write(root.path(), "second/review", "review", "", "Second body");
        let skills = Skills::new(vec![root.path().join("first"), root.path().join("second")]);
        assert_eq!(skills.discover().len(), 1);
        assert!(
            skills
                .expand("/skill:review")
                .unwrap()
                .contains("First body")
        );
        write(root.path(), "first/review", "review", "", "Edited body");
        assert!(
            skills
                .expand("/skill:review")
                .unwrap()
                .contains("Edited body")
        );
    }

    #[test]
    fn symlink_cycles_and_duplicate_paths_terminate_and_invalid_files_are_skipped() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "review", "review", "", "Valid body");
        write(root.path(), "bad", "bad/name", "", "Invalid body");
        std::os::unix::fs::symlink(root.path(), root.path().join("loop")).unwrap();
        let skills = Skills::new(vec![root.path().to_owned(), root.path().join("loop")]);
        assert_eq!(skills.discover().len(), 1);
        assert!(skills.expand("/skill:../bad").is_err());
    }
}
