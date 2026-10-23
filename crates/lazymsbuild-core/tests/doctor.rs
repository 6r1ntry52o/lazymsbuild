//! `doctor`, and above all what `--report` leaves out: a report goes into a public issue.

mod common;

use common::{build, sandbox};
use lazymsbuild_core::doctor::{self, Level, Options, Report};
use lazymsbuild_core::msbuild::Verb;
use lazymsbuild_core::status::State;
use lazymsbuild_core::workspace::{OpenOptions, Workspace};
use lazymsbuild_core::{clangd, solution};

fn text(report: &Report) -> String {
    serde_json::to_string(report).unwrap().replace("\\\\", "\\").to_ascii_lowercase()
}

fn item<'a>(report: &'a Report, name: &str) -> &'a doctor::Item {
    report.items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("no {name} in {report:?}"))
}

/// What a report must not contain: the sandbox's paths, the home directory, and the names of
/// the solution, its projects, files and configurations.
fn assert_anonymous(report: &Report, sb: &common::Sandbox) {
    let text = text(report);
    let base = sb.repo.parent().unwrap();
    let mut secrets = vec![base.display().to_string(), "lazymsbuild テスト".into()];
    if let Some(home) = dirs::home_dir() {
        secrets.push(home.display().to_string());
    }
    secrets.extend(
        ["Basic.sln", "Basic.slnx", "MyApp", "Util (Legacy)", "core.cpp", "util.c", "main.cpp", "Debug|", "Release|"]
            .map(String::from),
    );
    for secret in secrets {
        assert!(!text.contains(&secret.to_ascii_lowercase()), "the report contains {secret}: {text}");
    }
}

#[test]
fn a_report_leaves_out_paths_and_names() {
    require_msbuild!();
    let sb = sandbox("basic", false);
    let mut workspace = sb.workspace("Basic.sln");
    let (outcome, _) = build(&mut workspace, Verb::Rebuild, None);
    assert_eq!(outcome.state, State::Succeeded, "{:?}", outcome.notes);

    let plain = doctor::run(&workspace, &Options { report: false, check_files: 2 });
    assert!(text(&plain).contains("basic.sln"), "{plain:?}");
    assert_eq!(item(&plain, "database").level, Level::Ok, "{plain:?}");

    let report = doctor::run(&workspace, &Options { report: true, check_files: 2 });
    assert_anonymous(&report, &sb);
    assert!(item(&report, "solution").detail.contains("3 C++ project(s)"), "{report:?}");
    if clangd::find(None).is_some() {
        assert!(report.items.iter().any(|i| i.name == "analysis (project 1)"), "{report:?}");
    }
}

#[test]
fn without_a_solution_the_tools_are_still_checked() {
    // Basic.sln and Basic.slnx: which one is meant is unknown.
    let sb = sandbox("basic", false);
    let error = Workspace::open(&OpenOptions {
        start: sb.repo.clone(),
        solution: None,
        state_root: Some(sb.state.clone()),
        user_config_dir: Some(sb.config.clone()),
    })
    .err()
    .expect("two solutions");

    let plain = doctor::run_without_solution(&sb.repo, &error, Some(&sb.config), &Options::default());
    let entry = item(&plain, "solution");
    assert_eq!(entry.level, Level::Fail);
    assert!(entry.detail.contains("Basic.slnx"), "{entry:?}");
    for name in ["configuration", "MSBuild", "design-time extractor", "build logger", "clangd"] {
        item(&plain, name);
    }

    let report = doctor::run_without_solution(
        &sb.repo,
        &error,
        Some(&sb.config),
        &Options { report: true, ..Default::default() },
    );
    assert_eq!(item(&report, "solution").detail, "2 solutions found");
    assert_anonymous(&report, &sb);

    // Solutions are looked for above the directory too; a machine with one there skips this.
    let empty = tempfile::tempdir().unwrap();
    if solution::find_solutions(empty.path(), None).is_empty() {
        let error = anyhow::anyhow!("no .sln or .slnx found");
        let report = doctor::run_without_solution(empty.path(), &error, Some(&sb.config), &Options::default());
        assert_eq!(item(&report, "solution").detail, "no .sln or .slnx found");
    }
}
