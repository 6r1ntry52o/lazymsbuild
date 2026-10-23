//! Builds the fixture solutions with the real MSBuild (skipped without it, see `require_msbuild!`).

mod common;

use std::collections::HashSet;
use std::process::Command;

use common::{build, sandbox};
use lazymsbuild_core::check::{IgnoreState, SyncState};
use lazymsbuild_core::msbuild::{Verb, locate};
use lazymsbuild_core::status::{self, State};
use lazymsbuild_core::{clangd, compdb, init};

#[test]
fn target_names_match_the_metaproject_msbuild_generates() {
    require_msbuild!();
    let sb = sandbox("basic", false);
    let msbuild = locate::locate(None, None).unwrap();
    for solution in ["Basic.sln", "Basic.slnx"] {
        let workspace = sb.workspace(solution);
        for profile in &workspace.solution.profiles {
            let output = Command::new(&msbuild.exe)
                .current_dir(&sb.repo)
                .env("MSBuildEmitSolution", "1")
                .args([
                    solution,
                    "-nologo",
                    "-nodeReuse:false",
                    "-v:q",
                    "-t:ValidateSolutionConfiguration",
                    &format!("-p:Configuration={}", profile.configuration),
                    &format!("-p:Platform={}", profile.platform),
                ])
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
            let metaproject = std::fs::read_to_string(sb.path(&format!("{solution}.metaproj"))).unwrap();
            let doc = roxmltree::Document::parse(&metaproject).unwrap();
            let targets: HashSet<&str> = doc
                .descendants()
                .filter(|n| n.tag_name().name() == "Target")
                .filter_map(|n| n.attribute("Name"))
                .collect();
            for project in workspace.solution.cpp_projects() {
                let builds = project.builds_in(profile);
                for suffix in ["", ":Rebuild", ":Clean"] {
                    let name = format!("{}{suffix}", project.target_name);
                    assert_eq!(targets.contains(name.as_str()), builds, "{solution} {profile}: target {name}");
                }
            }
        }
    }
}

#[test]
fn a_rebuild_fills_a_complete_database() {
    require_msbuild!();
    // Not a Git repository: ignoring is the subject of init_then_build_keeps_git_status_clean.
    let sb = sandbox("basic", false);
    let mut workspace = sb.workspace("Basic.sln");
    let (outcome, _) = build(&mut workspace, Verb::Rebuild, None);
    assert_eq!(outcome.state, State::Succeeded, "{:?}", outcome.notes);
    let check = outcome.check.clone().unwrap();
    assert_eq!((check.coverage.covered, check.coverage.total), (7, 7), "{:?}", check.coverage.missing);
    assert!(check.stale.is_empty(), "{:?}", check.stale);
    assert_eq!(check.sync, SyncState::InSync);
    assert!(check.ok(), "{:?}", check.problems());

    // MSBuild passes IncludePath and ExternalIncludePath in the environment; the entries carry them.
    let entries = compdb::read(&workspace.output_path()).unwrap();
    let util = entries.iter().find(|e| e.file.ends_with("util.c")).unwrap();
    assert_eq!(util.arguments[1], "--target=x86_64-pc-windows-msvc");
    for dir in [r"third_party\vendor_sdk\include", r"third_party\external_sdk\include"] {
        assert!(
            util.arguments.iter().any(|a| a.starts_with("/imsvc") && a.ends_with(dir)),
            "{dir}: {:?}",
            util.arguments
        );
    }
    let extra = entries.iter().find(|e| e.file.ends_with("extra file.c")).unwrap();
    assert!(extra.file.ends_with(r"src\Util\sub dir\extra file.c"));

    // Nothing to compile: nothing changes.
    let (again, _) = build(&mut workspace, Verb::Build, None);
    assert_eq!(again.state, State::Succeeded);
    let report = again.report.unwrap();
    assert_eq!((report.added, report.updated, report.removed, report.after), (0, 0, 0, 7));
}

#[test]
fn win32_entries_pass_clangd_check() {
    require_msbuild!();
    let Some((clangd, _)) = clangd::find(None) else {
        eprintln!("skipped: clangd not found");
        return;
    };
    let sb = sandbox("basic", false);
    let mut workspace = sb.workspace("Basic.sln");
    let profile = workspace.solution.find_profile("Debug|x86").unwrap().clone();
    workspace.set_active_profile(&profile).unwrap();
    let (outcome, _) = build(&mut workspace, Verb::Rebuild, None);
    assert_eq!(outcome.state, State::Succeeded, "{:?}", outcome.notes);
    // core.cpp asserts the pointer size, util.c includes headers found only through IncludePath
    // and ExternalIncludePath, main.cpp uses a precompiled header.
    for file in [r"src\Core\core.cpp", r"src\Util\util.c", r"src\Util\sub dir\extra file.c", r"src\App\main.cpp"] {
        let result = clangd::check_file(&clangd, &sb.path(file), None).unwrap();
        assert!(result.from_database, "{file}: clangd did not use the database");
        assert_eq!(result.errors, Some(0), "{file}: {:?}", result.diagnostics);
    }
}

#[test]
fn clang_tidy_reads_the_database_without_p() {
    require_msbuild!();
    let msbuild = locate::locate(None, None).unwrap();
    let tidy = lazymsbuild_core::fsutil::which("clang-tidy").or_else(|| {
        let instance = msbuild.instance.as_ref()?;
        Some(instance.path.join(r"VC\Tools\Llvm\x64\bin\clang-tidy.exe")).filter(|p| p.is_file())
    });
    let Some(tidy) = tidy else {
        eprintln!("skipped: clang-tidy not found");
        return;
    };
    let sb = sandbox("basic", false);
    let mut workspace = sb.workspace("Basic.sln");
    let (outcome, _) = build(&mut workspace, Verb::Rebuild, None);
    assert_eq!(outcome.state, State::Succeeded, "{:?}", outcome.notes);
    // main.cpp uses App's precompiled header and pch.cpp makes it: clang-tidy reads neither .pch.
    for file in [r"src\App\main.cpp", r"src\App\pch.cpp", r"src\Core\core.cpp", r"src\Util\util.c"] {
        let output =
            Command::new(&tidy).current_dir(&sb.repo).args(["--checks=-*,clang-analyzer-*", file]).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(!text.contains("compilation database"), "{file}: clang-tidy did not find the database: {text}");
        assert!(output.status.success(), "{file}: {text}");
    }
}

#[test]
fn a_failed_build_keeps_the_entries() {
    require_msbuild!();
    let sb = sandbox("basic", false);
    let mut workspace = sb.workspace("Basic.sln");
    let (first, _) = build(&mut workspace, Verb::Rebuild, None);
    assert_eq!(first.state, State::Succeeded);

    std::fs::write(sb.path(r"src\Core\core.cpp"), "int broken() { return undeclared_name; }\n").unwrap();
    let (failed, _) = build(&mut workspace, Verb::Rebuild, None);
    assert_eq!(failed.state, State::Failed);
    assert!(failed.diagnostics.iter().any(|d| d.code.as_deref() == Some("C2065")), "{:?}", failed.diagnostics);
    let check = failed.check.unwrap();
    assert_eq!((check.entries, check.coverage.covered), (7, 7), "a failed rebuild replaces nothing");

    let errors = std::fs::read_to_string(workspace.errors_path()).unwrap();
    assert!(errors.lines().any(|l| l.contains(r"src\Core\core.cpp(1,") && l.contains(": error C2065:")), "{errors}");
    let status = status::read(&workspace.status_path()).unwrap();
    assert_eq!(status.state, State::Failed);
    assert!(status.errors >= 1);
    assert!(status.finished_at.is_some());
}

#[test]
fn rebuilding_one_project_replaces_only_its_entries() {
    require_msbuild!();
    let sb = sandbox("basic", false);
    let mut workspace = sb.workspace("Basic.sln");
    let (first, _) = build(&mut workspace, Verb::Rebuild, None);
    assert_eq!(first.check.unwrap().entries, 7);

    // special.cpp leaves the project but stays on disk.
    let project = sb.path(r"src\Core\MyApp.Core.vcxproj");
    let text = std::fs::read_to_string(&project).unwrap();
    let start = text.find(r#"<ClCompile Include="special.cpp">"#).unwrap();
    let end = text[start..].find("</ClCompile>").unwrap() + start + "</ClCompile>".len();
    std::fs::write(&project, format!("{}{}", &text[..start], &text[end..])).unwrap();

    let (outcome, _) = build(&mut workspace, Verb::Rebuild, Some("MyApp.Core"));
    assert_eq!(outcome.state, State::Succeeded, "{:?}", outcome.notes);
    let entries = compdb::read(&workspace.output_path()).unwrap();
    assert_eq!(entries.len(), 6);
    assert!(!entries.iter().any(|e| e.file.ends_with("special.cpp")));
    assert!(entries.iter().any(|e| e.file.ends_with("main.cpp")), "other projects keep their entries");
}

#[test]
fn clean_leaves_the_database_alone() {
    require_msbuild!();
    let sb = sandbox("basic", false);
    let mut workspace = sb.workspace("Basic.sln");
    build(&mut workspace, Verb::Rebuild, None);
    let before = std::fs::read(workspace.output_path()).unwrap();
    let (clean, _) = build(&mut workspace, Verb::Clean, None);
    assert_eq!(clean.state, State::Succeeded);
    assert!(clean.report.is_none());
    assert_eq!(std::fs::read(workspace.output_path()).unwrap(), before);
}

#[test]
fn init_then_build_keeps_git_status_clean() {
    require_msbuild!();
    if !common::git_available() {
        eprintln!("skipped: git not found");
        return;
    }
    let sb = sandbox("basic", true);
    let mut workspace = sb.workspace("Basic.sln");
    let report = init::run(&mut workspace, &init::Options::default(), &mut |_| false).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(sb.path(".clangd").is_file());
    let (outcome, _) = build(&mut workspace, Verb::Rebuild, None);
    assert_eq!(outcome.state, State::Succeeded);
    std::fs::create_dir_all(sb.path(r".cache\clangd\index")).unwrap();
    let status = Command::new("git").current_dir(&sb.repo).args(["status", "--porcelain"]).output().unwrap();
    // Build output under src\*\x64 is not part of the question; only what lazymsbuild generates.
    let dirty: Vec<String> = String::from_utf8_lossy(&status.stdout)
        .lines()
        .filter(|l| l.contains("compile_commands") || l.contains(".clangd") || l.contains(".cache"))
        .map(str::to_string)
        .collect();
    assert!(dirty.is_empty(), "{dirty:?}");
    assert_eq!(outcome.check.unwrap().ignored, IgnoreState::Ignored);
}

#[test]
fn sync_fills_a_complete_database() {
    require_msbuild!();
    let sb = sandbox("basic", false);
    let mut workspace = sb.workspace("Basic.sln");
    if matches!(
        lazymsbuild_core::extract::design_time::find(None, &workspace.root),
        lazymsbuild_core::extract::design_time::Lookup::Missing(_)
    ) {
        eprintln!("skipped: msbuild-extractor-sample not installed");
        return;
    }
    let outcome = common::sync(&mut workspace);
    assert_eq!(outcome.state, State::Succeeded, "{:?}", outcome.notes);
    let check = outcome.check.unwrap();
    assert_eq!((check.coverage.covered, check.coverage.total), (7, 7));
    assert!(check.ok(), "{:?}", check.problems());
    // A build afterwards replaces the design-time entries of what it compiles.
    let (built, _) = build(&mut workspace, Verb::Rebuild, None);
    assert_eq!(built.check.unwrap().methods.get("build"), Some(&7));
}
