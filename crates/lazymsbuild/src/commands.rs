//! The subcommands. Each returns the process exit code: 0 success, 1 failure (a failed build,
//! problems found by `check`/`doctor`); usage errors (2) come from clap.

use std::io::{IsTerminal, Write as _};

use anyhow::{Context, Result, bail};
use lazymsbuild_core::check::{self, IgnoreState, SyncState};
use lazymsbuild_core::config::{self, IgnoreMode, OutputMode};
use lazymsbuild_core::diag::{Diagnostic, Severity};
use lazymsbuild_core::doctor::{self, Level};
use lazymsbuild_core::engine::{self, BuildRequest, Event, Outcome};
use lazymsbuild_core::msbuild::Verb;
use lazymsbuild_core::solution::Profile;
use lazymsbuild_core::status::State;
use lazymsbuild_core::workspace::{OpenOptions, Workspace};
use lazymsbuild_core::{init, time};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::cli::{
    BuildArgs, CompletionsArgs, DoctorArgs, Global, IgnoreArg, InitArgs, ModeArg, PathArgs, ProfileArg, SyncArgs,
};
use crate::style::{self, say};

pub fn open_workspace(global: &Global) -> Result<Workspace> {
    let start = match &global.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().context("cannot read the current directory")?,
    };
    let workspace = Workspace::open(&OpenOptions { start, solution: global.solution.clone(), ..Default::default() })?;
    for warning in &workspace.warnings {
        eprintln!("{} {warning}", style::yellow("warning:"));
    }
    Ok(workspace)
}

/// Finds a configuration given as `Configuration|Platform` (or `Configuration/Platform`, easier
/// to type in a shell).
pub fn resolve_profile(workspace: &Workspace, text: &str) -> Result<Profile> {
    let normalized = text.replacen('/', "|", 1);
    match workspace.solution.find_profile(&normalized) {
        Some(profile) => Ok(profile.clone()),
        None => {
            let known: Vec<String> = workspace.solution.profiles.iter().map(|p| p.to_string()).collect();
            bail!("{text} is not a configuration of {}; it has: {}", workspace.solution.file_name(), known.join(", "))
        }
    }
}

fn print_json(value: &serde_json::Value) {
    say!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
}

fn symbol(level: Level) -> String {
    match level {
        Level::Ok => style::green("✔"),
        Level::Info => style::dim("·"),
        Level::Warn => style::yellow("⚠"),
        Level::Fail => style::red("✘"),
    }
}

// --- init -------------------------------------------------------------------------------------

pub fn init(global: &Global, args: &InitArgs) -> Result<i32> {
    let mut workspace = open_workspace(global)?;
    let options = init::Options {
        mode: args.mode.map(|m| match m {
            ModeArg::RepoRoot => OutputMode::RepoRoot,
            ModeArg::External => OutputMode::External,
        }),
        ignore: args.ignore.map(|i| match i {
            IgnoreArg::Exclude => IgnoreMode::Exclude,
            IgnoreArg::Gitignore => IgnoreMode::Gitignore,
            IgnoreArg::None => IgnoreMode::None,
        }),
    };
    let yes = args.yes;
    let json = global.json;
    let mut confirm = |question: &str| -> bool {
        if yes {
            return true;
        }
        if json || !std::io::stdin().is_terminal() {
            return false;
        }
        let _ = write!(std::io::stdout(), "{question} [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes")
    };
    let report = init::run(&mut workspace, &options, &mut confirm)?;
    if global.json {
        print_json(&json!({
            "mode": report.mode.map(init::mode_name),
            "done": report.done,
            "suggestions": report.suggestions,
            "warnings": report.warnings,
        }));
        return Ok(0);
    }
    for line in &report.done {
        say!("{} {line}", style::green("✔"));
    }
    for line in &report.warnings {
        say!("{} {line}", style::yellow("⚠"));
    }
    for line in &report.suggestions {
        say!("{} {line}", style::dim("→"));
    }
    if global.quiet == 0 {
        say!("{}", style::dim("next: `lazymsbuild sync` (or rebuild the solution) fills the database"));
    }
    Ok(0)
}

// --- sync and build ---------------------------------------------------------------------------

struct Printer {
    quiet: u8,
    json: bool,
}

impl Printer {
    fn event(&self, event: Event) {
        if self.json {
            return;
        }
        match event {
            Event::Line(line) if self.quiet == 0 => say!("{line}"),
            Event::Diagnostic(d) if self.quiet == 1 => say!("{}", diagnostic_line(&d)),
            Event::Note(note) if self.quiet < 2 => say!("{}", style::dim(&format!("lazymsbuild: {note}"))),
            _ => {}
        }
    }
}

fn diagnostic_line(d: &Diagnostic) -> String {
    let text = d.to_msvc();
    match d.severity {
        Severity::Error => style::red(&text),
        Severity::Warning => style::yellow(&text),
    }
}

/// Runs an engine operation with Ctrl+C cancelling it and the events printed.
async fn run_operation<F, Fut>(global: &Global, operation: F) -> Result<Outcome>
where
    F: FnOnce(mpsc::UnboundedSender<Event>, CancellationToken) -> Fut,
    Fut: std::future::Future<Output = Result<Outcome>>,
{
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let on_ctrl_c = cancel.clone();
    let json = global.json;
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            if !json {
                eprintln!("{}", style::yellow("cancelling (the build's process tree is ended)..."));
            }
            on_ctrl_c.cancel();
        }
    });
    let printer = Printer { quiet: global.quiet, json: global.json };
    let printing = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            printer.event(event);
        }
    });
    let result = operation(tx, cancel).await;
    let _ = printing.await;
    result
}

fn outcome_json(workspace: &Workspace, outcome: &Outcome, started: &str) -> serde_json::Value {
    json!({
        "state": outcome.state,
        "exit_code": outcome.exit_code,
        "solution": workspace.solution.path,
        "profile": workspace.active_profile().ok().map(|p| p.to_string()),
        "started_at": started,
        "finished_at": time::now(),
        "errors": outcome.errors(),
        "warnings": outcome.warnings(),
        "diagnostics": outcome.diagnostics,
        "db": outcome.db,
        "published": outcome.publish_error.is_none() && outcome.db.is_some(),
        "publish_error": outcome.publish_error,
        "notes": outcome.notes,
        "log": outcome.log,
    })
}

fn print_summary(verb: &str, workspace: &Workspace, outcome: &Outcome, elapsed: std::time::Duration) {
    let profile = workspace.active_profile().map(|p| p.to_string()).unwrap_or_default();
    let state = match outcome.state {
        State::Succeeded => style::green(&format!("{verb} succeeded")),
        State::Cancelled => style::yellow(&format!("{verb} cancelled")),
        _ => style::red(&format!("{verb} failed")),
    };
    say!(
        "{state} in {:.1}s ({profile}): {} error(s), {} warning(s)",
        elapsed.as_secs_f64(),
        outcome.errors(),
        outcome.warnings()
    );
    if let Some(db) = &outcome.db {
        let stale = if db.stale_projects.is_empty() { "none".to_string() } else { db.stale_projects.join(", ") };
        say!(
            "database: {} entries ({:+}), coverage {}/{}, stale: {stale}",
            db.entries,
            db.delta,
            db.coverage[0],
            db.coverage[1]
        );
    }
    if let Some(error) = &outcome.publish_error {
        say!("{} {error}", style::yellow("⚠"));
    }
}

fn exit_code(outcome: &Outcome) -> i32 {
    if outcome.state == State::Succeeded && outcome.publish_error.is_none() { 0 } else { 1 }
}

pub async fn sync(global: &Global, args: &SyncArgs) -> Result<i32> {
    let mut workspace = open_workspace(global)?;
    if let Some(text) = &args.profile {
        let profile = resolve_profile(&workspace, text)?;
        workspace.set_active_profile(&profile)?;
    }
    let started = time::now();
    let clock = std::time::Instant::now();
    let force = args.force;
    let outcome = run_operation(global, |tx, cancel| {
        let workspace = &mut workspace;
        async move { engine::sync(workspace, force, &tx, cancel).await }
    })
    .await?;
    if global.json {
        print_json(&outcome_json(&workspace, &outcome, &started));
    } else {
        print_summary("Sync", &workspace, &outcome, clock.elapsed());
    }
    Ok(exit_code(&outcome))
}

pub async fn build(global: &Global, args: &BuildArgs) -> Result<i32> {
    let mut workspace = open_workspace(global)?;
    let profile = args.profile.as_deref().map(|text| resolve_profile(&workspace, text)).transpose()?;
    // Check the target before switching the active configuration: a refused command changes nothing.
    if let Some(name) = &args.target {
        let wanted = match &profile {
            Some(profile) => profile.clone(),
            None => workspace.active_profile()?,
        };
        let project = workspace.solution.find_project(name).with_context(|| {
            let names: Vec<&str> = workspace.solution.cpp_projects().map(|p| p.name.as_str()).collect();
            format!("no project {name} in {}; it has: {}", workspace.solution.file_name(), names.join(", "))
        })?;
        if !project.builds_in(&wanted) {
            bail!("{} is not built in {wanted} (the solution configuration does not include it)", project.name);
        }
    }
    if let Some(profile) = &profile {
        workspace.set_active_profile(profile)?;
    }
    let request = BuildRequest {
        target: args.target.clone(),
        verb: if args.rebuild {
            Verb::Rebuild
        } else if args.clean {
            Verb::Clean
        } else {
            Verb::Build
        },
        parallel: args.no_parallel.then_some(false),
        force: args.force,
    };
    let started = time::now();
    let clock = std::time::Instant::now();
    let outcome = run_operation(global, |tx, cancel| {
        let workspace = &mut workspace;
        let request = &request;
        async move { engine::build(workspace, request, &tx, cancel).await }
    })
    .await?;
    if global.json {
        print_json(&outcome_json(&workspace, &outcome, &started));
    } else {
        print_summary(&request.verb.to_string(), &workspace, &outcome, clock.elapsed());
    }
    Ok(exit_code(&outcome))
}

// --- check ------------------------------------------------------------------------------------

pub fn check(global: &Global, args: &ProfileArg) -> Result<i32> {
    let workspace = open_workspace(global)?;
    let profile = match &args.profile {
        Some(text) => resolve_profile(&workspace, text)?,
        None => workspace.active_profile()?,
    };
    let report = check::run(&workspace, &profile)?;
    let problems = report.problems();
    if global.json {
        let mut value = serde_json::to_value(&report)?;
        value["problems"] = json!(problems);
        value["ok"] = json!(problems.is_empty());
        print_json(&value);
        return Ok(if problems.is_empty() { 0 } else { 1 });
    }

    let ok = |good: bool| if good { style::green("✔") } else { style::red("✘") };
    let warn = |good: bool| if good { style::green("✔") } else { style::yellow("⚠") };
    let limit = if global.verbose > 0 { usize::MAX } else { 10 };
    let list = |items: &[String]| {
        for item in items.iter().take(limit) {
            say!("      {}", style::dim(item));
        }
        if items.len() > limit {
            say!("      {}", style::dim(&format!("... and {} more (-v shows all)", items.len() - limit)));
        }
    };

    say!("{} {}", style::bold(&report.profile), style::dim(&format!("-> {}", report.output)));
    if !report.database_exists {
        say!("  {} database      none yet: run `lazymsbuild sync` (or rebuild the solution)", style::red("✘"));
        return Ok(1);
    }
    let methods: Vec<String> = report.methods.iter().map(|(m, n)| format!("{m} {n}")).collect();
    let updated = report.updated_at.as_deref().map(time::clock).unwrap_or_else(|| "?".into());
    say!(
        "  {} database      {} entries ({}), updated {updated}",
        style::green("✔"),
        report.entries,
        methods.join(", ")
    );

    let c = &report.coverage;
    say!("  {} coverage      {}/{}", ok(c.covered == c.total), c.covered, c.total);
    if c.covered < c.total {
        say!("      {}", style::dim("sources without an entry (`lazymsbuild sync` adds them):"));
        list(&c.missing);
    }
    if report.stale.is_empty() {
        say!("  {} freshness     no stale project", style::green("✔"));
    } else {
        say!("  {} freshness     {} stale project(s) (sync, or build them):", style::red("✘"), report.stale.len());
        list(&report.stale.iter().map(|s| format!("{}: {}", s.name, s.reason)).collect::<Vec<_>>());
    }
    say!(
        "  {} orphans       {}",
        warn(report.orphans.is_empty()),
        if report.orphans.is_empty() {
            "none".into()
        } else {
            format!("{} (dropped by the next build of their project)", report.orphans.len())
        }
    );
    if global.verbose > 0 {
        list(&report.orphans);
    }
    say!(
        "  {} duplicates    {}",
        warn(report.duplicates.is_empty()),
        if report.duplicates.is_empty() {
            "none".into()
        } else {
            format!("{} source(s) in several projects (the last build wins)", report.duplicates.len())
        }
    );
    if !report.duplicates.is_empty() {
        list(&report.duplicates.iter().map(|d| format!("{}: {}", d.file, d.projects.join(", "))).collect::<Vec<_>>());
    }
    let sync_text = match report.sync {
        SyncState::InSync => "published copy matches".to_string(),
        SyncState::OutOfSync => format!("{} differs from the database (another tool wrote it?)", report.output),
        SyncState::NoOutput => format!("{} does not exist", report.output),
        SyncState::NoDatabase => "no database".to_string(),
    };
    say!("  {} sync          {sync_text}", ok(report.sync == SyncState::InSync));
    if report.other_databases.is_empty() {
        say!("  {} other DBs     none", style::green("✔"));
    } else {
        say!("  {} other DBs     clangd may read these too:", style::red("✘"));
        list(
            &report
                .other_databases
                .iter()
                .map(|d| format!("{}{}", d.path, if d.shadows { " (read instead of ours)" } else { "" }))
                .collect::<Vec<_>>(),
        );
    }
    let (good, text) = match &report.ignored {
        IgnoreState::Ignored => (true, "generated files are ignored".to_string()),
        IgnoreState::NotIgnored(list) => {
            (false, format!("git status shows {} (run `lazymsbuild init`)", list.join(", ")))
        }
        IgnoreState::NotGit => (true, "not a Git repository".to_string()),
        IgnoreState::GitUnavailable => (true, "git not available, not checked".to_string()),
        IgnoreState::NotApplicable => (true, "external mode".to_string()),
    };
    say!("  {} ignored       {text}", ok(good));
    if !report.unresolved.is_empty() {
        say!(
            "  {} unresolved    {} project(s) have source paths lazymsbuild cannot resolve (not counted):",
            style::dim("·"),
            report.unresolved.len()
        );
        list(&report.unresolved.iter().map(|(p, items)| format!("{p}: {}", items.join(", "))).collect::<Vec<_>>());
    }
    for (project, error) in &report.unreadable {
        say!("  {} {project}: {error}", style::red("✘"));
    }
    Ok(if problems.is_empty() { 0 } else { 1 })
}

// --- doctor -----------------------------------------------------------------------------------

pub fn doctor(global: &Global, args: &DoctorArgs) -> Result<i32> {
    let workspace = open_workspace(global)?;
    let report = doctor::run(&workspace, &doctor::Options { report: args.report, ..Default::default() });
    let code = if report.worst() == Level::Fail { 1 } else { 0 };
    if global.json {
        print_json(&serde_json::to_value(&report)?);
        return Ok(code);
    }
    if args.report {
        say!("lazymsbuild {} doctor --report (paths masked)", report.lazymsbuild);
        say!("os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    } else {
        say!("lazymsbuild {}", report.lazymsbuild);
    }
    let width = report.items.iter().map(|i| i.name.chars().count()).max().unwrap_or(10);
    for item in &report.items {
        say!("  {} {:width$}  {}", symbol(item.level), item.name, item.detail);
        if let Some(hint) = &item.hint {
            for line in hint.lines() {
                say!("    {:width$}  {}", "", style::dim(line));
            }
        }
    }
    Ok(code)
}

// --- path -------------------------------------------------------------------------------------

pub fn path(global: &Global, args: &PathArgs) -> Result<i32> {
    if args.config {
        let dir = config::user_config_dir().context("cannot find a configuration directory")?;
        print_path_value(global, "config", &dir);
        return Ok(0);
    }
    let workspace = open_workspace(global)?;
    if args.db {
        print_path_value(global, "db", &workspace.output_dir());
    } else if args.state {
        print_path_value(global, "state", &workspace.state_dir);
    } else if global.json {
        print_json(&json!({
            "solution": workspace.solution.path,
            "root": workspace.root,
            "mode": init::mode_name(workspace.output_mode()),
            "profile": workspace.active_profile().ok().map(|p| p.to_string()),
            "db": workspace.output_dir(),
            "db_file": workspace.output_path(),
            "state": workspace.state_dir,
            "status": workspace.status_path(),
            "errors": workspace.errors_path(),
            "config": config::user_config_dir(),
        }));
    } else {
        say!("solution  {}", workspace.solution.path.display());
        say!("profile   {}", workspace.active_profile().map(|p| p.to_string()).unwrap_or_default());
        say!("mode      {}", init::mode_name(workspace.output_mode()));
        say!("db        {}", workspace.output_path().display());
        say!("state     {}", workspace.state_dir.display());
        if let Some(dir) = config::user_config_dir() {
            say!("config    {}", dir.join(config::USER_FILE).display());
        }
    }
    Ok(0)
}

fn print_path_value(global: &Global, key: &str, path: &std::path::Path) {
    if global.json {
        print_json(&json!({ key: path }));
    } else {
        say!("{}", path.display());
    }
}

// --- completions ------------------------------------------------------------------------------

pub fn completions(args: &CompletionsArgs) -> Result<i32> {
    use clap::CommandFactory;
    let mut command = crate::cli::Cli::command();
    clap_complete::generate(args.shell, &mut command, "lazymsbuild", &mut std::io::stdout());
    Ok(0)
}
