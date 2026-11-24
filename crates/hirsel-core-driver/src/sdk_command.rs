//! Build a server, then generate bindings from its bounded describe launch.
use crate::cli::{Invocation, OptionId};
use crate::sdk::{Describe, write_with_frame};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

type Error = Box<dyn std::error::Error>;

pub struct Options {
    pub program: PathBuf,
    pub out: PathBuf,
    pub timeout: Duration,
    pub n_place: u64,
    pub build: Vec<String>,
}
impl Options {
    pub fn read(invocation: Invocation) -> Result<Self, Error> {
        let program = invocation
            .positional
            .ok_or("sdk ts requires a program directory or built executable")?
            .into();
        let mut out = None;
        let mut timeout = Duration::from_secs(10);
        let mut n_place = 16 * 1024 * 1024;
        let mut build = Vec::new();
        for (id, value) in invocation.options {
            let text = value.as_deref().unwrap_or_default();
            match id {
                OptionId::Output => out = Some(PathBuf::from(text)),
                OptionId::PlaceTimeout => {
                    let seconds: f64 = text.parse()?;
                    timeout = Duration::try_from_secs_f64(seconds)
                        .map_err(|_| "--place-timeout must be finite and positive")?;
                    if timeout.is_zero() {
                        return Err("--place-timeout must be positive".into());
                    }
                }
                OptionId::NPlace => n_place = text.parse()?,
                _ => {
                    let name = match id {
                        OptionId::Runtime => "--runtime",
                        OptionId::Backend => "--backend",
                        OptionId::Cc => "--cc",
                        OptionId::Target => "--target",
                        OptionId::Inputs => "--inputs",
                        OptionId::PackageStore => "--package-store",
                        OptionId::Locked => "--locked",
                        OptionId::Frozen => "--frozen",
                        OptionId::Libraries => "--libraries",
                        OptionId::Entry => "--entry",
                        OptionId::Store => "--store",
                        OptionId::CoreProduct => "--core-product",
                        OptionId::Limit => "--limit",
                        OptionId::SourceCore => "--source-core",
                        OptionId::SourceStd => "--source-std",
                        OptionId::Release => "--release",
                        OptionId::DebugLines => "--debug-lines",
                        _ => return Err("unsupported sdk build option".into()),
                    };
                    build.push(name.into());
                    if let Some(value) = value {
                        build.push(value);
                    }
                }
            }
        }
        Ok(Self {
            program,
            out: out.ok_or("sdk ts requires --output DIR")?,
            timeout,
            n_place,
            build,
        })
    }
}

/// The launcher owns timeout, kill/reap and response bounds. Generation has no
/// output effects until the child and every schema/name have succeeded.
pub fn run(options: Options) -> Result<(), Error> {
    run_with(options, describe)
}

pub(crate) fn describe(
    executable: &Path,
    n_place: u64,
    timeout: Duration,
) -> Result<Vec<u8>, Error> {
    match crate::describe::describe_child(executable, n_place, timeout) {
        Ok(bytes) => Ok(bytes),
        Err(crate::describe::DescribeError::PlaceError(bytes)) => {
            Err(crate::sdk::place_error(&bytes)?.into())
        }
        Err(error) => Err(Box::new(error)),
    }
}

fn run_with(
    options: Options,
    describe: impl FnOnce(&Path, u64, Duration) -> Result<Vec<u8>, Error>,
) -> Result<(), Error> {
    let work = tempfile::tempdir()?;
    let executable = if options.program.is_file() {
        if let Some(flag) = options.build.first() {
            return Err(format!("{flag} applies when building a program directory").into());
        }
        options.program.canonicalize()?
    } else {
        let executable = work.path().join("server");
        let status = Command::new(std::env::current_exe()?)
            .arg("build")
            .arg(&options.program)
            .args(&options.build)
            .arg("-o")
            .arg(&executable)
            .stdout(Stdio::null())
            .status()?;
        if !status.success() {
            return Err("SDK program build failed".into());
        }
        executable
    };
    let response = describe(&executable, options.n_place, options.timeout)?;
    let description = Describe::read(&response)?;
    write_with_frame(&description, FRAME_SCHEMA, &options.out)
}

pub use crate::product::bundled::FRAME_SCHEMA;

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Options, Error> {
        match crate::cli::parse(args.iter().map(|s| (*s).to_owned()))? {
            crate::cli::Action::Invoke(invocation) => Options::read(invocation),
            _ => Err("expected invocation".into()),
        }
    }
    #[test]
    fn sdk_command_options() {
        let options = parse(&["sdk", "ts", "server", "-o", "bindings"]).unwrap();
        assert_eq!(options.out, PathBuf::from("bindings"));
        assert!(parse(&["sdk", "ts", "server", "--out", "bindings"]).is_err());
        assert_eq!(options.timeout, Duration::from_secs(10));
        assert_eq!(options.n_place, 16 * 1024 * 1024);
        let options = parse(&[
            "sdk",
            "ts",
            "server",
            "--output",
            "bindings",
            "--place-timeout",
            "0.5",
            "--n-place",
            "65536",
        ])
        .unwrap();
        assert_eq!(options.timeout, Duration::from_millis(500));
        assert_eq!(options.n_place, 65536);
        for seconds in ["0", "-1", "NaN", "inf"] {
            assert!(
                parse(&[
                    "sdk",
                    "ts",
                    "server",
                    "--output",
                    "bindings",
                    "--place-timeout",
                    seconds
                ])
                .is_err()
            );
        }
        assert!(parse(&["sdk", "ts", "server"]).is_err());
        assert!(parse(&["sdk", "js", "server", "--output", "bindings"]).is_err());
    }
    #[test]
    fn sdk_command_does_not_write_after_a_describe_error() {
        let dir = tempfile::tempdir().unwrap();
        let server = dir.path().join("server");
        std::fs::write(&server, []).unwrap();
        let output = dir.path().join("out");
        let options = Options {
            program: server,
            out: output.clone(),
            timeout: Duration::from_secs(10),
            n_place: 7,
            build: vec![],
        };
        let error = run_with(options, |_, n_place, _| {
            assert_eq!(n_place, 7);
            Err("PlaceTimedOut".into())
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "PlaceTimedOut");
        assert!(!output.exists());
    }

    #[test]
    fn sdk_command_uses_bounded_child_and_preserves_errors() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let server = dir.path().join("server");
        let output = dir.path().join("out");
        std::fs::create_dir(&output).unwrap();
        let marker = output.join("keep");
        std::fs::write(&marker, "unchanged").unwrap();
        let check = |body: &str, n_place, timeout, expected: &str| {
            std::fs::write(&server, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o700)).unwrap();
            let options = Options {
                program: server.clone(),
                out: output.clone(),
                timeout,
                n_place,
                build: vec![],
            };
            assert_eq!(run(options).unwrap_err().to_string(), expected);
            assert_eq!(std::fs::read_to_string(&marker).unwrap(), "unchanged");
            assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
        };
        let response_timeout = Duration::from_secs(3600);
        for n_place in [0, 1] {
            check(
                "printf '\\000\\000\\000\\000\\010\\000\\000\\000\\000\\000\\000\\000\\000\\000\\000\\000\\000\\000\\000\\000'; exit 2",
                n_place,
                response_timeout,
                "PlaceCapacity(Room { requested: 8, available: 0 })",
            );
        }
        check(
            "printf '\\001\\000\\000\\000\\003\\000\\000\\000abc'; exit 2",
            4096,
            response_timeout,
            "AlreadyRegistered([97, 98, 99])",
        );
        check(
            "exec python3 -c 'import threading; threading.Event().wait()'",
            4096,
            Duration::from_millis(100),
            "PlaceTimedOut",
        );
    }
}
