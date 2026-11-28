//! Confined Compiler capability entry for one canonical C source member.
use crate::package_content::Content;
use crate::package_dir::path_rule;
use bytes::Bytes;
use hirsel_compiler::{check_library, read_source::*};
use std::io::{self, Write};

pub(super) fn run_worker() -> i32 {
    let run = || -> io::Result<i32> {
        check_library::seal_worker()?;
        let (content, path, limit) = read_request(io::stdin().lock())?;
        let Ok(path) = std::str::from_utf8(&path) else {
            return Ok(INVALID_PATH);
        };
        if path_rule(path, ".hirsel").is_err() {
            return Ok(INVALID_PATH);
        }
        let Ok(archive) = Content::decode_as(&Bytes::from(content), false) else {
            return Ok(MALFORMED_ARCHIVE);
        };
        let Some(file) = archive.files.get(path) else {
            return Ok(MISSING_PATH);
        };
        let Ok(text) = std::str::from_utf8(&file.bytes) else {
            return Ok(MALFORMED_ARCHIVE);
        };
        if text.len() as u64 > limit {
            return Ok(OUTPUT_LIMIT);
        }
        io::stdout().lock().write_all(text.as_bytes())?;
        Ok(0)
    };
    match run() {
        Ok(status) => status,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "read_source: {error}");
            2
        }
    }
}
