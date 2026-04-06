//! `shell-node version` — display version information.

/// Print version information.
pub fn version() -> Result<(), Box<dyn std::error::Error>> {
    let name = env!("CARGO_PKG_NAME");
    let version = env!("CARGO_PKG_VERSION");
    let git_hash = env!("GIT_HASH");

    eprintln!("{name} {version}");
    if !git_hash.is_empty() {
        eprintln!("  commit: {git_hash}");
    }

    Ok(())
}
