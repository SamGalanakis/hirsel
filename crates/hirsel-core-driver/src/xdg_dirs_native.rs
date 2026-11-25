// Laws of launcher-resolved directories (FABLE-RULING R5; launch.md,
// Launcher-resolved directories). Each builds one program whose `Dir` writes
// `note`, then runs the executable with a cleared environment, so the base is
// only what the law sets.

const XDG_NOTE: &str = "pub fn main(dir: Dir, out: Stdout) -> Result[(), Exit] waits {\n    let mut f = result.unwrap(host.open(dir, \"note\", CreateNew))\n    result.unwrap(host.write(mut f, string.into_bytes(string.from(\"noted\"))))\n    result.unwrap(stdout.write(out, \"ok\"))\n    Ok(())\n}\n";

/// The law's program built with `dir` as its one directory and Stdout.
fn xdg_program(law: &PackageLaw, dir: &str, source: &str) -> PathBuf {
    law.manifest("app", "app", 1, "[sources]\n\"main.hirsel\" = \"main\"\n");
    law.write("app/main.hirsel", source);
    let (status, stdout, stderr) =
        law.run(&["build", "app", "--output", "program", "--dir", dir, "--grant", "Stdout", "--limit", "1048576"]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    law.path("program")
}

fn xdg_run(law: &PackageLaw, program: &Path, env: &[(&str, PathBuf)], args: &[&str]) -> (Option<i32>, String) {
    let mut command = Command::new(program);
    command.env_clear().current_dir(law.path("")).args(args);
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    (output.status.code(), String::from_utf8_lossy(&output.stderr).into_owned())
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

/// Law A (R5): `xdg-config:app` is `$XDG_CONFIG_HOME/app` when that is
/// absolute, else `$HOME/.config/app`; the missing directory is created with
/// mode 0700 and the program writes inside it. Red on main, where the form is
/// a relative path that does not exist (not started, 103).
#[test]
#[ignore = "explicit native integration target"]
fn xdg_directory_is_resolved_created_and_granted() {
    let law = PackageLaw::new();
    let program = xdg_program(&law, "xdg-config:app", XDG_NOTE);
    let config = law.path("t/c");
    let (status, stderr) = xdg_run(&law, &program, &[("XDG_CONFIG_HOME", config.clone())], &[]);
    assert_eq!(status, Some(0), "{stderr}");
    assert_eq!(std::fs::read(config.join("app/note")).unwrap(), b"noted");
    assert_eq!(mode(&config.join("app")), 0o700);

    // A relative XDG_CONFIG_HOME is ignored, as the XDG specification says.
    let home = law.path("t/h");
    std::fs::create_dir_all(&home).unwrap();
    let env = [("XDG_CONFIG_HOME", PathBuf::from("relative")), ("HOME", home.clone())];
    let (status, stderr) = xdg_run(&law, &program, &env, &[]);
    assert_eq!(status, Some(0), "{stderr}");
    assert_eq!(std::fs::read(home.join(".config/app/note")).unwrap(), b"noted");
    assert_eq!(mode(&home.join(".config/app")), 0o700);
    assert!(!law.path("relative").exists());

    // Neither base: not started, with its own line.
    let (status, stderr) = xdg_run(&law, &program, &[], &[]);
    assert_eq!(status, Some(103), "{stderr}");
    assert!(
        stderr.contains("not started: directory grant `xdg-config:app` needs $XDG_CONFIG_HOME or $HOME set to an absolute path"),
        "{stderr}"
    );
}

/// Law B (R5; CAP-4: a run that does not start has no host effect): a run
/// refused for another missing grant creates nothing. Red when the directory
/// is created as it is resolved, before the refusal.
#[test]
fn xdg_directory_is_not_created_for_a_run_that_does_not_start() {
    let law = PackageLaw::new();
    let source = "pub fn main(dir: Dir, out: Stdout, clock: Clock) -> Result[(), Exit] waits {\n    let _ = host.mono_ns(clock)\n    let _ = dir\n    result.unwrap(stdout.write(out, \"ok\"))\n    Ok(())\n}\n";
    let program = xdg_program(&law, "xdg-config:app", source);
    let config = law.path("t/c");
    let (status, stderr) = xdg_run(&law, &program, &[("XDG_CONFIG_HOME", config.clone())], &[]);
    assert_eq!(status, Some(103), "{stderr}");
    assert!(stderr.contains("not started: grant `clock` is not in the run's configuration"), "{stderr}");
    assert!(!config.exists(), "{stderr}");
}

/// Law C (R5): an application name outside `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`
/// is refused at build. Red on main, where each is built as a relative path.
#[test]
fn xdg_directory_name_is_refused_at_build() {
    let law = PackageLaw::new();
    law.manifest("app", "app", 1, "[sources]\n\"main.hirsel\" = \"main\"\n");
    law.write("app/main.hirsel", XDG_NOTE);
    let long = format!("xdg-data:{}", "a".repeat(65));
    for dir in ["xdg-config:..", "xdg-config:a/b", "xdg-cache:", "xdg-state:.hidden", "xdg-data:-flag", long.as_str()] {
        let (status, stdout, stderr) =
            law.run(&["build", "app", "--output", "program", "--dir", dir, "--grant", "Stdout", "--limit", "1048576"]);
        assert_eq!(status, Some(2), "{dir}: {stdout}\n{stderr}");
        assert!(stderr.contains("the application name must match"), "{dir}: {stderr}");
        assert!(!law.path("program").exists(), "{dir}");
    }
}

/// Law D (R5, guard): a relative directory literally named `xdg-config:x` is
/// written `./xdg-config:x` and opened as that path.
#[test]
#[ignore = "explicit native integration target"]
fn xdg_shaped_relative_path_is_a_path() {
    let law = PackageLaw::new();
    let program = xdg_program(&law, "./xdg-config:x", XDG_NOTE);
    std::fs::create_dir(law.path("xdg-config:x")).unwrap();
    let config = law.path("t/c");
    let (status, stderr) = xdg_run(&law, &program, &[("XDG_CONFIG_HOME", config.clone())], &[]);
    assert_eq!(status, Some(0), "{stderr}");
    assert_eq!(std::fs::read(law.path("xdg-config:x/note")).unwrap(), b"noted");
    assert!(!config.exists());
}

/// Law E (R5): the resolved directory follows symlinks, so a link to a
/// directory is that directory; a dangling link is not started (103) and
/// nothing is created through it. Red on main (a relative path), and the
/// dangling half is red when creation follows the link (mkdir of its target).
#[test]
#[ignore = "explicit native integration target"]
fn xdg_directory_follows_symlinks_and_refuses_a_dangling_one() {
    let law = PackageLaw::new();
    let program = xdg_program(&law, "xdg-config:app", XDG_NOTE);
    let config = law.path("t/c");
    let real = law.path("dotfiles/app");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, config.join("app")).unwrap();
    let (status, stderr) = xdg_run(&law, &program, &[("XDG_CONFIG_HOME", config.clone())], &[]);
    assert_eq!(status, Some(0), "{stderr}");
    assert_eq!(std::fs::read(real.join("note")).unwrap(), b"noted");

    let other = law.path("t/o");
    let target = law.path("missing/app");
    std::fs::create_dir_all(&other).unwrap();
    std::os::unix::fs::symlink(&target, other.join("app")).unwrap();
    let (status, stderr) = xdg_run(&law, &program, &[("XDG_CONFIG_HOME", other.clone())], &[]);
    assert_eq!(status, Some(103), "{stderr}");
    assert!(stderr.contains("not started: directory grant `"), "{stderr}");
    assert!(!law.path("missing").exists(), "{stderr}");
}

/// Law F (R5): a describe launch creates nothing. Red when the directory is
/// created whatever the launch mode.
#[test]
fn xdg_directory_is_not_created_by_a_describe_launch() {
    let law = PackageLaw::new();
    let program = xdg_program(&law, "xdg-config:app", XDG_NOTE);
    let config = law.path("t/c");
    let (_, stderr) = xdg_run(
        &law,
        &program,
        &[("XDG_CONFIG_HOME", config.clone())],
        &["--hirsel-describe", "--hirsel-n-place=1"],
    );
    assert!(!config.exists(), "{stderr}");
}
