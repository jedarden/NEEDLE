//! Load operator-managed environment variables for launched workers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const WORKER_ENV_RELATIVE_PATH: &str = ".config/needle/workers/env";

pub fn path() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(WORKER_ENV_RELATIVE_PATH)
}

pub fn apply_to_command(command: &mut Command) {
    apply_to_command_from_path(command, &path());
}

pub fn apply_to_command_from_path(command: &mut Command, path: &Path) {
    for (key, value) in load_from_path(path) {
        command.env(key, value);
    }
}

pub fn load() -> Vec<(String, String)> {
    load_from_path(&path())
}

pub fn load_from_path(path: &Path) -> Vec<(String, String)> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "unable to read worker environment file");
            return Vec::new();
        }
    };

    warn_if_permissive(path);
    contents
        .lines()
        .enumerate()
        .filter_map(|(index, line)| parse_line(path, index + 1, line))
        .collect()
}

fn parse_line(path: &Path, line_number: usize, line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        tracing::warn!(path = %path.display(), line = line_number, "skipping blank line in worker environment file");
        return None;
    }
    if trimmed.starts_with('#') {
        tracing::warn!(path = %path.display(), line = line_number, "skipping comment in worker environment file");
        return None;
    }

    let Some((key, value)) = trimmed.split_once('=') else {
        warn_malformed(path, line_number);
        return None;
    };
    let key = key.trim();
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        || key.as_bytes()[0].is_ascii_digit()
    {
        warn_malformed(path, line_number);
        return None;
    }

    Some((key.to_string(), value.to_string()))
}

fn warn_malformed(path: &Path, line_number: usize) {
    tracing::warn!(path = %path.display(), line = line_number, "skipping malformed line in worker environment file");
}

#[cfg(unix)]
fn warn_if_permissive(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    match fs::metadata(path) {
        Ok(metadata) if metadata.permissions().mode() & 0o077 != 0 => {
            tracing::warn!(path = %path.display(), "worker environment file is readable by group or other users; use mode 600");
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "unable to inspect worker environment file permissions")
        }
    }
}

#[cfg(not(unix))]
fn warn_if_permissive(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_comments_blank_lines_and_valid_assignments() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("env");
        fs::write(
            &path,
            "# operator note\n\nPROBE_VALUE=harmless\nSECOND=value with spaces\n",
        )
        .unwrap();
        assert_eq!(
            load_from_path(&path),
            vec![
                ("PROBE_VALUE".to_string(), "harmless".to_string()),
                ("SECOND".to_string(), "value with spaces".to_string())
            ]
        );
    }

    #[test]
    fn skips_malformed_lines() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("env");
        fs::write(
            &path,
            "NO_EQUALS\n=missing-key\nnot-a-key=value\n1STARTS_WITH_DIGIT=value\nVALID=kept\n",
        )
        .unwrap();
        assert_eq!(
            load_from_path(&path),
            vec![("VALID".to_string(), "kept".to_string())]
        );
    }

    #[test]
    fn missing_file_is_unchanged() {
        let directory = tempdir().unwrap();
        assert!(load_from_path(&directory.path().join("missing")).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn permissive_file_is_still_loaded() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempdir().unwrap();
        let path = directory.path().join("env");
        fs::write(&path, "PROBE_VALUE=harmless\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            load_from_path(&path),
            vec![("PROBE_VALUE".to_string(), "harmless".to_string())]
        );
    }
}
