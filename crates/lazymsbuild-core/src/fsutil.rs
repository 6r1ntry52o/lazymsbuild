//! File writes that never leave a half-written file behind.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Writes `bytes` to `path` atomically: to a temporary file next to it, then a rename over it.
/// A reader (clangd, the editor) sees either the old file or the new one, never a partial one.
///
/// The rename is retried for a short while, because Windows refuses to replace a file that
/// another process has open without delete sharing, as clangd briefly does while reading.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = temp_path(path);
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    match rename_with_retry(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut delay = Duration::from_millis(10);
    let mut last = None;
    for _ in 0..12 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if is_sharing_violation(&err) => {
                last = Some(err);
                thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(250));
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
}

fn is_sharing_violation(err: &io::Error) -> bool {
    // ERROR_ACCESS_DENIED (5), ERROR_SHARING_VIOLATION (32), ERROR_LOCK_VIOLATION (33).
    matches!(err.raw_os_error(), Some(5 | 32 | 33)) || err.kind() == io::ErrorKind::PermissionDenied
}

/// SHA-256 of a file's contents, as lower-case hex. `None` when the file does not exist.
pub fn sha256_file(path: &Path) -> io::Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(sha256(&bytes))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// SHA-256 of `bytes`, as lower-case hex.
pub fn sha256(bytes: &[u8]) -> String {
    crate::paths::hex(&Sha256::digest(bytes))
}

/// Appends lines to a text file, adding a line break first if the file does not end with one.
pub fn append_lines(path: &Path, lines: &[String]) -> io::Result<()> {
    let existing = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(err),
    };
    let crlf = existing.windows(2).any(|w| w == b"\r\n");
    let newline = if crlf { "\r\n" } else { "\n" };
    let mut text = String::new();
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        text.push_str(newline);
    }
    for line in lines {
        text.push_str(line);
        text.push_str(newline);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(text.as_bytes())
}

/// Modification time of a file, if it exists.
pub fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Opens a file with its default application (Explorer's "open").
pub fn open_with_default_app(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        crate::win::shell_open(path)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn().map(|_| ())
    }
}

/// Finds an executable on `PATH`, trying the `PATHEXT` extensions when `name` has none.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let has_extension = Path::new(name).extension().is_some();
    let extensions: Vec<String> = if has_extension {
        vec![String::new()]
    } else {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect()
    };
    for dir in std::env::split_paths(&path) {
        for extension in &extensions {
            let candidate = dir.join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("out.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        // No temporary file is left behind.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn append_lines_keeps_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exclude");
        fs::write(&path, "a\r\nb").unwrap();
        append_lines(&path, &["c".to_string()]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\r\nb\r\nc\r\n");
        let fresh = dir.path().join("fresh");
        append_lines(&fresh, &["x".to_string()]).unwrap();
        assert_eq!(fs::read_to_string(&fresh).unwrap(), "x\n");
    }

    #[test]
    fn sha256_of_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(sha256_file(&dir.path().join("none")).unwrap(), None);
        assert_eq!(sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
