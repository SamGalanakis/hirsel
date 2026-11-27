//! `git+https://HOST/PATH[?dir=DIR][#REF]` and `github:OWNER/REPO[/DIR][@REF]`: a commit of a
//! repository, its reference resolved by one listing (GIT-10) and checked against the binding
//! it would replace (GIT-11).

use super::Failure;
use crate::package_content::Root;
use crate::package_git::{Budget, Listed, Remote, list_refs};
use crate::package_manifest::{GitSource, git_dir_rule, git_rev_rule, git_url_rule};

const FORMS: &str = "git+https://HOST/PATH[?dir=DIR][#REF] or github:OWNER/REPO[/DIR][@REF]";

/// A git source as typed: the repository, the package's directory and the reference.
pub(super) struct Request {
    pub(super) url: String,
    pub(super) dir: Option<String>,
    /// `REF` as typed, `None` for the default branch.
    pub(super) reference: Option<String>,
}

fn refuse(text: &str, part: impl std::fmt::Display) -> Failure {
    format!("{text} is not {FORMS}: {part}").into()
}

fn dir_part(text: &str, dir: &str) -> Result<Option<String>, Failure> {
    git_dir_rule(dir).map_err(|error| refuse(text, format!("dir {dir:?} {error}")))?;
    Ok(Some(dir.to_string()))
}

fn reference_part(text: &str, reference: Option<&str>) -> Result<Option<String>, Failure> {
    match reference {
        Some("") => Err(refuse(text, "the REF is empty")),
        other => Ok(other.map(str::to_string)),
    }
}

impl Request {
    /// `git+https://HOST/PATH[?dir=DIR][#REF]`.
    pub(super) fn parse_git(text: &str) -> Result<Self, Failure> {
        let rest = text.strip_prefix("git+").expect("the caller matched git+");
        if !rest.starts_with("https://") {
            let scheme = rest.split_once("://").map_or(rest, |(scheme, _)| scheme);
            return Err(refuse(text, format!("the scheme {scheme:?} is not https")));
        }
        let (rest, reference) = match rest.split_once('#') {
            Some((rest, reference)) => (rest, Some(reference)),
            None => (rest, None),
        };
        let (url, dir) = match rest.split_once('?') {
            Some((url, query)) => match query.strip_prefix("dir=") {
                Some(dir) if !dir.contains(['&', '=']) => (url, dir_part(text, dir)?),
                _ => return Err(refuse(text, "the only query is one dir=DIR")),
            },
            None => (rest, None),
        };
        git_url_rule(url).map_err(|error| refuse(text, format!("the URL {error}")))?;
        Ok(Self {
            url: url.to_string(),
            dir,
            reference: reference_part(text, reference)?,
        })
    }

    /// `github:OWNER/REPO[/DIR][@REF]`, exactly `git+https://github.com/OWNER/REPO?dir=DIR#REF`.
    pub(super) fn parse_github(text: &str) -> Result<Self, Failure> {
        let rest = text
            .strip_prefix("github:")
            .expect("the caller matched github:");
        let (path, reference) = match rest.split_once('@') {
            Some((path, reference)) => (path, Some(reference)),
            None => (rest, None),
        };
        let mut parts = path.splitn(3, '/');
        let owner = parts.next().unwrap_or("");
        let repo = parts
            .next()
            .ok_or_else(|| refuse(text, "it names no REPO"))?;
        if owner.is_empty()
            || !owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(refuse(
                text,
                format!("OWNER {owner:?} is not [A-Za-z0-9-]+"),
            ));
        }
        if repo.is_empty()
            || repo == "."
            || repo == ".."
            || !repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(refuse(
                text,
                format!("REPO {repo:?} is not [A-Za-z0-9._-]+"),
            ));
        }
        let dir = match parts.next() {
            Some(dir) => dir_part(text, dir)?,
            None => None,
        };
        Ok(Self {
            url: format!("https://github.com/{owner}/{repo}"),
            dir,
            reference: reference_part(text, reference)?,
        })
    }

    /// The commit the reference names now, with the full ref name and the remote the listing
    /// opened (GIT-10). A commit id is taken as written and nothing is listed.
    pub(super) fn resolve(
        &self,
        transport: &crate::package_store::Transport,
        budget: &mut Budget,
    ) -> Result<(GitSource, Option<Remote>, Option<String>), Failure> {
        let source = |rev: String, reference: Option<String>| GitSource {
            url: self.url.clone(),
            rev,
            dir: self.dir.clone(),
            reference,
        };
        if let Some(rev) = &self.reference
            && git_rev_rule(rev)
        {
            return Ok((source(rev.clone(), None), None, None));
        }
        let candidates: Vec<String> = match self.reference.as_deref() {
            None => vec!["HEAD".into()],
            Some(full) if full.starts_with("refs/heads/") || full.starts_with("refs/tags/") => {
                vec![full.to_string()]
            }
            Some(short) => vec![format!("refs/tags/{short}"), format!("refs/heads/{short}")],
        };
        let (remote, listed) = list_refs(transport, &self.url, &candidates, budget)
            .map_err(|failure| failure.explain_after("", &self.url))?;
        let found = |name: &str| listed.iter().find(|entry| entry.name == name);
        let commit = |entry: &Listed| entry.peeled.unwrap_or(entry.id).to_string();
        let (rev, name) = match self.reference.as_deref() {
            None => {
                let head = found("HEAD");
                match head.and_then(|head| head.symref.as_deref()) {
                    Some(target) if target.starts_with("refs/heads/") => {
                        (commit(head.unwrap()), target.to_string())
                    }
                    _ => {
                        return Err(format!(
                            "{}: HEAD names no branch; give a branch, a tag or a commit",
                            self.url
                        )
                        .into());
                    }
                }
            }
            Some(reference) => {
                let matches: Vec<&Listed> =
                    candidates.iter().filter_map(|name| found(name)).collect();
                match matches[..] {
                    [] => {
                        return Err(format!("{} has no branch or tag {reference}", self.url).into());
                    }
                    [only] => (commit(only), only.name.clone()),
                    _ => {
                        return Err(format!(
                            "{}: {reference} names both refs/tags/{reference} and refs/heads/{reference}; write the full name",
                            self.url
                        )
                        .into());
                    }
                }
            }
        };
        let chose = self
            .reference
            .is_none()
            .then(|| format!("HEAD of {} is {name} at {rev}", self.url));
        Ok((source(rev, Some(name)), Some(remote), chose))
    }
}

/// GIT-11: `fetched` against a binding of the same source, the pair (`git`, `dir`). A bound
/// reference that now names another commit refuses without `--update`; another reference or a
/// commit id is a named update.
pub(super) fn check_moved(
    alias: &str,
    bound: &GitSource,
    bound_root: Root,
    fetched: &GitSource,
    fetched_root: Root,
) -> Result<(), Failure> {
    if bound.url != fetched.url || bound.dir != fetched.dir {
        return Ok(());
    }
    let Some(reference) = &fetched.reference else {
        return Ok(());
    };
    if bound.reference.as_ref() != Some(reference)
        || (bound.rev == fetched.rev && bound_root == fetched_root)
    {
        return Ok(());
    }
    let what = if reference.starts_with("refs/tags/") {
        "moved"
    } else {
        "now names another commit"
    };
    Err(format!(
        "{} {reference} {what}:\n  bound: {} {bound_root}\n  fetched: {} {fetched_root}\nto take it, run: hirsel update {alias}",
        fetched.url, bound.rev, fetched.rev
    )
    .into())
}
