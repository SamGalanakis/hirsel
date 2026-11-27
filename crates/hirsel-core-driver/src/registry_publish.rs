//! Publish captured library content to the registry's authenticated release log.

use crate::cli::{RegistryPublishOption, Request};
use crate::package_content::Id;
use crate::package_manifest::name::scoped;
use crate::package_resolve::Resolution;
use crate::package_store::PackageStore;
use crate::package_workspace::Workspace;
use crate::registry_fetch::now;
use crate::registry_verify::{Release, decimal};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

mod auth;
mod plan;
mod wire;

#[cfg(test)]
mod laws;

type Failure = Box<dyn std::error::Error>;
const METADATA_LIMIT: usize = 1 << 20;

#[derive(Default)]
struct Options {
    workspace: bool,
    description: String,
    source_commit: Option<String>,
    package_store: Option<PathBuf>,
    libraries: Option<String>,
    generation: Option<String>,
    enroll: Option<String>,
    key: Option<String>,
    workload: Option<String>,
    token_command: Option<String>,
}

impl Options {
    fn read(options: Vec<RegistryPublishOption>) -> Result<Self, Failure> {
        use RegistryPublishOption::*;
        let mut opts = Self::default();
        for option in options {
            match option {
                PublishWorkspace => opts.workspace = true,
                PublishDescription(value) => opts.description = value,
                PublishSourceCommit(value) => {
                    if !matches!(value.len(), 40 | 64) || !value.bytes().all(lower_hex) {
                        return Err(
                            "--source-commit requires a lowercase 40- or 64-digit Git object name"
                                .into(),
                        );
                    }
                    opts.source_commit = Some(value);
                }
                PackageStore(value) => opts.package_store = Some(value.into()),
                Libraries(value) => opts.libraries = Some(value),
                PublishGeneration(value) => opts.generation = Some(value),
                PublishEnroll(value) => opts.enroll = Some(value),
                PublishKey(value) => opts.key = Some(value),
                PublishWorkload(value) => opts.workload = Some(value),
                PublishTokenCommand(value) => opts.token_command = Some(value),
            }
        }
        Ok(opts)
    }
}

fn lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

fn hex(bytes: &[u8]) -> String {
    hirsel_core_ids::hex::encode(bytes)
}

pub(crate) fn run(
    request: Request<RegistryPublishOption>,
    mut check: impl FnMut(&Resolution, &Id, Option<&str>) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let mut options = Options::read(request.options)?;
    let dir = Path::new(request.positional.as_deref().unwrap_or("."));
    let workspace = Workspace::discover_for(dir, !options.workspace)?
        .ok_or_else(|| format!("{} has no workspace manifest", dir.display()))?;
    let order = plan::order(&workspace, options.workspace)?;
    let store = PackageStore::new(options.package_store.take());
    for id in &order {
        if !scoped(&id.name) {
            return Err(format!(
                "{}: {id} needs a scoped package name to publish",
                workspace.members[id].manifest_path.display()
            )
            .into());
        }
        workspace.members[id].check_pins()?;
        if options.workspace || workspace.member.as_ref() == Some(id) {
            store.registry(&id.name, None)?;
        }
    }
    let authorizer = auth::Authorizer::load(
        options.key.take(),
        options.workload.take(),
        options.token_command.take(),
    )?;
    let generation = auth::generation(options.generation.as_deref(), options.enroll.is_some())?;
    let enrollment = options
        .enroll
        .as_deref()
        .map(|path| auth::enrollment(path, &authorizer.subject))
        .transpose()?;
    let (jobs, mut frames, mut history, store) = {
        let mut resolution = Resolution::resolve(workspace, store, false)?;
        for id in &order {
            check(&resolution, id, options.libraries.as_deref())?;
        }
        let (jobs, frames, history) = plan::prepare(&mut resolution, order, options.workspace)?;
        (jobs, frames, history, resolution.store)
    };

    let mut scopes = BTreeSet::new();
    for (i, job) in jobs.iter().enumerate() {
        let result = (|| {
            let client = store.registry(&job.id.name, None)?;
            let origin = client.route().origin().to_string();
            let scope = job.id.name.rsplit_once('/').expect("scoped member").0;
            let first = scopes.insert((origin.clone(), scope));
            let initial = enrollment.as_ref().filter(|_| first);
            let generation = if enrollment.is_some() && !first {
                1
            } else {
                generation
            };
            let root = hex(&job.root.0);
            let intent = json!([
                "hirsel-intent-v1",
                origin,
                "publish",
                scope,
                generation.to_string(),
                root,
                [
                    job.id.to_string(),
                    job.version.to_string(),
                    options.description,
                    options.source_commit,
                    initial
                ],
            ]);
            let children = job
                .logged
                .iter()
                .map(|root| {
                    history
                        .get(root)
                        .map(release_hint)
                        .ok_or_else(|| format!("{root}: child release was not confirmed"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let proof = authorizer.prove(&origin, &root, &serde_json::to_vec(&intent)?)?;
            let frame_roots: Vec<_> = job.frames.iter().map(|root| hex(&root.0)).collect();
            let metadata = json!([intent, authorizer.subject, proof, frame_roots, children]);
            let receipt = wire::submit(
                &store,
                &client,
                job.root,
                &metadata,
                &job.frames,
                &mut frames,
            )?;
            let release = receipt.verify(&client, &job.id, &job.version, job.root)?;
            history.insert(job.root, release);
            println!(
                "{} {} {} ({}/log, leaf {}, head {})",
                job.id,
                job.version,
                job.root,
                origin.strip_prefix("https://").expect("HTTPS route"),
                receipt.index,
                receipt.head_size
            );
            Ok::<(), Failure>(())
        })();
        if let Err(error) = result {
            return Err(format!(
                "registry publish: failed {}: {error}\n  committed: {}\n  unattempted: {}",
                job.id,
                members(&jobs[..i]),
                members(&jobs[i + 1..]),
            )
            .into());
        }
    }
    Ok(())
}

fn release_hint(release: &Release) -> Value {
    json!([
        release.id().to_string(),
        release.version().to_string(),
        hex(&release.root().0),
        release.index().to_string(),
    ])
}

fn members(jobs: &[plan::Job]) -> String {
    if jobs.is_empty() {
        return "none".into();
    }
    jobs.iter()
        .map(|job| job.id.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
