//! EMB-5 builds the Apple archives first, then publishes one Swift package.
use crate::cli::{Request, SdkOption};
use crate::sdk_command::Options;
use hirsel_core_ids::embed::binding_projection;
use hirsel_core_layout::{AppleRecord, Target};
use object::read::archive::ArchiveFile;
use object::{Object, ObjectSection, ObjectSymbol};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "sdk_swift/codec.rs"]
mod codec;
#[path = "sdk_swift/entries.rs"]
mod entries;
#[path = "sdk_swift/package.rs"]
mod package;

type Error = Box<dyn std::error::Error>;

pub(crate) fn run(request: Request<SdkOption>) -> Result<(), Error> {
    let flags = &request.options;
    if flags
        .iter()
        .any(|flag| matches!(flag, SdkOption::Target(_)))
    {
        return Err("sdk swift builds every Apple target and refuses --target".into());
    }
    if flags
        .iter()
        .any(|flag| matches!(flag, SdkOption::Runtime(_)))
    {
        return Err(
            "sdk swift uses the installation's per-target embed slots and refuses --runtime".into(),
        );
    }
    let inputs = flags
        .iter()
        .any(|flag| matches!(flag, SdkOption::Inputs(_)));
    let entry = flags.iter().rev().find_map(|flag| match flag {
        SdkOption::Entry(entry) => hirsel_core_ids::DefPath::from_full(entry),
        _ => None,
    });
    let options = Options::read(request)?;
    let out = options
        .out
        .as_ref()
        .ok_or("sdk swift requires --output DIR")?;
    let name = if inputs {
        let name = entry
            .ok_or("sdk swift with --inputs requires --entry")?
            .0
            .package
            .name;
        if !crate::package_manifest::name::any(&name) {
            return Err(format!("bad package name {name:?}").into());
        }
        hirsel_core_resolve::short_name(&name).to_owned()
    } else {
        if !options.program.is_dir() {
            return Err("sdk swift requires a program directory".into());
        }
        match crate::package_workspace::Workspace::discover(&options.program)? {
            Some(workspace) => {
                hirsel_core_resolve::short_name(&workspace.selected()?.name).to_owned()
            }
            None => "app".into(),
        }
    };
    let work = tempfile::tempdir()?;
    let mut slices: Vec<Slice> = Vec::new();
    let mut projection = Vec::new();
    for apple in Target::ALL.into_iter().filter_map(Target::apple) {
        let dir = work.path().join(apple.xcframework_slice);
        fs::create_dir(&dir)?;
        let archive = dir.join(format!("lib{name}.a"));
        let mut build = Command::new(std::env::current_exe()?);
        build.arg("build");
        if !inputs {
            build.arg(&options.program);
        }
        let output = build
            .args(&options.build)
            .args(["--emit", "staticlib", "--target", apple.triple, "--n-place"])
            .arg(options.n_place.to_string())
            .arg("-o")
            .arg(&archive)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "sdk swift: static library build for {} failed\n{}{}",
                apple.triple,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        let slice = Slice::read(apple, archive)?;
        let current = binding_projection(&slice.manifest)?;
        if let Some(first) = slices.first() {
            if current != projection {
                return Err(format!(
                    "TargetDependentEntries({}, {})",
                    first.apple.triple, apple.triple
                )
                .into());
            }
        } else {
            projection = current;
        }
        slices.push(slice);
    }
    package::write(out, &name, &projection, &slices)
}

struct Slice {
    apple: &'static AppleRecord,
    archive: PathBuf,
    manifest: Vec<u8>,
    digest: [u8; 32],
    anchor: String,
}

impl Slice {
    fn read(apple: &'static AppleRecord, archive: PathBuf) -> Result<Self, Error> {
        let bytes = fs::read(&archive)?;
        let file = ArchiveFile::parse(bytes.as_slice())?;
        let mut members = file.members();
        // The archive writer puts the exports and entry before the program,
        // foreign and runtime members.
        let exports = members.next().ok_or("missing archive exports")??;
        let entry = members.next().ok_or("missing archive entry")??;
        let exports = object::File::parse(exports.data(bytes.as_slice())?)?;
        let entry = object::File::parse(entry.data(bytes.as_slice())?)?;
        let anchor = exports
            .symbols()
            .filter_map(|symbol| symbol.name().ok())
            .find_map(|symbol| symbol.strip_prefix("_hirsel_embed_v1_anchor_"))
            .ok_or("missing archive anchor")?;
        if anchor.len() != 16 || !anchor.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid archive anchor".into());
        }
        let manifest = entry
            .section_by_name(crate::native_entry::APPLE_MANIFEST_SECTION)
            .ok_or("missing archive manifest")?
            .data()?
            .to_vec();
        Ok(Self {
            apple,
            archive,
            manifest,
            digest: Sha256::digest(&bytes).into(),
            anchor: anchor.to_owned(),
        })
    }
}

fn upper_camel(value: &str) -> String {
    value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| format!("{}{}", part[..1].to_ascii_uppercase(), &part[1..]))
        .collect()
}

fn identifier(value: &str) -> String {
    let mut name = String::new();
    for (index, c) in value.char_indices() {
        if c.is_ascii_alphabetic() || (index != 0 && c.is_ascii_digit()) {
            name.push(c);
        } else if c == '_' && !value.starts_with("__hirsel") && !value[index..].starts_with("_u") {
            name.push('_');
        } else {
            use std::fmt::Write;
            write!(name, "_u{:x}_", u32::from(c)).unwrap();
        }
    }
    if name.is_empty() || name == "_" {
        name.push_str("hirsel");
    }
    format!("`{name}`")
}
