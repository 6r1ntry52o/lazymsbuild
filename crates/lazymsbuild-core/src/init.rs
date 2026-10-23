//! `lazymsbuild init`: prepares a repository so clangd finds the database and `git status` stays
//! clean (spec §4-1, §7-1).
//!
//! - repo-root mode: writes `.clangd` when there is none (an existing one is never changed; what
//!   it lacks is suggested), and keeps the generated files out of Git: in `.git/info/exclude` by
//!   default, which changes no tracked file, or in `.gitignore` after asking.
//! - external mode: writes nothing into the repository, and prints the block to add to clangd's
//!   user configuration (lazymsbuild never edits that file).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{IgnoreMode, OutputMode};
use crate::workspace::Workspace;
use crate::{clangd, fsutil, git, paths};

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub mode: Option<OutputMode>,
    pub ignore: Option<IgnoreMode>,
}

/// What `init` did and what it suggests.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub mode: Option<OutputMode>,
    /// Things done, for the user.
    pub done: Vec<String>,
    /// Things for the user to do.
    pub suggestions: Vec<String>,
    pub warnings: Vec<String>,
}

/// Runs `init`. `confirm` is asked before changing a tracked file (`.gitignore`).
pub fn run(workspace: &mut Workspace, options: &Options, confirm: &mut dyn FnMut(&str) -> bool) -> Result<Report> {
    let mut report = Report::default();
    if let Some(mode) = options.mode {
        workspace.set_mode(mode)?;
        report.done.push(format!("output mode: {}", mode_name(mode)));
    }
    let mode = workspace.output_mode();
    report.mode = Some(mode);

    if mode == OutputMode::External {
        let block = clangd::user_config_block(&workspace.root);
        let target = clangd::user_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "clangd's config.yaml".into());
        report.suggestions.push(format!(
            "clangd: pass --compile-commands-dir={} (`lazymsbuild path --db` prints it)",
            workspace.output_dir().display()
        ));
        report.suggestions.push(format!("add to {target} (lazymsbuild does not edit it):\n{block}"));
        report.warnings.push(
            "in external mode only editors set up for it use the database; clang-tidy and other tools do not find it"
                .into(),
        );
        return Ok(report);
    }

    let output = workspace.output_path();
    let clangd_file = workspace.root.join(clangd::FILE_NAME);

    // A tracked database is the team's: never written, never ignored.
    let git_ok = workspace.in_git && git::available();
    if git_ok && output.exists() && git::is_tracked(&workspace.root, &output).unwrap_or(false) {
        report.warnings.push(format!(
            "{} is tracked by Git, so lazymsbuild will not write it; use `lazymsbuild init --mode external`",
            workspace.display_path(&output)
        ));
    }

    // .clangd
    let generated_clangd;
    if clangd_file.exists() {
        let check = clangd::inspect(&clangd_file);
        generated_clangd = check.generated;
        if check.missing.is_empty() {
            report.done.push(format!("{} already has what clangd needs", workspace.display_path(&clangd_file)));
        } else {
            report.suggestions.push(format!(
                "{} exists, so lazymsbuild leaves it alone; add:\n{}",
                workspace.display_path(&clangd_file),
                clangd::suggestion(&check.missing)
            ));
        }
    } else {
        std::fs::write(&clangd_file, clangd::default_file())
            .with_context(|| format!("cannot write {}", clangd_file.display()))?;
        generated_clangd = true;
        report.done.push(format!("wrote {}", workspace.display_path(&clangd_file)));
    }

    // Ignoring
    let ignore = options.ignore.unwrap_or(workspace.config.init.ignore);
    if !workspace.in_git {
        report.done.push("not a Git repository: nothing to ignore".into());
        return Ok(report);
    }
    if ignore == IgnoreMode::None {
        return Ok(report);
    }
    let mut patterns = Vec::new();
    if let Some(rel) = paths::relative_to(&output, &workspace.root) {
        patterns.push(anchored(&rel));
        let cache = rel.parent().map(|p| p.join(".cache")).unwrap_or_else(|| PathBuf::from(".cache"));
        patterns.push(format!("{}/", anchored(&cache)));
    }
    let clangd_tracked =
        git_ok && clangd_file.exists() && git::is_tracked(&workspace.root, &clangd_file).unwrap_or(false);
    if generated_clangd && !clangd_tracked {
        patterns.push(anchored(Path::new(clangd::FILE_NAME)));
    }

    let file = match ignore {
        IgnoreMode::Exclude => match git::info_exclude(&workspace.root) {
            Ok(path) => path,
            Err(_) => workspace.root.join(r".git\info\exclude"),
        },
        IgnoreMode::Gitignore => workspace.root.join(".gitignore"),
        IgnoreMode::None => unreachable!(),
    };
    let existing: Vec<String> = crate::text::read_text(&file)
        .map(|t| crate::text::lines(&t).map(|l| l.trim().to_string()).collect())
        .unwrap_or_default();
    let mut missing: Vec<String> = patterns.into_iter().filter(|p| !existing.contains(p)).collect();
    if missing.is_empty() {
        report.done.push(format!("{} already ignores the generated files", workspace.display_path(&file)));
        return Ok(report);
    }
    if ignore == IgnoreMode::Gitignore
        && !confirm(&format!("Add {} to {}?", missing.join(", "), workspace.display_path(&file)))
    {
        report
            .suggestions
            .push(format!("not changed: {}; use `--ignore exclude` to keep it local", workspace.display_path(&file)));
        return Ok(report);
    }
    if !existing.iter().any(|l| l == "# lazymsbuild") {
        missing.insert(0, "# lazymsbuild".into());
    }
    fsutil::append_lines(&file, &missing).with_context(|| format!("cannot write {}", file.display()))?;
    report.done.push(format!(
        "added to {}: {}",
        workspace.display_path(&file),
        missing.iter().filter(|l| !l.starts_with('#')).cloned().collect::<Vec<_>>().join(", ")
    ));
    Ok(report)
}

/// A root-anchored Git pattern: `/compile_commands.json`.
fn anchored(rel: &Path) -> String {
    format!("/{}", rel.to_string_lossy().replace('\\', "/"))
}

pub fn mode_name(mode: OutputMode) -> &'static str {
    match mode {
        OutputMode::RepoRoot => "repo-root",
        OutputMode::External => "external",
    }
}
