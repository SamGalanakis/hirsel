/// Checked mode sweeps the ownership and body fixtures. The ordinary walkers
/// cover the full front-end corpus.
fn front_run_paths() -> Vec<std::path::PathBuf> {
    fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "hirsel") {
                out.push(path);
            }
        }
    }

    let root = hirsel_core_program_tests::repo().join("core/front/cases");
    let mut paths = Vec::new();
    if std::env::var_os("HIRSEL_ALLOC").as_deref() == Some(std::ffi::OsStr::new("checked")) {
        files(&root.join("own"), &mut paths);
        files(&root.join("body"), &mut paths);
    } else {
        files(&root, &mut paths);
    }
    paths.sort();
    paths
}
