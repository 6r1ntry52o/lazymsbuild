//! Solutions (`.sln` and `.slnx`), read into one model.
//!
//! What lazymsbuild needs from a solution: its configurations ("profiles", `Debug|x64`), its C++
//! projects with the solution folders they sit in, the MSBuild target name of each project, and
//! for every solution configuration the project configuration it maps to and whether the project
//! is built in it.

mod names;
mod sln;
mod slnx;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::paths;

/// A configuration and platform pair, as a solution or a project names it (`Debug|x64`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Profile {
    pub configuration: String,
    pub platform: String,
}

impl Profile {
    pub fn new(configuration: impl Into<String>, platform: impl Into<String>) -> Profile {
        Profile { configuration: configuration.into(), platform: platform.into() }
    }

    /// Parses `Configuration|Platform`.
    pub fn parse(text: &str) -> Option<Profile> {
        let (configuration, platform) = text.split_once('|')?;
        let (configuration, platform) = (configuration.trim(), platform.trim());
        if configuration.is_empty() || platform.is_empty() {
            return None;
        }
        Some(Profile::new(configuration, platform))
    }

    /// Case-insensitive comparison, as Visual Studio and MSBuild compare configuration names.
    pub fn eq_ignore_case(&self, other: &Profile) -> bool {
        self.configuration.eq_ignore_ascii_case(&other.configuration)
            && self.platform.eq_ignore_ascii_case(&other.platform)
    }

    /// A file-name friendly form (`Debug_x64`).
    pub fn file_stem(&self) -> String {
        format!("{}_{}", paths::file_name_safe(&self.configuration), paths::file_name_safe(&self.platform))
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}|{}", self.configuration, self.platform)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolutionFormat {
    Sln,
    Slnx,
}

/// A solution folder.
#[derive(Clone, Debug)]
pub struct Folder {
    pub name: String,
    pub parent: Option<usize>,
}

/// The project configuration a solution configuration maps to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectConfig {
    pub profile: Profile,
    /// Whether the solution configuration builds the project (`Build.0` in a `.sln`).
    pub build: bool,
}

/// A project in a solution.
#[derive(Clone, Debug)]
pub struct Project {
    /// Display name (the name in the `.sln`, or the file name in a `.slnx`).
    pub name: String,
    /// Absolute path of the project file.
    pub path: PathBuf,
    /// The project GUID as written, with braces, when the solution has one.
    pub guid: Option<String>,
    /// A C++ project (`.vcxproj`). Other projects are kept for target names, but not built.
    pub is_cpp: bool,
    /// The solution folder the project is in.
    pub folder: Option<usize>,
    /// The MSBuild target that builds this project from the solution (`Libs\MyApp_Core`).
    pub target_name: String,
    /// Solution configuration -> project configuration. A solution configuration missing here
    /// does not include the project at all.
    pub configs: BTreeMap<Profile, ProjectConfig>,
}

impl Project {
    /// The project configuration for a solution configuration (compared case-insensitively).
    pub fn config_for(&self, profile: &Profile) -> Option<&ProjectConfig> {
        self.configs
            .get(profile)
            .or_else(|| self.configs.iter().find(|(p, _)| p.eq_ignore_case(profile)).map(|(_, c)| c))
    }

    /// Whether the solution configuration builds this project.
    pub fn builds_in(&self, profile: &Profile) -> bool {
        self.config_for(profile).is_some_and(|c| c.build)
    }

    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    pub fn key(&self) -> String {
        paths::key(&self.path)
    }
}

/// A solution.
#[derive(Clone, Debug)]
pub struct Solution {
    /// Absolute path of the `.sln` / `.slnx`.
    pub path: PathBuf,
    pub format: SolutionFormat,
    /// `VisualStudioVersion` from a `.sln` header (`17.14.36429.23`).
    pub visual_studio_version: Option<String>,
    /// Solution configurations, in the order the solution lists them.
    pub profiles: Vec<Profile>,
    pub projects: Vec<Project>,
    pub folders: Vec<Folder>,
}

impl Solution {
    /// Reads a `.sln` or `.slnx`.
    pub fn load(path: &Path) -> Result<Solution> {
        let path = paths::normalize(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
        let text = crate::text::read_text(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let is_slnx = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("slnx"));
        let mut solution = if is_slnx { slnx::parse(&path, &text) } else { sln::parse(&path, &text) }
            .with_context(|| format!("cannot parse {}", path.display()))?;
        names::assign_target_names(&mut solution);
        Ok(solution)
    }

    pub fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The C++ projects, in solution order.
    pub fn cpp_projects(&self) -> impl Iterator<Item = &Project> {
        self.projects.iter().filter(|p| p.is_cpp)
    }

    /// Finds a solution configuration by name, ignoring case.
    pub fn find_profile(&self, text: &str) -> Option<&Profile> {
        let wanted = Profile::parse(text)?;
        self.profiles.iter().find(|p| p.eq_ignore_case(&wanted))
    }

    /// Finds a project by its display name or its target name, ignoring case.
    pub fn find_project(&self, name: &str) -> Option<&Project> {
        self.projects
            .iter()
            .find(|p| p.target_name.eq_ignore_ascii_case(name))
            .or_else(|| self.projects.iter().find(|p| p.name.eq_ignore_ascii_case(name)))
    }

    /// The major Visual Studio version the solution was saved with (`17`).
    pub fn visual_studio_major(&self) -> Option<u32> {
        self.visual_studio_version.as_deref()?.split('.').next()?.parse().ok()
    }

    /// Names of the folders from the root down to `folder`.
    pub fn folder_path(&self, folder: Option<usize>) -> Vec<&str> {
        let mut names = Vec::new();
        let mut current = folder;
        while let Some(index) = current {
            let Some(f) = self.folders.get(index) else { break };
            names.push(f.name.as_str());
            current = f.parent;
            if names.len() > self.folders.len() {
                break; // a cycle in a hand-edited solution
            }
        }
        names.reverse();
        names
    }
}

/// Looks for solutions: in `start` and each directory above it, stopping at the first that has
/// any; failing that, in `git_root` and the directories below it (three levels, skipping hidden
/// and build output directories). Returns them sorted.
pub fn find_solutions(start: &Path, git_root: Option<&Path>) -> Vec<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let found = solutions_in(d);
        if !found.is_empty() {
            return found;
        }
        if git_root.is_some_and(|root| paths::same(root, d)) {
            break;
        }
        dir = d.parent();
    }
    let Some(root) = git_root else { return Vec::new() };
    let mut found = Vec::new();
    search_below(root, 3, &mut found);
    found.sort();
    found
}

fn solutions_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<PathBuf> =
        read.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file() && is_solution_file(p)).collect();
    found.sort();
    found
}

fn search_below(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    found.extend(solutions_in(dir));
    if depth == 0 {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let skip = name.starts_with('.')
            || matches!(
                name.as_str(),
                "node_modules"
                    | "packages"
                    | "bin"
                    | "obj"
                    | "out"
                    | "build"
                    | "target"
                    | "x64"
                    | "x86"
                    | "debug"
                    | "release"
            );
        if !skip && entry.file_type().is_ok_and(|t| t.is_dir()) {
            search_below(&path, depth - 1, found);
        }
    }
}

/// Whether a path names a solution file.
pub fn is_solution_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("sln") || e.eq_ignore_ascii_case("slnx"))
}

/// Picks the solution to use from a list found by [`find_solutions`].
pub fn pick_one(found: Vec<PathBuf>, start: &Path) -> Result<PathBuf> {
    match found.len() {
        0 => bail!("no .sln or .slnx found in {} or above it", start.display()),
        1 => Ok(found.into_iter().next().unwrap()),
        _ => {
            let list: Vec<String> = found.iter().map(|p| format!("  {}", p.display())).collect();
            bail!(
                "several solutions found; choose one with --solution or `solution = \"...\"` in .lazymsbuild.toml:\n{}",
                list.join("\n")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_parsing() {
        assert_eq!(Profile::parse("Debug|x64"), Some(Profile::new("Debug", "x64")));
        assert_eq!(Profile::parse(" Release | Any CPU "), Some(Profile::new("Release", "Any CPU")));
        assert_eq!(Profile::parse("Debug"), None);
        assert_eq!(Profile::parse("|x64"), None);
        assert_eq!(Profile::new("Debug", "x64").file_stem(), "Debug_x64");
        assert!(Profile::new("debug", "X64").eq_ignore_case(&Profile::new("Debug", "x64")));
    }
}
