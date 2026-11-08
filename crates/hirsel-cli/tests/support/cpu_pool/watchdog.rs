fn qual_native_output(command: &mut Command, root: &std::path::Path) -> std::process::Output {
    use std::os::unix::process::CommandExt;
    let stdout = root.join("child.stdout");
    let stderr = root.join("child.stderr");
    let status = command
        .process_group(0)
        .stdout(fs::File::create(&stdout).unwrap())
        .stderr(fs::File::create(&stderr).unwrap())
        .status()
        .unwrap();
    std::process::Output {
        status,
        stdout: fs::read(stdout).unwrap(),
        stderr: fs::read(stderr).unwrap(),
    }
}
