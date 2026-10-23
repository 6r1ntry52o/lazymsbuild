//! `lazymsbuild doctor`: is everything in place for clangd to use the database (spec §7-4)?
//!
//! With `report`, the output is meant for an issue: absolute paths are masked and no project,
//! file or configuration name appears, only counts.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::check::{self, IgnoreState, SyncState};
use crate::config::{self, Config, OutputMode};
use crate::extract::{build_log, design_time};
use crate::msbuild::locate;
use crate::solution::{self, Profile};
use crate::workspace::Workspace;
use crate::{clangd, git};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub name: String,
    pub level: Level,
    pub detail: String,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub lazymsbuild: String,
    pub items: Vec<Item>,
}

impl Report {
    pub fn worst(&self) -> Level {
        self.items.iter().map(|i| i.level).max().unwrap_or(Level::Ok)
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// For an issue: no paths, no names.
    pub report: bool,
    /// How many projects get a `clangd --check` of their first source.
    pub check_files: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { report: false, check_files: 8 }
    }
}

struct Items {
    items: Vec<Item>,
    /// What `report` replaces, in order: (path, placeholder).
    masks: Vec<(PathBuf, &'static str)>,
    report: bool,
}

impl Items {
    /// `state_dir` is masked before `root`, and both before the home directory that holds them.
    fn new(report: bool, state_dir: Option<&Path>, root: &Path) -> Items {
        let mut masks: Vec<(PathBuf, &'static str)> = Vec::new();
        if let Some(dir) = state_dir {
            masks.push((dir.to_path_buf(), "<state>"));
        }
        masks.push((root.to_path_buf(), "<repo>"));
        if let Some(home) = dirs::home_dir() {
            masks.push((home, "~"));
        }
        Items { items: Vec::new(), masks, report }
    }

    fn push(&mut self, name: &str, level: Level, detail: impl Into<String>, hint: Option<String>) {
        let mut detail = detail.into();
        let mut hint = hint;
        if self.report {
            detail = mask(&detail, &self.masks);
            hint = hint.map(|h| mask(&h, &self.masks));
        }
        self.items.push(Item { name: name.into(), level, detail, hint });
    }

    fn path(&self, path: &Path) -> String {
        if self.report { mask(&path.display().to_string(), &self.masks) } else { path.display().to_string() }
    }
}

/// Replaces each path of `masks` in `text`, ignoring ASCII case.
fn mask(text: &str, masks: &[(PathBuf, &str)]) -> String {
    let mut out = text.to_string();
    for (path, with) in masks {
        let needle = path.display().to_string();
        if needle.len() < 3 {
            continue;
        }
        // ASCII-only lowering keeps byte offsets equal between the two strings.
        let lower = out.to_ascii_lowercase();
        let needle_lower = needle.to_ascii_lowercase();
        let mut result = String::with_capacity(out.len());
        let mut last = 0;
        for (index, _) in lower.match_indices(&needle_lower) {
            result.push_str(&out[last..index]);
            result.push_str(with);
            last = index + needle.len();
        }
        result.push_str(&out[last..]);
        out = result;
    }
    out
}

fn configuration(items: &mut Items, files: &[PathBuf], warnings: &[String]) {
    let files: Vec<String> = files.iter().map(|f| items.path(f)).collect();
    if warnings.is_empty() {
        let detail = if files.is_empty() { "defaults (no configuration file)".to_string() } else { files.join(", ") };
        items.push("configuration", Level::Ok, detail, None);
    } else {
        items.push("configuration", Level::Warn, warnings.join("; "), Some("see docs/config.md".into()));
    }
}

/// Without a solution (none found, several, or one that cannot be read), the checks that do not
/// need one, so that `doctor` still says whether MSBuild, the extractors and clangd are in place.
/// `error` is why the workspace did not open; `user_config_dir` as in `OpenOptions`.
pub fn run_without_solution(
    start: &Path,
    error: &anyhow::Error,
    user_config_dir: Option<&Path>,
    options: &Options,
) -> Report {
    // As Workspace::open sees it, so that the paths in `error` are masked.
    let start = &crate::paths::normalize(start);
    let git_root = git::find_root(start);
    let root = git_root.clone().unwrap_or_else(|| start.to_path_buf());
    let mut items = Items::new(options.report, None, &root);
    let config = match config::load(user_config_dir, git_root.as_deref()) {
        Ok(loaded) => {
            configuration(&mut items, &loaded.files, &loaded.warnings);
            loaded.config
        }
        Err(err) => {
            items.push("configuration", Level::Fail, format!("{err:#}"), Some("see docs/config.md".into()));
            Config::default()
        }
    };
    let found = solution::find_solutions(start, git_root.as_deref()).len();
    let (detail, hint) = match found {
        0 => ("no .sln or .slnx found".to_string(), "run doctor in a solution's directory, or pass -C <dir>"),
        1 if options.report => ("the solution cannot be read (doctor without --report says why)".to_string(), ""),
        1 => (format!("{error:#}"), ""),
        n if options.report => (format!("{n} solutions found"), "choose one with --solution"),
        _ => (format!("{error:#}"), "choose one with --solution"),
    };
    items.push("solution", Level::Fail, detail, (!hint.is_empty()).then(|| hint.to_string()));
    tools(&mut items, &config, None, &root);
    Report { lazymsbuild: env!("CARGO_PKG_VERSION").to_string(), items: items.items }
}

pub fn run(workspace: &Workspace, options: &Options) -> Report {
    let mut items = Items::new(options.report, Some(&workspace.state_dir), &workspace.root);
    let solution = &workspace.solution;
    configuration(&mut items, &workspace.config_files, &workspace.warnings);

    // Solution
    let profile = workspace.active_profile().ok();
    let cpp = solution.cpp_projects().count();
    let detail = if options.report {
        format!("{} C++ project(s), {} configuration(s)", cpp, solution.profiles.len())
    } else {
        format!(
            "{} ({} C++ project(s); active {})",
            solution.path.display(),
            cpp,
            profile.as_ref().map(|p| p.to_string()).unwrap_or_else(|| "none".into())
        )
    };
    items.push(
        "solution",
        if cpp == 0 { Level::Warn } else { Level::Ok },
        detail,
        (cpp == 0).then(|| "the solution has no .vcxproj".into()),
    );

    let clangd_exe = tools(&mut items, &workspace.config, solution.visual_studio_major(), &workspace.root);
    workspace_checks(&mut items, workspace, profile.as_ref(), clangd_exe.as_ref(), options);
    Report { lazymsbuild: env!("CARGO_PKG_VERSION").to_string(), items: items.items }
}

/// MSBuild, the two extractors and clangd. Returns clangd, for the analysis.
fn tools(
    items: &mut Items,
    config: &Config,
    visual_studio_major: Option<u32>,
    root: &Path,
) -> Option<(PathBuf, &'static str)> {
    // MSBuild
    match locate::locate(config.build.msbuild.as_deref(), visual_studio_major) {
        Ok(msbuild) => {
            let version = locate::version(&msbuild.exe).unwrap_or_else(|_| "?".into());
            let instance =
                msbuild.instance.as_ref().map(|i| format!(" ({} {})", i.display_name, i.version)).unwrap_or_default();
            let mismatch = match (visual_studio_major, msbuild.instance.as_ref().and_then(|i| i.major())) {
                (Some(wanted), Some(found)) if wanted != found => {
                    Some(format!("the solution was saved by Visual Studio {wanted}, the build uses {found}"))
                }
                _ => None,
            };
            let level = if mismatch.is_some() { Level::Info } else { Level::Ok };
            items.push("MSBuild", level, format!("{} {version}{instance}", items.path(&msbuild.exe)), mismatch);
        }
        Err(err) => items.push(
            "MSBuild",
            Level::Fail,
            format!("{err:#}"),
            Some(
                "install Visual Studio or Build Tools with \"Desktop development with C++\", or set build.msbuild"
                    .into(),
            ),
        ),
    }

    // Design-time extractor
    match design_time::find(config.extractor.design_time.as_deref(), root) {
        design_time::Lookup::Found { exe, origin } => {
            let version = extractor_version(&exe);
            let hostfxr = exe.parent().is_some_and(|d| d.join("hostfxr.dll").is_file());
            if hostfxr {
                items.push("design-time extractor", Level::Ok, format!("{} {version} (from {origin})", items.path(&exe)), None);
            } else {
                items.push(
                    "design-time extractor",
                    Level::Warn,
                    format!("{} {version} (from {origin}); no hostfxr.dll next to it", items.path(&exe)),
                    Some(
                        "without it the extractor fails with DllNotFoundException when it cannot find Visual Studio's MSBuild; \
                         copy hostfxr.dll from %ProgramFiles%\\dotnet\\host\\fxr\\<version>\\ next to the exe \
                         (scripts/install-extractor.ps1 does this)"
                            .into(),
                    ),
                );
            }
        }
        design_time::Lookup::Disabled => items.push("design-time extractor", Level::Info, "off (extractor.design_time = \"off\")", None),
        design_time::Lookup::Missing(searched) => items.push(
            "design-time extractor",
            Level::Warn,
            format!("not found (searched {})", searched.join(", ")),
            Some("`sync` needs it: run scripts/install-extractor.ps1. Without it, rebuild the solution once to fill the database".into()),
        ),
    }

    // Build logger
    match build_log::find_logger(config.extractor.logger.as_deref()) {
        build_log::Lookup::Found(dll) => items.push("build logger", Level::Ok, items.path(&dll), None),
        build_log::Lookup::Disabled => items.push("build logger", Level::Info, "off: builds do not update the database", None),
        build_log::Lookup::Missing(tried) => items.push(
            "build logger",
            Level::Warn,
            format!("{} not found: builds do not update the database", build_log::LOGGER_DLL),
            Some(format!(
                "it ships next to lazymsbuild.exe in the release archive; after `cargo install` or `cargo binstall`, copy it from the archive next to lazymsbuild.exe, or build logger/ with `dotnet build -c Release` and set extractor.logger (tried {})",
                tried.iter().map(|p| items.path(p)).collect::<Vec<_>>().join(", ")
            )),
        ),
    }

    // clangd
    let clangd_exe = clangd::find(config.clangd.path.as_deref());
    match &clangd_exe {
        Some((exe, origin)) => {
            let version = clangd::version(exe).unwrap_or_else(|e| format!("cannot run: {e:#}"));
            items.push("clangd", Level::Ok, format!("{version} ({}, from {origin})", items.path(exe)), None);
        }
        None => items.push(
            "clangd",
            Level::Warn,
            "not found (PATH, Mason, Visual Studio's LLVM, VS Code's clangd extension)",
            Some("install clangd (LLVM releases, or :MasonInstall clangd in Neovim) or set clangd.path".into()),
        ),
    }
    clangd_exe
}

/// The database, the analysis, the clangd configuration and Git.
fn workspace_checks(
    items: &mut Items,
    workspace: &Workspace,
    profile: Option<&Profile>,
    clangd_exe: Option<&(PathBuf, &'static str)>,
    options: &Options,
) {
    let solution = &workspace.solution;

    // Database
    let check = profile.and_then(|p| check::run(workspace, p).ok());
    match (profile, &check) {
        (Some(p), Some(c)) => {
            let problems = c.problems();
            // A configuration name can be a product's, so a report leaves it out.
            let configuration = if options.report { "active configuration".to_string() } else { p.to_string() };
            let summary = format!(
                "{configuration}: {} entries, coverage {}/{}, {} stale, {}",
                c.entries,
                c.coverage.covered,
                c.coverage.total,
                c.stale.len(),
                match c.sync {
                    SyncState::InSync => "published",
                    SyncState::OutOfSync => "published copy differs",
                    SyncState::NoOutput => "not published",
                    SyncState::NoDatabase => "no database",
                }
            );
            let level = if problems.is_empty() {
                Level::Ok
            } else if c.database_exists {
                Level::Warn
            } else {
                Level::Fail
            };
            let hint = (!problems.is_empty()).then(|| {
                if options.report {
                    format!("{} problem(s); run `lazymsbuild check`", problems.len())
                } else {
                    problems.join("; ")
                }
            });
            items.push("database", level, summary, hint);
        }
        _ => items.push("database", Level::Fail, "cannot check the database", None),
    }

    // Analysis: clangd --check on the first source of the first projects.
    if let (Some((exe, _)), Some(p)) = (clangd_exe, profile) {
        let (projects, _) = check::load_projects(workspace);
        let external = (workspace.output_mode() == OutputMode::External).then(|| workspace.output_dir());
        let mut checked = 0;
        for project in solution.cpp_projects().filter(|pr| pr.builds_in(p)) {
            if checked == options.check_files {
                break;
            }
            let Some(config) = project.config_for(p) else { continue };
            let Some(source) = projects.get(&project.key()).and_then(|f| f.built_sources(&config.profile).next())
            else {
                continue;
            };
            checked += 1;
            let name = if options.report { format!("project {checked}") } else { project.name.clone() };
            match clangd::check_file(exe, &source.path, external.as_deref()) {
                Ok(result) => {
                    let errors = result.errors;
                    let level = match errors {
                        Some(0) if result.from_database => Level::Ok,
                        Some(0) => Level::Warn,
                        _ => Level::Fail,
                    };
                    let mut detail = match errors {
                        Some(n) => format!("{n} error(s)"),
                        None => "clangd did not finish".into(),
                    };
                    if !result.from_database {
                        detail.push_str(", not from the database (clangd guessed the command)");
                    }
                    if !options.report {
                        detail = format!("{}: {detail}", workspace.display_path(&source.path));
                    }
                    let hint = (!options.report && !result.diagnostics.is_empty())
                        .then(|| result.diagnostics.iter().take(3).cloned().collect::<Vec<_>>().join("; "));
                    items.push(&format!("analysis ({name})"), level, detail, hint);
                }
                Err(err) => items.push(&format!("analysis ({name})"), Level::Fail, format!("{err:#}"), None),
            }
        }
        if checked == 0 {
            items.push("analysis", Level::Info, "no source to check", None);
        }
    }

    // clangd configuration
    match workspace.output_mode() {
        OutputMode::RepoRoot => {
            let file = workspace.root.join(clangd::FILE_NAME);
            let c = clangd::inspect(&file);
            if !c.exists {
                items.push(".clangd", Level::Warn, "missing", Some("run `lazymsbuild init`".into()));
            } else if c.missing.is_empty() {
                items.push(".clangd", Level::Ok, "removes MSVC precompiled-header flags", None);
            } else {
                items.push(
                    ".clangd",
                    Level::Warn,
                    format!("lacks Remove entries: {}", c.missing.join(", ")),
                    Some(clangd::suggestion(&c.missing)),
                );
            }
        }
        OutputMode::External => {
            let file = clangd::user_config_path();
            let c = file.as_deref().map(clangd::inspect).unwrap_or_default();
            if c.exists && c.missing.is_empty() {
                items.push("clangd user configuration", Level::Ok, "removes MSVC precompiled-header flags", None);
            } else {
                items.push(
                    "clangd user configuration",
                    Level::Warn,
                    format!("{} lacks the Remove entries", file.map(|f| items.path(&f)).unwrap_or_default()),
                    Some(clangd::user_config_block(&workspace.root)),
                );
            }
        }
    }

    // Git
    if let Some(c) = &check {
        match &c.ignored {
            IgnoreState::Ignored => items.push("git", Level::Ok, "generated files are ignored", None),
            IgnoreState::NotIgnored(list) => items.push(
                "git",
                Level::Warn,
                if options.report {
                    format!("{} generated file(s) show in git status", list.len())
                } else {
                    format!("git status shows {}", list.join(", "))
                },
                Some("run `lazymsbuild init`".into()),
            ),
            IgnoreState::NotGit => items.push("git", Level::Info, "not a Git repository", None),
            IgnoreState::GitUnavailable => {
                items.push("git", Level::Info, "git is not available; ignoring was not checked", None)
            }
            IgnoreState::NotApplicable => {
                items.push("git", Level::Ok, "external mode writes nothing into the repository", None)
            }
        }
        if !c.other_databases.is_empty() {
            let names: Vec<String> = c.other_databases.iter().map(|d| d.path.clone()).collect();
            let shadows = c.other_databases.iter().any(|d| d.shadows);
            items.push(
                "other databases",
                if shadows { Level::Fail } else { Level::Warn },
                if options.report { format!("{} other compile_commands.json", names.len()) } else { names.join(", ") },
                Some(if shadows {
                    "clangd reads these instead of lazymsbuild's for some sources; delete them".into()
                } else {
                    "a second database (for example build/compile_commands.json from an older setup) goes stale; delete it".into()
                }),
            );
        }
    }
}

fn extractor_version(exe: &Path) -> String {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--version").stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    match cmd.output() {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            let version = text.lines().next().unwrap_or_default().trim();
            // "0.3.0+<commit>": the commit adds nothing for people.
            version.split('+').next().unwrap_or(version).to_string()
        }
        Err(_) => "?".into(),
    }
}
