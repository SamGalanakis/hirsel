fn build_copy_filter(image: &Path) {
    // macOS kills relocated system executables. Pin a native copy filter built
    // in the work directory so deleting that directory removes the host image.
    let source = image.with_extension("c");
    std::fs::write(&source, r#"#include <stdio.h>
int main(void) {
    int c;
    while ((c = getchar()) != EOF) {
        if (putchar(c) == EOF) return 1;
    }
    return ferror(stdin) || fflush(stdout);
}
"#).unwrap();
    let options = native_options(Backend::Cranelift, image.parent().unwrap());
    command(Command::new(options.link.driver()).arg(&source).arg("-o").arg(image)).unwrap();
}

/// The replay has neither the copied executable nor the Unix root used by the
/// recorded run. It must recover the child, pipes and packet only from tape.
#[test]
fn exec_unix_recording_replays_without_host_resources() {
    use crate::package_content::archive_digest;
    use hirsel_core_net::{GrantRule, UnixDirection, UnixRule};

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let image = work.join("image");
    build_copy_filter(&image);
    let pin = hirsel_core_ids::hex::encode(&archive_digest(&std::fs::read(&image).unwrap()));
    let policy = work.join("policy.hcfg");
    // Empty forms use three input bytes each and four snapshot bytes each.
    // Keep the policy under the config limit and the snapshot over a tape block.
    let empty_forms = "[],".repeat(16 * 1024);
    let unused_arg = "x".repeat(hirsel_tape_format::MAX_LEN - 56 * 1024);
    std::fs::write(
        &policy,
        format!(
            "exec_v1.Policy(profiles: [(\"cat\", Profile(image: {:?}, sha256: \"{pin}\", argv0: \"cat\", args: [{empty_forms}[Lit(\"{unused_arg}\")]], env: [], dirs: [(\"work\", {:?})]))])",
            image.to_str().unwrap(),
            work.to_str().unwrap(),
        ),
    )
    .unwrap();
    let request =
        hirsel_core_net::exec::Request::parse(&format!("{} cat", policy.display())).unwrap();
    let digest = request.load().unwrap().digest;
    let source =
        std::fs::read_to_string(root.join("core/front/fixtures/recording/exec-unix.hirsel"))
            .unwrap();
    let mut inputs = source_request(&source, dir.path());
    inputs.inputs.packages[1] =
        PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let checked = Driver::default()
        .check(inputs, &mut Meter::new(false))
        .unwrap();
    assert!(
        checked.accepted(),
        "{:?} {:?}",
        checked.diagnostics,
        checked.incomplete
    );
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.runtime = PathBuf::from(env_record_runtime());
    options.recording = true;
    options.output = dir.path().join("record");
    options.grant_mask = 1 << grant_kind("Net").unwrap();
    options.exec_grants.push(crate::exec_launch::Binding {
        request,
        policy: Some(digest),
        workdir: None,
    });
    options
        .unix_roots
        .insert("work".into(), work.to_str().unwrap().into());
    options.net_rules = [UnixDirection::Bind, UnixDirection::Send]
        .into_iter()
        .map(|direction| GrantRule::Unix(UnixRule::new(direction, "work", "packet").unwrap()))
        .collect();
    let executable = checked.build(&entry, &options).unwrap().executable;
    let tape = dir.path().join("run.tape");
    let mut record = Command::new(executable);
    options.exec_grants[0].approve(&mut record);
    let recorded = record.env("HIRSEL_RECORD", &tape).output().unwrap();
    assert_eq!(
        recorded.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&recorded.stderr)
    );
    let sealed = std::fs::read(&tape).unwrap();
    assert!(rl8_root_frames(&tape).iter().any(|frame| {
        matches!(frame, hirsel_tape_format::Frame::Grant { args: Some(bytes), .. }
            if bytes.len() > hirsel_tape_format::MAX_LEN
                && hirsel_tape_format::exec_snapshot_rules(bytes).is_ok())
    }));
    std::fs::remove_dir_all(&work).unwrap();

    options.output = dir.path().join("replay");
    options.runtime = env_observe_runtime();
    options.exec_grants.clear();
    options.net_rules.clear();
    options.unix_roots.clear();
    options.grant_mask = 0;
    options.replay = Some(crate::ReplayPlan {
        path: tape.to_str().unwrap().into(),
        diagnose: false,
    });
    let replay = checked.build(&entry, &options).unwrap().executable;
    let (status, _, stderr) = observed_tape_run(&replay, &[]);
    assert_eq!(status, Some(0), "{stderr}");
    assert!(!work.exists());
    assert_eq!(std::fs::read(tape).unwrap(), sealed);
}
