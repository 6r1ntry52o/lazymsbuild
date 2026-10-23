//! The little Git that lazymsbuild needs: where the repository is, where its local ignore file is,
//! and whether a path is tracked or ignored. Everything works without Git installed; the checks
//! that need it then report that they could not run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

use crate::paths;

/// The repository root: the nearest directory at or above `start` with a `.git` directory or file
/// (a worktree or submodule has a `.git` file).
pub fn find_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return Some(paths::normalize(d));
        }
        dir = d.parent();
    }
    None
}

/// Whether the `git` command can be run.
pub fn available() -> bool {
    command(None).arg("--version").output().is_ok_and(|o| o.status.success())
}

fn command(root: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(root) = root {
        cmd.arg("-C").arg(root);
    }
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console window flashes when an editor runs lazymsbuild.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn run(root: &Path, args: &[&str]) -> Result<Output> {
    command(Some(root)).args(args).output().context("cannot run git")
}

/// Git's path for a repository file, e.g. `info/exclude`, which in a worktree is in the main
/// repository (`git rev-parse --git-path`).
pub fn git_path(root: &Path, name: &str) -> Result<PathBuf> {
    let output = run(root, &["rev-parse", "--git-path", name])?;
    if !output.status.success() {
        bail!("git rev-parse failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(paths::absolutize(root, Path::new(&text)))
}

/// The repository's local ignore file (`.git/info/exclude`).
pub fn info_exclude(root: &Path) -> Result<PathBuf> {
    git_path(root, "info/exclude")
}

fn pathspec(root: &Path, path: &Path) -> String {
    let rel = paths::relative_to(path, root).unwrap_or_else(|| path.to_path_buf());
    rel.to_string_lossy().replace('\\', "/")
}

/// Whether Git tracks a path.
pub fn is_tracked(root: &Path, path: &Path) -> Result<bool> {
    let spec = pathspec(root, path);
    let output = run(root, &["ls-files", "--error-unmatch", "--", &spec])?;
    Ok(output.status.success())
}

/// Whether Git ignores a path (so it never appears in `git status`).
pub fn is_ignored(root: &Path, path: &Path) -> Result<bool> {
    let spec = pathspec(root, path);
    let output = run(root, &["check-ignore", "-q", "--", &spec])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("git check-ignore failed: {}", String::from_utf8_lossy(&output.stderr).trim()),
    }
}

/// Paths `git status` reports as untracked or modified, among `paths`.
pub fn dirty(root: &Path, paths_to_check: &[PathBuf]) -> Result<Vec<String>> {
    let mut args = vec!["status".to_string(), "--porcelain".into(), "--untracked-files=all".into(), "--".into()];
    args.extend(paths_to_check.iter().map(|p| pathspec(root, p)));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run(root, &args)?;
    if !output.status.success() {
        bail!("git status failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().map(|l| l.to_string()).filter(|l| !l.is_empty()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_root_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let deep = dir.path().join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_root(&deep).map(|p| paths::key(&p)), Some(paths::key(dir.path())));
    }
}
