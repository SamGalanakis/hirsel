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

    // Rebuild the embedded sha whenever HEAD or its symbolic branch ref moves.
    if let Some(head) = Command::new("git")
        .args(["rev-parse", "--git-path", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
    {
        let head = head.trim();
        if !head.is_empty() {
            println!("cargo:rerun-if-changed={head}");
        }
    }

    if let Some(symbolic_head) = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        if let Some(branch_ref) = Command::new("git")
            .args(["rev-parse", "--git-path", &symbolic_head])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            println!("cargo:rerun-if-changed={branch_ref}");
        }

        if let Some(packed_refs) = Command::new("git")
            .args(["rev-parse", "--git-path", "packed-refs"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            println!("cargo:rerun-if-changed={packed_refs}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
