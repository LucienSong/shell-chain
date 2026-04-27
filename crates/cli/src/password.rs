//! Password resolution for CLI subcommands.
//!
//! Provides [`resolve_password`] which checks for non-interactive sources
//! (`--password-file`, `--password-stdin`) before falling back to a live
//! TTY prompt via `rpassword`.
//!
//! Usage pattern:
//!
//! ```rust,ignore
//! use crate::password::{PasswordArgs, resolve_password};
//!
//! let pw = resolve_password("Enter keystore password: ", &args.password_args)?;
//! ```

use std::io::{self, BufRead};
use std::path::PathBuf;

/// Password source flags forwarded from the global `Cli` struct.
#[derive(Clone, Debug, Default)]
pub struct PasswordArgs {
    /// Read the password from the first line of this file instead of prompting.
    pub password_file: Option<PathBuf>,
    /// Read the password from stdin (one line) instead of prompting.
    pub password_stdin: bool,
}

/// Resolve a keystore password from the configured source.
///
/// Priority order:
/// 1. `--password-file <path>` — read the first non-empty line from the file.
/// 2. `--password-stdin`       — read one line from standard input.
/// 3. Interactive TTY prompt   — use `rpassword` (default behaviour, no echo).
///
/// Trailing `\n` / `\r\n` is stripped from file and stdin sources.
pub fn resolve_password(
    prompt: &str,
    args: &PasswordArgs,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(ref path) = args.password_file {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read password file {}: {e}", path.display()))?;
        let password = content
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        if password.is_empty() {
            return Err(format!(
                "password file {} is empty or contains only blank lines",
                path.display()
            )
            .into());
        }
        return Ok(password);
    }

    if args.password_stdin {
        let stdin = io::stdin();
        let mut line = String::new();
        stdin
            .lock()
            .read_line(&mut line)
            .map_err(|e| format!("cannot read password from stdin: {e}"))?;
        let password = line.trim_end_matches('\n').trim_end_matches('\r').to_string();
        return Ok(password);
    }

    eprint!("{prompt}");
    Ok(rpassword::read_password()?)
}

/// Like [`resolve_password`] but prompts twice and checks they match.
///
/// Used for key generation where the user sets a new password.
/// Falls through to a single-prompt for non-interactive sources (file / stdin),
/// because confirmation doesn't make sense when the password is already written.
pub fn resolve_new_password(
    args: &PasswordArgs,
) -> Result<String, Box<dyn std::error::Error>> {
    if args.password_file.is_some() || args.password_stdin {
        return resolve_password("", args);
    }

    let password = resolve_password("Enter password for new keystore: ", args)?;
    let confirm = resolve_password("Confirm password: ", args)?;
    if password != confirm {
        return Err("Passwords do not match".into());
    }
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn password_file_reads_first_line() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "hunter2").unwrap();
        writeln!(f, "ignored").unwrap();

        let args = PasswordArgs { password_file: Some(f.path().to_path_buf()), password_stdin: false };
        let pw = resolve_password("", &args).unwrap();
        assert_eq!(pw, "hunter2");
    }

    #[test]
    fn password_file_empty_is_error() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let args = PasswordArgs { password_file: Some(f.path().to_path_buf()), password_stdin: false };
        assert!(resolve_password("", &args).is_err());
    }

    #[test]
    fn password_file_missing_is_error() {
        let args = PasswordArgs {
            password_file: Some(PathBuf::from("/nonexistent/path/pw.txt")),
            password_stdin: false,
        };
        assert!(resolve_password("", &args).is_err());
    }
}
