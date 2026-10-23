//! The workspace: one solution, the repository it lives in, the settings that apply, and where
//! lazymsbuild keeps its state for it.
//!
//! State lives outside the repository, in `%LOCALAPPDATA%\lazymsbuild\<repo>-<hash>\`:
//!
//! ```text
//! active\compile_commands.json        external mode only
//! profiles\<solution>-<hash>\         one folder per solution
//!   <Config>_<Platform>.json          the configuration's database (the master copy)
//!   <Config>_<Platform>.meta.json     entry -> project, source (A/B), times, last build
//! status.json                          what the build is doing (docs/status-json.md)
//! errors.txt                           errors and warnings of the last build, MSVC format
//! logs\<stamp>.log                     build logs (the newest `logs.keep`)
//! tmp\                                 logger output, extractor output
//! build.lock                           held while a build or sync runs
//! state.json                           active configuration, output mode, what was published
//! ```
//!
//! Everything there can be deleted; `sync` recreates the databases.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{self, Config, OutputMode};
use crate::solution::{self, Profile, Solution};
use crate::{fsutil, git, paths};

/// How to find the workspace.
#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    /// Where to start looking for a solution (the current directory, or `-C`).
    pub start: PathBuf,
    /// An explicit solution (`--solution`).
    pub solution: Option<PathBuf>,
    /// The state root, instead of `%LOCALAPPDATA%\lazymsbuild` (tests).
    pub state_root: Option<PathBuf>,
    /// The user configuration directory, instead of searching for it (tests).
    pub user_config_dir: Option<PathBuf>,
}

/// The local state file (`state.json`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalState {
    pub format_version: u32,
    /// The output mode `init` chose for this repository.
    pub mode: Option<OutputMode>,
    /// The solution used last, so tools that only know the directory find the same one.
    pub last_solution: Option<PathBuf>,
    /// Per solution (by key): the active configuration.
    pub solutions: BTreeMap<String, SolutionState>,
    /// Per output file (by key): what lazymsbuild wrote there last.
    pub outputs: BTreeMap<String, OutputRecord>,
    /// The TUI offered `init` for this repository once (it does not ask again).
    pub init_offered: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SolutionState {
    pub path: PathBuf,
    pub active_profile: Option<String>,
}

/// What lazymsbuild last wrote to an output file. A file whose hash differs was changed by
/// something else.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputRecord {
    pub path: PathBuf,
    pub sha256: String,
    pub solution: PathBuf,
    pub profile: String,
    pub written_at: String,
}

pub const LOCAL_STATE_VERSION: u32 = 1;

#[derive(Clone)]
pub struct Workspace {
    pub solution: Solution,
    /// The repository root, or the solution's directory outside Git.
    pub root: PathBuf,
    pub in_git: bool,
    pub config: Config,
    pub config_files: Vec<PathBuf>,
    pub warnings: Vec<String>,
    /// `%LOCALAPPDATA%\lazymsbuild\<repo>-<hash>`.
    pub state_dir: PathBuf,
    pub local: LocalState,
}

impl Workspace {
    pub fn open(options: &OpenOptions) -> Result<Workspace> {
        let start = paths::normalize(&std::path::absolute(&options.start).unwrap_or_else(|_| options.start.clone()));
        let start_root = git::find_root(&start);

        // Settings first: .lazymsbuild.toml can name the solution.
        let user_dir = options.user_config_dir.as_deref();
        let early = config::load(user_dir, start_root.as_deref())?;
        let state_root = match &options.state_root {
            Some(root) => root.clone(),
            None => config::state_root()?,
        };
        let solution_path = match &options.solution {
            Some(path) => paths::absolutize(&start, path),
            None => match early.config.solution.as_deref().zip(start_root.as_deref()) {
                Some((configured, root)) => paths::absolutize(root, Path::new(configured)),
                None => {
                    let found = solution::find_solutions(&start, start_root.as_deref());
                    if found.len() > 1 {
                        // A tool that only knows the directory (the editor) uses the one used last.
                        let remembered = start_root.as_deref().and_then(|root| {
                            let local = read_local_state(&state_dir_for(&state_root, root));
                            local.last_solution.filter(|last| found.iter().any(|f| paths::same(f, last)))
                        });
                        match remembered {
                            Some(last) => last,
                            None => solution::pick_one(found, &start)?,
                        }
                    } else {
                        solution::pick_one(found, &start)?
                    }
                }
            },
        };
        if !solution_path.is_file() {
            bail!("solution not found: {}", solution_path.display());
        }
        let solution = Solution::load(&solution_path)?;

        let repo_root = git::find_root(solution.dir());
        let in_git = repo_root.is_some();
        let root = repo_root.unwrap_or_else(|| solution.dir().to_path_buf());
        let loaded = if start_root.as_deref().is_some_and(|r| paths::same(r, &root)) {
            early
        } else {
            config::load(user_dir, Some(&root))?
        };

        let state_dir = state_dir_for(&state_root, &root);
        let local = read_local_state(&state_dir);
        Ok(Workspace {
            solution,
            root,
            in_git,
            config: loaded.config,
            config_files: loaded.files,
            warnings: loaded.warnings,
            state_dir,
            local,
        })
    }

    /// The output mode: what `init` chose for this repository, otherwise the setting.
    pub fn output_mode(&self) -> OutputMode {
        self.local.mode.unwrap_or(self.config.output.mode)
    }

    /// Where the active configuration's database is published.
    pub fn output_path(&self) -> PathBuf {
        match self.output_mode() {
            OutputMode::RepoRoot => paths::absolutize(&self.root, &self.config.output.path),
            OutputMode::External => self.state_dir.join("active").join("compile_commands.json"),
        }
    }

    /// The directory clangd needs for `--compile-commands-dir`.
    pub fn output_dir(&self) -> PathBuf {
        self.output_path().parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone())
    }

    pub fn profiles_dir(&self) -> PathBuf {
        let stem = self.solution.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.state_dir.join("profiles").join(format!(
            "{}-{}",
            paths::file_name_safe(&stem),
            paths::hash8(&self.solution_key())
        ))
    }

    pub fn profile_db_path(&self, profile: &Profile) -> PathBuf {
        self.profiles_dir().join(format!("{}.json", profile.file_stem()))
    }

    pub fn profile_meta_path(&self, profile: &Profile) -> PathBuf {
        self.profiles_dir().join(format!("{}.meta.json", profile.file_stem()))
    }

    pub fn status_path(&self) -> PathBuf {
        self.state_dir.join("status.json")
    }

    pub fn errors_path(&self) -> PathBuf {
        self.state_dir.join("errors.txt")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.state_dir.join("tmp")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.state_dir.join("build.lock")
    }

    fn solution_key(&self) -> String {
        paths::key(&self.solution.path)
    }

    /// The configuration currently in use: the one chosen last, otherwise the default.
    pub fn active_profile(&self) -> Result<Profile> {
        let chosen = self
            .local
            .solutions
            .get(&self.solution_key())
            .and_then(|s| s.active_profile.as_deref())
            .and_then(|text| self.solution.find_profile(text))
            .cloned();
        match chosen.or_else(|| self.default_profile()) {
            Some(profile) => Ok(profile),
            None => bail!("{} has no configurations", self.solution.path.display()),
        }
    }

    /// The configuration to start with: the `profile` setting, then the one
    /// `msbuild-extractor.json` declares, then the first the solution lists.
    pub fn default_profile(&self) -> Option<Profile> {
        if let Some(profile) = self.config.profile.as_deref().and_then(|p| self.solution.find_profile(p)) {
            return Some(profile.clone());
        }
        if let Some(declared) = crate::extract::design_time::ConfigFile::find(self.solution.dir(), &self.root)
            .and_then(|file| file.declared_profile())
            .and_then(|p| self.solution.find_profile(&p.to_string()).cloned())
        {
            return Some(declared);
        }
        self.solution.profiles.first().cloned()
    }

    /// Makes `profile` the active configuration and remembers it.
    pub fn set_active_profile(&mut self, profile: &Profile) -> Result<()> {
        let key = self.solution_key();
        let entry = self.local.solutions.entry(key).or_default();
        entry.path = self.solution.path.clone();
        entry.active_profile = Some(profile.to_string());
        self.local.last_solution = Some(self.solution.path.clone());
        self.save_local_state()
    }

    /// Remembers this solution as the one used last.
    pub fn remember_solution(&mut self) -> Result<()> {
        if self.local.last_solution.as_deref().is_some_and(|p| paths::same(p, &self.solution.path)) {
            return Ok(());
        }
        self.local.last_solution = Some(self.solution.path.clone());
        self.save_local_state()
    }

    pub fn set_mode(&mut self, mode: OutputMode) -> Result<()> {
        self.local.mode = Some(mode);
        self.save_local_state()
    }

    /// The record of what lazymsbuild last wrote to `path`.
    pub fn output_record(&self, path: &Path) -> Option<&OutputRecord> {
        self.local.outputs.get(&paths::key(path))
    }

    pub fn record_output(&mut self, record: OutputRecord) -> Result<()> {
        self.local.outputs.insert(paths::key(&record.path), record);
        self.save_local_state()
    }

    /// Reads `state.json` again, after another process (or a build task) changed it.
    pub fn reload_local_state(&mut self) {
        self.local = read_local_state(&self.state_dir);
    }

    pub fn save_local_state(&mut self) -> Result<()> {
        self.local.format_version = LOCAL_STATE_VERSION;
        let json = serde_json::to_string_pretty(&self.local)?;
        fsutil::write_atomic(&self.state_dir.join("state.json"), json.as_bytes())
            .with_context(|| format!("cannot write {}", self.state_dir.join("state.json").display()))
    }

    /// `path` relative to the repository root when it is inside it.
    pub fn display_path(&self, path: &Path) -> String {
        paths::display_relative(path, &self.root)
    }
}

/// `<state root>\<repo name>-<hash of the root path>`.
pub fn state_dir_for(state_root: &Path, root: &Path) -> PathBuf {
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "root".into());
    state_root.join(format!("{}-{}", paths::file_name_safe(&name), paths::hash8(&paths::key(root))))
}

fn read_local_state(state_dir: &Path) -> LocalState {
    let path = state_dir.join("state.json");
    std::fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}
