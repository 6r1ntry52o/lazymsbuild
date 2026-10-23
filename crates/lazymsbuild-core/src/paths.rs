//! Path normalization, and the keys lazymsbuild compares paths by.
//!
//! Windows file systems are case-insensitive and accept both separators, so two spellings of one
//! file must compare equal: [`key`] is the normalized path in lower case.

use std::ffi::{OsStr, OsString};
use std::path::{Component, MAIN_SEPARATOR_STR, Path, PathBuf, Prefix};

use sha2::{Digest, Sha256};

/// Resolves `.` and `..` and writes every separator as the platform's, without touching the
/// file system, so it also works for files that no longer exist. A `\\?\` prefix is dropped and
/// the drive letter is upper-cased.
pub fn normalize(path: &Path) -> PathBuf {
    let mut prefix: Option<OsString> = None;
    let mut has_root = false;
    let mut parts: Vec<&OsStr> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => prefix = Some(plain_prefix(p.kind(), p.as_os_str())),
            Component::RootDir => has_root = true,
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(last) if *last != ".." => {
                    parts.pop();
                }
                // `C:\..` is `C:\`; a relative path keeps its leading `..`.
                _ if has_root => {}
                _ => parts.push(OsStr::new("..")),
            },
            Component::Normal(part) => parts.push(part),
        }
    }

    let mut out = OsString::new();
    if let Some(prefix) = prefix {
        out.push(prefix);
    }
    if has_root {
        out.push(MAIN_SEPARATOR_STR);
    }
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push(MAIN_SEPARATOR_STR);
        }
        out.push(part);
    }
    if out.is_empty() {
        out.push(".");
    }
    PathBuf::from(out)
}

fn plain_prefix(kind: Prefix<'_>, raw: &OsStr) -> OsString {
    match kind {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
            OsString::from(format!("{}:", char::from(drive).to_ascii_uppercase()))
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            let mut s = OsString::from(r"\\");
            s.push(server);
            s.push(r"\");
            s.push(share);
            s
        }
        _ => raw.to_os_string(),
    }
}

/// `path` made absolute against `base` (when it is relative) and normalized.
pub fn absolutize(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() { normalize(path) } else { normalize(&base.join(path)) }
}

/// The comparison key of a path: normalized and lower case.
pub fn key(path: &Path) -> String {
    normalize(path).to_string_lossy().to_lowercase()
}

/// Whether two paths name the same file, by [`key`].
pub fn same(a: &Path, b: &Path) -> bool {
    key(a) == key(b)
}

/// `path` relative to `base` when it lies under it (compared by [`key`]), otherwise `None`.
pub fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    let path = normalize(path);
    let base = normalize(base);
    let mut path_parts = path.components();
    for base_part in base.components() {
        let path_part = path_parts.next()?;
        if !component_eq(base_part, path_part) {
            return None;
        }
    }
    let rest: PathBuf = path_parts.collect();
    Some(rest)
}

fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
}

/// `path` shown relative to `base` when it lies under it, otherwise in full.
pub fn display_relative(path: &Path, base: &Path) -> String {
    match relative_to(path, base) {
        Some(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
        _ => path.display().to_string(),
    }
}

/// First eight hex digits of the SHA-256 of `text`. Used for short, stable directory names.
pub fn hash8(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))[..8].to_string()
}

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Replaces the characters Windows does not allow in a file name, so a configuration or
/// solution name can become part of one.
pub fn file_name_safe(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    // Trailing dots and spaces are dropped by Windows, which would merge distinct names.
    let trimmed = cleaned.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() { "_".to_string() } else { trimmed.to_string() }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn normalize_resolves_dots_and_separators() {
        assert_eq!(normalize(Path::new(r"C:\a\b\..\c\.\d")), PathBuf::from(r"C:\a\c\d"));
        assert_eq!(normalize(Path::new("c:/a/b/../c")), PathBuf::from(r"C:\a\c"));
        assert_eq!(normalize(Path::new(r"C:\..\a")), PathBuf::from(r"C:\a"));
        assert_eq!(normalize(Path::new(r"..\..\a")), PathBuf::from(r"..\..\a"));
        assert_eq!(normalize(Path::new(r"\\?\C:\a\b")), PathBuf::from(r"C:\a\b"));
        assert_eq!(normalize(Path::new(r"\\server\share\a\..\b")), PathBuf::from(r"\\server\share\b"));
    }

    #[test]
    fn absolutize_joins_relative_paths() {
        let base = Path::new(r"C:\repo\src\App");
        assert_eq!(absolutize(base, Path::new(r"..\Core\core.cpp")), PathBuf::from(r"C:\repo\src\Core\core.cpp"));
        assert_eq!(absolutize(base, Path::new(r"D:\x.cpp")), PathBuf::from(r"D:\x.cpp"));
        assert_eq!(
            absolutize(base, Path::new(r"sub dir\extra file.c")),
            PathBuf::from(r"C:\repo\src\App\sub dir\extra file.c")
        );
    }

    #[test]
    fn key_ignores_case_and_spelling() {
        assert_eq!(key(Path::new(r"C:\Repo\Main.CPP")), key(Path::new("c:/repo/./x/../main.cpp")));
        assert!(same(Path::new(r"C:\テスト\A.cpp"), Path::new(r"c:\テスト\a.cpp")));
    }

    #[test]
    fn relative_to_requires_a_prefix() {
        let base = Path::new(r"C:\Repo");
        assert_eq!(relative_to(Path::new(r"c:\repo\src\a.cpp"), base), Some(PathBuf::from(r"src\a.cpp")));
        assert_eq!(relative_to(Path::new(r"C:\Repository\a.cpp"), base), None);
        assert_eq!(relative_to(Path::new(r"D:\Repo\a.cpp"), base), None);
    }

    #[test]
    fn file_name_safe_replaces_reserved_characters() {
        assert_eq!(file_name_safe("Debug|x64"), "Debug_x64");
        assert_eq!(file_name_safe("Release DLL"), "Release DLL");
        assert_eq!(file_name_safe("a:b*c?"), "a_b_c_");
        assert_eq!(file_name_safe("trailing. "), "trailing");
        assert_eq!(file_name_safe(""), "_");
    }

    #[test]
    fn hash8_is_stable() {
        assert_eq!(hash8("abc"), "ba7816bf");
    }
}
