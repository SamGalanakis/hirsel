use super::{Opt, Outcome, Remote, Request, launch, run_compiler_request};
use crate::meter::Stage;
use crate::package_dir::PackageDir;
use crate::package_lock::{Lock, Mode, PackOutput};
use crate::package_resolve::Resolution;
use crate::package_store::PackageStore;
use crate::package_workspace::Workspace;
use hirsel_core_layout::OutputKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub(super) enum PackageInput {
    Discover(Mode, PackageStore),
    Captured(PackageDir),
}

pub(super) fn options(
    invocation: &mut Request<Opt>,
    command: &str,
) -> (Mode, Option<PathBuf>, Option<PathBuf>) {
    let mut package_mode = Mode::default();
    let mut package_store = None;
    let mut pack_out = None;
    for option in invocation.options.extract_if(.., |option| {
        matches!(option, Opt::Locked | Opt::Frozen | Opt::PackageStore(_))
            || (command == "pack" && matches!(option, Opt::Output(_)))
    }) {
        match option {
            Opt::Locked if package_mode != Mode::Frozen => package_mode = Mode::Locked,
            Opt::Frozen => package_mode = Mode::Frozen,
            Opt::PackageStore(value) => package_store = Some(PathBuf::from(value)),
            Opt::Output(value) => pack_out = Some(PathBuf::from(value)),
            _ => {}
        }
    }
    (package_mode, package_store, pack_out)
}

pub(super) fn run(
    invocation: Request<Opt>,
    store: PackageStore,
    mode: Mode,
    output: Option<PathBuf>,
    remote: Option<Arc<Remote>>,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let dir = Path::new(invocation.positional.as_deref().unwrap_or("."));
    let frozen = mode == Mode::Frozen;
    if invocation.command.name == "pack" {
        let output = output.ok_or("pack requires --output FILE")?;
        let workspace = Workspace::discover_pack(dir)?
            .ok_or_else(|| format!("{} has no workspace manifest", dir.display()))?;
        let selected = workspace.selected()?.clone();
        let mut resolution = Resolution::resolve_pack(workspace, store, frozen, &selected)?;
        let lock = Lock::compare(&resolution, mode)?;
        let check = crate::cli::COMMANDS
            .iter()
            .find(|spec| spec.name == "check")
            .unwrap();
        for member in resolution.pack_checks(&selected) {
            let manifest = &resolution.workspace.members[member].manifest_path;
            let checked = (|| {
                let app = PackageDir::from_resolution(&resolution, member, true)?;
                let invocation = Request {
                    command: check,
                    options: Vec::new(),
                    positional: Some(manifest.parent().unwrap().to_str().unwrap().to_owned()),
                    arguments: Vec::new(),
                };
                run_compiler_request(
                    invocation,
                    check.name,
                    OutputKind::Executable,
                    false,
                    None,
                    None,
                    PackageInput::Captured(app),
                    remote.clone(),
                )
            })();
            match checked {
                Ok(Outcome::Exit(0)) => (),
                other => {
                    if let Err(error) = other {
                        launch::report_request_error(error.as_ref());
                    }
                    return Err(format!(
                        "pack: {} does not check; nothing was written",
                        manifest.display()
                    )
                    .into());
                }
            }
        }
        let (bytes, archive) = resolution.make_archive(&selected)?;
        let output = PackOutput::stage(&resolution, &archive, &lock, &output)?;
        lock.commit(&resolution.workspace)?;
        resolution.store.install(&archive)?;
        output.write(&bytes)?;
        let head = &archive.entries[&archive.head].content;
        println!("{} {} {}", head.id, head.version, archive.head);
        for (root, library) in &archive.entries {
            if *root != archive.head {
                println!("{} {} {root}", library.content.id, library.content.version);
            }
        }
    } else {
        let workspace = Workspace::discover_for(dir, false)?
            .ok_or_else(|| format!("{} has no workspace manifest", dir.display()))?;
        let resolution = Resolution::resolve(workspace, store, frozen)?;
        Lock::compare(&resolution, mode)?.commit(&resolution.workspace)?;
    }
    launch::conclude_with_remote(Outcome::Exit(0), remote.as_deref())
}

pub(super) fn answer_check(
    product: &crate::CheckProduct,
    sources: &crate::SourceFiles,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    if product.accepted() {
        return Ok(Outcome::Exit(0));
    }
    super::output::render_check_diagnostics(product, sources)?;
    Ok(Outcome::Exit(2))
}

pub(super) struct PackageStages(Option<Vec<Stage>>);

impl PackageStages {
    pub(super) fn new(enabled: bool) -> Self {
        Self(enabled.then(Vec::new))
    }

    pub(super) fn measure<T>(&mut self, name: &'static str, work: impl FnOnce() -> T) -> T {
        let Some(stages) = &mut self.0 else {
            return work();
        };
        let start = Instant::now();
        let value = work();
        stages.push(Stage {
            name,
            nanoseconds: start.elapsed().as_nanos(),
            instructions: None,
        });
        value
    }

    pub(super) fn into_stages(self) -> Vec<Stage> {
        self.0.unwrap_or_default()
    }
}
