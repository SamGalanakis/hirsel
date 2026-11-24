/// The program a `run` or `replay` request built: its argument vector (the
/// runner's, then the executable's, then the request's), the variables it runs
/// with besides the caller's environment, and the scratch directory that holds
/// it until it has run.
pub struct Launch {
    pub argv: Vec<std::ffi::OsString>,
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    pub(crate) keep: Option<crate::store::Scratch>,
}
impl Launch {
    /// Run the program and return the request's exit status: the program's,
    /// or 102 when a signal ended it.
    pub fn execute(
        argv: &[std::ffi::OsString],
        env: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Result<i32, Box<dyn std::error::Error>> {
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .envs(env.iter().map(|(name, value)| (name, value)))
            .status()
            .map_err(|e| format!("run execution {:?}: {e}", argv[0]))?;
        if !status.success() {
            eprintln!("hirsel: main {}", crate::test_run::outcome_text(status));
        }
        Ok(status.code().unwrap_or(102))
    }
}
