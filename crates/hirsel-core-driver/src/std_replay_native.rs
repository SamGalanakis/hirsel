/// Run each recording on every backend and std composition. A replay's path is
/// fixed at link time, so replace the tape there between runs of the same image.
fn std_replay_matrix(
    product: &CheckProduct,
    options: &NativeOptions,
    project: &Path,
    tapes: &[(StdBuild, Option<Vec<u8>>)],
) -> usize {
    let recordings: Vec<_> = tapes.iter().filter_map(|(label, bytes)| {
        let bytes = bytes.as_deref()?;
        let read = |offset: u64, buffer: &mut [u8]| {
            std::io::Read::read(&mut &bytes[offset as usize..], buffer)
        };
        let (_, header) = hirsel_tape_format::Index::open(read).unwrap();
        // A Run header ends its launch arguments with the recording build digest.
        let digest = *header.arg.last_chunk::<32>().unwrap();
        Some((label, bytes, digest))
    }).collect();
    let path = project.join("replay.tape");
    for (build, _, own_digest) in &recordings {
        let replay = NativeOptions {
            replay: Some(crate::ReplayPlan { path: path.to_str().unwrap().into(), diagnose: false }),
            grant_mask: 0,
            ..options.clone()
        };
        let executable = std_executable(product, &replay, **build, project);
        for (label, tape, digest) in &recordings {
            std::fs::write(&path, tape).unwrap();
            let result = Command::new(&executable).current_dir(project).env_remove("HIRSEL_RECORD").output().unwrap();
            let diagnostic = if digest == own_digest { &b""[..] } else { b"replay: build differs\n" };
            assert_eq!(
                (result.status.code(), result.stdout.as_slice(), result.stderr.as_slice()),
                (Some(0), &b""[..], diagnostic),
                "{}: {label:?} tape on {build:?}", project.display()
            );
        }
    }
    recordings.len()
}
