//! Immutable build binding and the separate launch approval for Exec.
use crate::{NativeOptions, NativeStop};
use hirsel_core_net::exec::Request;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;

impl NativeOptions {
    pub(crate) fn with_bound_exec(&self) -> Result<Cow<'_, Self>, NativeStop> {
        if self.exec_grants.iter().any(|grant| grant.policy.is_none()) {
            let mut bound = self.clone();
            bound.bind_exec()?;
            Ok(Cow::Owned(bound))
        } else {
            Ok(Cow::Borrowed(self))
        }
    }
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct Binding {
    pub(crate) request: Request,
    pub(crate) policy: Option<[u8; 32]>,
    pub(crate) workdir: Option<PathBuf>,
}

impl Binding {
    pub(crate) fn digest(&self) -> Result<[u8; 32], NativeStop> {
        self.policy.map_or_else(
            || {
                self.request
                    .load()
                    .map(|p| p.digest)
                    .map_err(NativeStop::Entry)
            },
            Ok,
        )
    }

    pub(crate) fn seal_policy(&mut self) -> Result<(), NativeStop> {
        self.policy = Some(self.digest()?);
        Ok(())
    }

    pub(crate) fn argument(&self) -> String {
        format!("--hirsel-exec={}", self.request.text())
    }

    pub(crate) fn approve(&self, command: &mut Command) {
        command.arg(self.argument());
    }

    pub(crate) fn refusal(&self) -> String {
        format!(
            "not granted: Exec[{}]: pass --grant 'Exec={}' to hirsel test",
            self.request.names.join(","),
            self.request.text()
        )
    }
}

use hirsel_core_ir::release_recipe::{GrantKind, Recipe};
use std::collections::BTreeSet;

fn configured(recipe: &Recipe) -> impl Iterator<Item = (String, [u8; 32])> + '_ {
    recipe
        .launch
        .grants
        .iter()
        .filter(|grant| grant.kind == GrantKind::Exec)
        .filter_map(|grant| {
            let [file, names @ ..] = grant.values.as_slice() else {
                return None;
            };
            Some((format!("{file} {}", names.join(",")), grant.policy?.0))
        })
}

pub(crate) fn arguments(recipe: &Recipe) -> impl Iterator<Item = String> + '_ {
    configured(recipe).map(|(request, _)| format!("--hirsel-exec={request}"))
}

pub(crate) fn refuse_missing(recipe: &Recipe, approved: &BTreeSet<String>) -> Option<String> {
    let (request, _) = configured(recipe).find(|(request, _)| !approved.contains(request))?;
    Some(format!(
        "the recipe grants Exec={request}; pass --grant 'Exec={request}'"
    ))
}

pub(crate) fn approval_lines(recipe: &Recipe, workdir: &Path) -> Result<String, String> {
    let mut lines = String::new();
    for (request, expected) in configured(recipe) {
        let request = Request::parse(&request)?;
        let policy = request.load_from(workdir)?;
        if policy.digest != expected {
            return Err(format!(
                "Exec[{}]: launch policy digest differs from the build binding",
                request.names.join(",")
            ));
        }
        let (_, approved) = policy.pin_images()?;
        lines.extend(approved);
    }
    Ok(lines)
}
