use std::process::Command;

// Embed the short git sha so the running host can report its build identity to
// clients in `hello_ok` (Settings → About). Falls back to "unknown" outside a
// git checkout.
fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=HIRSEL_GIT_SHA={sha}");

    // Rebuild the embedded sha whenever HEAD moves (works in worktrees too).
    if let Some(head) = Command::new("git")
        .args(["rev-parse", "--git-path", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
    {
        println!("cargo:rerun-if-changed={}", head.trim());
    }
    println!("cargo:rerun-if-changed=build.rs");
}
