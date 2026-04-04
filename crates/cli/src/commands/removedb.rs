//! `shell-node removedb` — remove the chain database directory.

use std::path::PathBuf;

/// Remove the chain data directory.
///
/// Without `--force`, prints what would be removed and exits.
/// With `--force`, deletes the database directory.
pub fn removedb(datadir: PathBuf, force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = datadir.join("db");

    if !db_path.exists() {
        eprintln!("Nothing to remove: {} does not exist.", db_path.display());
        return Ok(());
    }

    // Calculate directory size for display.
    let dir_size = dir_size(&db_path).unwrap_or(0);

    if !force {
        eprintln!("Would remove: {} ({} bytes)", db_path.display(), dir_size);
        eprintln!("Run with --force to actually delete.");
        return Ok(());
    }

    std::fs::remove_dir_all(&db_path)?;
    eprintln!("✓ Removed {} ({} bytes)", db_path.display(), dir_size);

    Ok(())
}

/// Recursively compute the total size of a directory.
fn dir_size(path: &PathBuf) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            total += dir_size(&entry.path().to_path_buf())?;
        } else {
            total += meta.len();
        }
    }
    Ok(total)
}
