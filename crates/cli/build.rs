use std::process::Command;

fn main() {
    // Embed git commit hash at compile time.
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    println!("cargo:rustc-env=GIT_HASH={}", git_hash.trim());

    // Re-run if HEAD changes (new commit).
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
