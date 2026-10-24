//! "Is the database complete and current?" (`lazymsbuild check`, the database status of the TUI).
//!
//! | check | meaning |
//! |---|---|
//! | coverage | sources the configuration compiles (not `ExcludedFromBuild`) that have an entry |
//! | freshness | projects whose project files (`.vcxproj`, imports from the repository, `Directory.Build.*`, the solution) changed after their entries were captured |
//! | orphans | entries of sources no project lists and no build compiled (dropped by the next GC) |
//! | duplicates | sources several projects compile |
//! | sync | the published `compile_commands.json` equals the configuration's database |
//! | dual databases | another `compile_commands.json` that clangd would find instead of, or as well as, ours |
//! | ignored | the generated files do not show up in `git status` |
//!
//! Editing a source does not change its compile command, so sources are not part of freshness.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::config::OutputMode;
use crate::extract::Method;
use crate::solution::Profile;
use crate::store::{self, ProfileDb};
use crate::vcxproj::ProjectFile;
use crate::workspace::Workspace;
use crate::{fsutil, git, paths, time};

#[derive(Clone, Debug, Default, Serialize)]
pub struct Coverage {
    pub covered: usize,
    pub total: usize,
    /// Sources without an entry.
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StaleProject {
    pub name: String,
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Duplicate {
    pub file: String,
    pub projects: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncState {
    InSync,
    OutOfSync,
    NoOutput,
    NoDatabase,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "state", content = "paths")]
pub enum IgnoreState {
    Ignored,
    /// `git status` lists these.
    NotIgnored(Vec<String>),
    NotGit,
    GitUnavailable,
    /// External mode writes nothing into the repository.
    NotApplicable,
}

#[derive(Clone, Debug, Serialize)]
pub struct OtherDatabase {
    pub path: String,
    /// clangd reads it instead of ours for some sources (it is closer to them).
    pub shadows: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub profile: String,
    pub database: String,
    pub output: String,
    pub database_exists: bool,
    pub entries: usize,
    pub updated_at: Option<String>,
    /// How the entries were obtained (`design-time`, `build`), with counts.
    pub methods: BTreeMap<String, usize>,
    pub coverage: Coverage,
    pub stale: Vec<StaleProject>,
    pub orphans: Vec<String>,
    pub duplicates: Vec<Duplicate>,
    pub sync: SyncState,
    pub other_databases: Vec<OtherDatabase>,
    pub ignored: IgnoreState,
    /// Project -> item paths lazymsbuild could not resolve (they are not counted).
    pub unresolved: BTreeMap<String, Vec<String>>,
    /// Projects that could not be read.
    pub unreadable: BTreeMap<String, String>,
}

impl Report {
    /// The failing checks, worded for people. Orphans, duplicates and unresolved items are
    /// reported but do not fail.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if !self.database_exists {
            problems.push(format!(
                "no database for {} yet: run `lazymsbuild sync` (or rebuild the solution)",
                self.profile
            ));
            return problems;
        }
        if self.coverage.covered < self.coverage.total {
            problems.push(format!(
                "{} of {} sources have no entry: run `lazymsbuild sync`",
                self.coverage.total - self.coverage.covered,
                self.coverage.total
            ));
        }
        if !self.stale.is_empty() {
            problems.push(format!(
                "{} project(s) changed since their entries were captured: sync or build them",
                self.stale.len()
            ));
        }
        match self.sync {
            SyncState::OutOfSync => {
                problems.push(format!("{} differs from the {} database", self.output, self.profile))
            }
            SyncState::NoOutput => problems.push(format!("{} does not exist", self.output)),
            SyncState::InSync | SyncState::NoDatabase => {}
        }
        if !self.other_databases.is_empty() {
            problems.push(format!(
                "other compile_commands.json files that clangd may read: {}",
                self.other_databases.iter().map(|d| d.path.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        if let IgnoreState::NotIgnored(list) = &self.ignored {
            problems.push(format!("git status shows generated files: {} (run `lazymsbuild init`)", list.join(", ")));
        }
        problems
    }

    pub fn ok(&self) -> bool {
        self.problems().is_empty()
    }
}

/// The solution's C++ project files, parsed, by project key. Unreadable ones are reported.
pub fn load_projects(workspace: &Workspace) -> (HashMap<String, ProjectFile>, BTreeMap<String, String>) {
    let mut projects = HashMap::new();
    let mut unreadable = BTreeMap::new();
    for project in workspace.solution.cpp_projects() {
        match ProjectFile::load(&project.path, Some(workspace.solution.dir()), Some(&workspace.root)) {
            Ok(file) => {
                projects.insert(project.key(), file);
            }
            Err(err) => {
                unreadable.insert(workspace.display_path(&project.path), format!("{err:#}"));
            }
        }
    }
    (projects, unreadable)
}

/// Runs every check for a configuration.
pub fn run(workspace: &Workspace, profile: &Profile) -> Result<Report> {
    let (projects, unreadable) = load_projects(workspace);
    let db = ProfileDb::load(workspace, profile)?;
    run_with(workspace, profile, &db, &projects, unreadable)
}

pub fn run_with(
    workspace: &Workspace,
    profile: &Profile,
    db: &ProfileDb,
    projects: &HashMap<String, ProjectFile>,
    unreadable: BTreeMap<String, String>,
) -> Result<Report> {
    let solution = &workspace.solution;
    let solution_mtime = fsutil::mtime(&solution.path);

    let mut coverage = Coverage::default();
    let mut counted = BTreeSet::new();
    let mut compiled_by: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    let mut stale = Vec::new();
    let mut unresolved = BTreeMap::new();
    let mut listed_anywhere = BTreeSet::new();

    for project in solution.cpp_projects() {
        let Some(file) = projects.get(&project.key()) else { continue };
        for source in &file.sources {
            listed_anywhere.insert(source.key.clone());
        }
        if !file.unresolved.is_empty() {
            unresolved.insert(workspace.display_path(&project.path), file.unresolved.clone());
        }
        let Some(config) = project.config_for(profile).filter(|c| c.build) else { continue };

        for source in file.built_sources(&config.profile) {
            let slot = compiled_by
                .entry(source.key.clone())
                .or_insert_with(|| (workspace.display_path(&source.path), Vec::new()));
            slot.1.push(project.name.clone());
            if counted.insert(source.key.clone()) {
                coverage.total += 1;
                if db.entries.contains_key(&source.key) {
                    coverage.covered += 1;
                } else {
                    coverage.missing.push(workspace.display_path(&source.path));
                }
            }
        }

        // Freshness: the newest input against the capture time.
        let captured = db.meta.projects.get(&project.key()).and_then(|m| m.captured_at.clone());
        let newest = file
            .inputs
            .iter()
            .filter_map(|p| fsutil::mtime(p).map(|t| (t, p.clone())))
            .chain(solution_mtime.map(|t| (t, solution.path.clone())))
            .max_by_key(|(t, _)| *t);
        let reason = match (&captured, &newest) {
            (None, _) if db.exists => Some("never captured in this configuration".to_string()),
            (Some(at), Some((t, path))) if time::is_after(*t, at) => {
                Some(format!("{} changed after {}", workspace.display_path(path), time::clock(at)))
            }
            _ => None,
        };
        if let Some(reason) = reason {
            stale.push(StaleProject {
                name: project.name.clone(),
                path: workspace.display_path(&project.path),
                reason,
            });
        }
    }

    let orphans: Vec<String> = db
        .entries
        .iter()
        .filter(|(k, _)| !listed_anywhere.contains(*k))
        // A build compiled it, so its project has it, even if only a target adds it.
        .filter(|(k, _)| db.meta.entries.get(*k).is_none_or(|m| m.method != Method::Build))
        .map(|(_, e)| workspace.display_path(Path::new(&e.file)))
        .collect();
    let duplicates: Vec<Duplicate> = compiled_by
        .into_values()
        .filter(|(_, projects)| projects.len() > 1)
        .map(|(file, projects)| Duplicate { file, projects })
        .collect();

    let mut methods = BTreeMap::new();
    for key in db.entries.keys() {
        let method = db.meta.entries.get(key).map(|m| m.method.to_string()).unwrap_or_else(|| "unknown".into());
        *methods.entry(method).or_insert(0) += 1;
    }

    let output = workspace.output_path();
    let sync = if !db.exists {
        SyncState::NoDatabase
    } else {
        match store::in_sync(workspace, db)? {
            None => SyncState::NoOutput,
            Some(true) => SyncState::InSync,
            Some(false) => SyncState::OutOfSync,
        }
    };

    Ok(Report {
        profile: profile.to_string(),
        database: db.db_path.display().to_string(),
        output: workspace.display_path(&output),
        database_exists: db.exists,
        entries: db.entries.len(),
        updated_at: db.meta.updated_at.clone(),
        methods,
        coverage,
        stale,
        orphans,
        duplicates,
        sync,
        other_databases: other_databases(workspace, &output),
        ignored: ignore_state(workspace, &output),
        unresolved,
        unreadable,
    })
}

/// Other `compile_commands.json` files clangd could pick up, searching as clangd does: from each
/// project directory up to the repository root, `<dir>\compile_commands.json` then
/// `<dir>\build\compile_commands.json`, the first one found winning.
pub fn other_databases(workspace: &Workspace, output: &Path) -> Vec<OtherDatabase> {
    if workspace.output_mode() == OutputMode::External {
        return Vec::new();
    }
    let ours = paths::key(output);
    let mut found: BTreeMap<String, OtherDatabase> = BTreeMap::new();
    let mut dirs: Vec<PathBuf> = workspace
        .solution
        .cpp_projects()
        .map(|p| p.dir().to_path_buf())
        .filter(|d| paths::relative_to(d, &workspace.root).is_some())
        .collect();
    dirs.push(workspace.root.clone());
    for start in dirs {
        let mut ours_seen = false;
        let mut dir = Some(start.as_path());
        while let Some(d) = dir {
            for candidate in [d.join("compile_commands.json"), d.join("build").join("compile_commands.json")] {
                let key = paths::key(&candidate);
                if key == ours {
                    ours_seen = true;
                } else if candidate.is_file() {
                    let shadows = !ours_seen;
                    let entry = found
                        .entry(key)
                        .or_insert_with(|| OtherDatabase { path: workspace.display_path(&candidate), shadows });
                    entry.shadows |= shadows;
                }
            }
            if paths::same(d, &workspace.root) {
                break;
            }
            dir = d.parent();
        }
    }
    found.into_values().collect()
}

fn ignore_state(workspace: &Workspace, output: &Path) -> IgnoreState {
    if workspace.output_mode() == OutputMode::External {
        return IgnoreState::NotApplicable;
    }
    if !workspace.in_git {
        return IgnoreState::NotGit;
    }
    if !git::available() {
        return IgnoreState::GitUnavailable;
    }
    let candidates = [output.to_path_buf(), workspace.root.join(".cache"), workspace.root.join(".clangd")];
    let existing: Vec<PathBuf> = candidates.into_iter().filter(|p| p.exists()).collect();
    if existing.is_empty() {
        return IgnoreState::Ignored;
    }
    match git::dirty(&workspace.root, &existing) {
        Ok(lines) => {
            // Only untracked files: a tracked .clangd the user edits is theirs to commit.
            let untracked: Vec<String> =
                lines.iter().filter_map(|l| l.strip_prefix("?? ")).map(str::to_string).collect();
            if untracked.is_empty() { IgnoreState::Ignored } else { IgnoreState::NotIgnored(untracked) }
        }
        Err(_) => IgnoreState::GitUnavailable,
    }
}
