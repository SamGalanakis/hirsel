use zed_extension_api::{self as zed, Command, Result, Worktree};

struct HirselExtension;

impl zed::Extension for HirselExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let command = worktree
            .which("hirsel")
            .ok_or("Install Hirsel and add its executable to PATH.")?;
        Ok(Command {
            command,
            args: vec!["lsp".into()],
            env: Vec::new(),
        })
    }
}

zed::register_extension!(HirselExtension);
