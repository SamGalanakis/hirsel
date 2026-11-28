//! Shared discovery and bounded decoding of Hirsel's private configuration.
use hirsel_config_schema::{Config, Limits, user_v1};
use std::fs::File;
use std::io;
use std::path::PathBuf;

pub(crate) fn path() -> Option<PathBuf> {
    fn absolute(var: &str) -> Option<PathBuf> {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    }
    absolute("XDG_CONFIG_HOME")
        .or_else(|| absolute("HOME").map(|home| home.join(".config")))
        .map(|base| base.join("hirsel/config.hcfg"))
}

pub(crate) fn decode(input: impl io::Read) -> Result<user_v1::Config, String> {
    let bytes = hirsel_config_schema::read_bounded(input).map_err(|e| e.to_string())?;
    user_v1::Config::read(&bytes, Limits::default()).map_err(|e| e.to_string())
}

pub(crate) fn load() -> Result<Option<user_v1::Config>, String> {
    let Some(path) = path() else { return Ok(None) };
    match File::open(&path) {
        Ok(file) => decode(file)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}
