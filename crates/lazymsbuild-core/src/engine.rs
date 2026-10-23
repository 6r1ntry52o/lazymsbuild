//! The operations the command line and the TUI share: building, syncing, and switching the
//! active configuration.
//!
//! A build or sync holds `build.lock` for its whole run, keeps `status.json` current (every second
//! and at every state change), writes its output to `logs\<stamp>.log`, and at the end updates the
//! configuration's database, publishes it, and writes `errors.txt`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::check::{self, Report};
use crate::diag::{self, Collector, Diagnostic};
use crate::extract::build_log::{self, BuildLogger};
use crate::extract::design_time;
use crate::extract::{CommandExtractor, Extraction};
use crate::lock::BuildLock;
use crate::msbuild::invoke::{self, BuildArgs, Verb};
use crate::msbuild::{locate, process};
use crate::solution::Profile;
use crate::status::{self, DbStatus, State, Status};
use crate::store::{self, ApplyReport, LastBuild, ProfileDb, PublishError};
use crate::text::CodePage;
use crate::workspace::Workspace;
use crate::{cmdline, time};

/// What a running operation reports.
#[derive(Clone, Debug)]
pub enum Event {
    /// A line of the tool's output.
    Line(String),
    /// An error or warning, as soon as it is seen.
    Diagnostic(Diagnostic),
    /// A message from lazymsbuild itself.
    Note(String),
    /// The status as written to `status.json`.
    Status(Box<Status>),
}

/// How an operation ended.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub state: State,
    pub exit_code: Option<i32>,
    pub diagnostics: Vec<Diagnostic>,
    pub report: Option<ApplyReport>,
    pub check: Option<Report>,
    pub db: Option<DbStatus>,
    /// Why the database was not published, when it was not.
    pub publish_error: Option<String>,
    /// Whether the publish failed only because the output was not written by lazymsbuild.
    pub publish_needs_force: bool,
    pub notes: Vec<String>,
    pub log: Option<PathBuf>,
}

impl Outcome {
    pub fn errors(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.severity == diag::Severity::Error).count()
    }

    pub fn warnings(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.severity == diag::Severity::Warning).count()
    }
}

/// Another build or sync holds the lock.
#[derive(Debug, thiserror::Error)]
#[error("another build is running: {0}")]
pub struct Busy(pub String);

/// The database cannot be filled without building.
#[derive(Debug, thiserror::Error)]
#[error(
    "the design-time extractor (msbuild-extractor-sample.exe) was not found (searched: {}); install it with scripts/install-extractor.ps1, or fill the database by rebuilding the solution",
    .0.join(", ")
)]
pub struct SyncUnavailable(pub Vec<String>);

/// A build to run.
#[derive(Clone, Debug)]
pub struct BuildRequest {
    /// The project's solution target name; `None` for the whole solution.
    pub target: Option<String>,
    pub verb: Verb,
    /// `-m`; `None` uses the setting.
    pub parallel: Option<bool>,
    /// Overwrite an output file lazymsbuild did not write.
    pub force: bool,
}

fn describe_running(workspace: &Workspace) -> String {
    match status::read(&workspace.status_path()) {
        Some(s) if s.state.is_running() => format!(
            "{} {} of {} (started {}, pid {})",
            s.verb.as_deref().unwrap_or("build"),
            s.target.as_deref().unwrap_or("the solution"),
            s.profile.as_deref().unwrap_or("?"),
            s.started_at.as_deref().map(time::clock).unwrap_or_else(|| "?".into()),
            s.pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into())
        ),
        _ => format!("{} is locked", workspace.lock_path().display()),
    }
}

fn acquire(workspace: &Workspace) -> Result<BuildLock> {
    match BuildLock::try_acquire(&workspace.lock_path())? {
        Some(lock) => Ok(lock),
        None => Err(Busy(describe_running(workspace)).into()),
    }
}

fn code_page(workspace: &Workspace) -> CodePage {
    match workspace.config.build.code_page {
        0 => CodePage::Oem,
        n => CodePage::Number(n),
    }
}

/// Streams a process's lines into the log and the event channel while keeping `status.json`
/// current, until the process ends.
struct Run<'a> {
    status_path: PathBuf,
    events: &'a mpsc::UnboundedSender<Event>,
    status: Status,
    log: std::io::BufWriter<std::fs::File>,
    collector: Collector,
    tail: Vec<String>,
}

impl<'a> Run<'a> {
    fn start(
        workspace: &Workspace,
        events: &'a mpsc::UnboundedSender<Event>,
        status: Status,
        log_path: &Path,
    ) -> Result<Run<'a>> {
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log = std::fs::File::create(log_path).with_context(|| format!("cannot create {}", log_path.display()))?;
        let mut run = Run {
            status_path: workspace.status_path(),
            events,
            status,
            log: std::io::BufWriter::new(log),
            collector: Collector::default(),
            tail: Vec::new(),
        };
        run.write_status()?;
        Ok(run)
    }

    fn write_status(&mut self) -> Result<()> {
        self.status.errors = self.collector.errors() as u32;
        self.status.warnings = self.collector.warnings() as u32;
        status::write(&self.status_path, &self.status)?;
        let _ = self.events.send(Event::Status(Box::new(self.status.clone())));
        Ok(())
    }

    fn line(&mut self, line: String) {
        let _ = writeln!(self.log, "{line}");
        if let Some(d) = diag::parse_line(&line)
            && self.collector.add(d.clone())
        {
            let _ = self.events.send(Event::Diagnostic(d));
        }
        if self.tail.len() == 40 {
            self.tail.remove(0);
        }
        self.tail.push(line.clone());
        let _ = self.events.send(Event::Line(line));
    }

    fn note(&mut self, note: impl Into<String>) {
        let note = note.into();
        let _ = writeln!(self.log, "lazymsbuild: {note}");
        let _ = self.events.send(Event::Note(note));
    }

    async fn process(&mut self, spec: process::Spec, cancel: CancellationToken) -> Result<process::Exit> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runner = tokio::spawn(process::run(spec, tx, cancel));
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.tick().await;
        loop {
            tokio::select! {
                line = rx.recv() => match line {
                    Some(line) => self.line(line),
                    None => break,
                },
                _ = ticker.tick() => {
                    let _ = self.log.flush();
                    let _ = self.write_status();
                }
            }
        }
        let _ = self.log.flush();
        runner.await.context("the process runner panicked")?
    }
}

fn header(program: &Path, args: &[String]) -> String {
    format!("> {}", cmdline::join_args(std::iter::once(program.display().to_string()).chain(args.iter().cloned())))
}

/// Builds, then updates the active configuration's database from what the build compiled.
pub async fn build(
    workspace: &mut Workspace,
    request: &BuildRequest,
    events: &mpsc::UnboundedSender<Event>,
    cancel: CancellationToken,
) -> Result<Outcome> {
    let profile = workspace.active_profile()?;
    if let Some(name) = &request.target {
        let project =
            workspace.solution.find_project(name).with_context(|| format!("no project {name} in the solution"))?;
        if !project.builds_in(&profile) {
            bail!("{} is not built in {profile} (the solution configuration does not include it)", project.name);
        }
    }
    let _lock = acquire(workspace)?;
    workspace.remember_solution()?;
    let started_at = time::now();
    let stamp = time::file_stamp();
    let log_path = workspace.logs_dir().join(format!("{stamp}.log"));
    let target_name =
        request.target.as_deref().and_then(|n| workspace.solution.find_project(n)).map(|p| p.target_name.clone());

    let status = Status {
        state: State::Building,
        profile: Some(profile.to_string()),
        target: target_name.clone(),
        verb: Some(request.verb.to_string()),
        solution: Some(workspace.solution.path.display().to_string()),
        started_at: Some(started_at.clone()),
        pid: Some(std::process::id()),
        log: Some(log_path.display().to_string()),
        db: status::read(&workspace.status_path()).and_then(|s| s.db),
        ..Default::default()
    };
    let mut run = Run::start(workspace, events, status, &log_path)?;

    let msbuild = locate::locate(workspace.config.build.msbuild.as_deref(), workspace.solution.visual_studio_major())?;
    let logger = match build_log::find_logger(workspace.config.extractor.logger.as_deref()) {
        build_log::Lookup::Found(dll) => Some(BuildLogger { dll }),
        build_log::Lookup::Disabled => None,
        build_log::Lookup::Missing(tried) => {
            run.note(format!(
                "the build logger ({}) was not found, so this build does not update the database (tried {})",
                build_log::LOGGER_DLL,
                tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
            ));
            None
        }
    };
    std::fs::create_dir_all(workspace.tmp_dir())?;
    let events_path = workspace.tmp_dir().join(format!("events-{stamp}.jsonl"));
    let binlog = workspace.config.build.keep_binlog.then(|| workspace.logs_dir().join(format!("{stamp}.binlog")));
    let args = invoke::build_args(&BuildArgs {
        solution: &workspace.solution.path,
        target: target_name.as_deref(),
        verb: request.verb,
        profile: &profile,
        parallel: request.parallel.unwrap_or(workspace.config.build.parallel),
        max_cpu_count: workspace.config.build.max_cpu_count,
        verbosity: &workspace.config.build.verbosity,
        binlog: binlog.as_deref(),
        extractor_args: logger.as_ref().map(|l| l.msbuild_args(&events_path)).unwrap_or_default(),
        extra_args: &workspace.config.build.extra_args,
    })?;
    run.line(header(&msbuild.exe, &args));
    let spec = process::Spec {
        program: msbuild.exe.clone(),
        args,
        cwd: workspace.solution.dir().to_path_buf(),
        env: invoke::environment(workspace.config.build.english),
        code_page: code_page(workspace),
    };
    let exit = run.process(spec, cancel).await?;
    let state = if exit.cancelled {
        State::Cancelled
    } else if exit.code == Some(0) {
        State::Succeeded
    } else {
        State::Failed
    };

    let mut notes = Vec::new();
    let mut extraction: Option<Extraction> = None;
    let mut logger_succeeded = None;
    if logger.is_some() {
        match std::fs::read(&events_path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| build_log::parse_events(&crate::text::decode(&bytes)))
        {
            Ok(parsed) => {
                logger_succeeded = parsed.succeeded;
                let x = parsed.extraction();
                notes.extend(x.warnings.iter().cloned());
                extraction = Some(x);
            }
            Err(err) => notes.push(format!("could not read the build logger's output: {err:#}")),
        }
        // The newest few are kept for bug reports.
        prune(&workspace.tmp_dir(), "events-", 3);
    }

    // Diagnostics: the logger's when it has any (structured, nothing parsed from translated text),
    // otherwise those parsed from the console output (no logger, or one that reports none).
    let diagnostics = match &extraction {
        Some(x) if !x.diagnostics.is_empty() => {
            let mut collector = Collector::default();
            for d in &x.diagnostics {
                collector.add(d.clone());
            }
            collector.list
        }
        _ => run.collector.list.clone(),
    };
    run.collector = Collector::default();
    for d in &diagnostics {
        run.collector.add(d.clone());
    }
    diag::write_errors_file(&workspace.errors_path(), &diagnostics)?;

    let mut outcome = Outcome {
        state,
        exit_code: exit.code,
        diagnostics,
        report: None,
        check: None,
        db: None,
        publish_error: None,
        publish_needs_force: false,
        notes,
        log: Some(log_path.clone()),
    };

    if request.verb != Verb::Clean
        && let Some(x) = &extraction
    {
        let replace = state == State::Succeeded && logger_succeeded == Some(true);
        let finished = time::now();
        let last_build = LastBuild {
            verb: request.verb.to_string(),
            target: target_name.clone(),
            result: state_name(state).into(),
            started_at: started_at.clone(),
            finished_at: finished,
            errors: run.collector.errors() as u32,
            warnings: run.collector.warnings() as u32,
        };
        apply_and_publish(workspace, &profile, x, replace, &started_at, Some(last_build), request.force, &mut outcome)?;
    }
    for note in outcome.notes.clone() {
        run.note(note);
    }
    run.status.db = outcome.db.clone().or(run.status.db.take());
    run.status.state = state;
    run.status.finished_at = Some(time::now());
    run.write_status()?;
    prune(&workspace.logs_dir(), "", workspace.config.logs.keep.max(1) * if binlog.is_some() { 2 } else { 1 });
    Ok(outcome)
}

fn state_name(state: State) -> &'static str {
    match state {
        State::Succeeded => "succeeded",
        State::Cancelled => "cancelled",
        _ => "failed",
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_and_publish(
    workspace: &mut Workspace,
    profile: &Profile,
    extraction: &Extraction,
    replace: bool,
    captured_at: &str,
    last_build: Option<LastBuild>,
    force: bool,
    outcome: &mut Outcome,
) -> Result<()> {
    let (projects, unreadable) = check::load_projects(workspace);
    let mut db = ProfileDb::load(workspace, profile)?;
    let report = db.apply(extraction, &projects, replace, captured_at);
    if last_build.is_some() {
        db.meta.last_build = last_build;
    }
    db.save()?;
    outcome.notes.push(format!(
        "database {profile}: {} entries ({:+}; {} added, {} updated, {} removed)",
        report.after,
        report.delta(),
        report.added,
        report.updated,
        report.removed
    ));
    match store::publish(workspace, &db, force) {
        Ok(path) => outcome.notes.push(format!("published {}", workspace.display_path(&path))),
        Err(err) => {
            outcome.publish_needs_force = matches!(err.downcast_ref::<PublishError>(), Some(PublishError::Foreign(_)));
            outcome.publish_error = Some(format!("{err:#}"));
            outcome.notes.push(format!("not published: {err:#}"));
        }
    }
    let check = check::run_with(workspace, profile, &db, &projects, unreadable)?;
    outcome.db = Some(db_status(workspace, &check, report.delta()));
    outcome.report = Some(report);
    outcome.check = Some(check);
    Ok(())
}

/// The `db` part of `status.json` from a check.
pub fn db_status(workspace: &Workspace, check: &Report, delta: i64) -> DbStatus {
    DbStatus {
        path: workspace.output_path().display().to_string(),
        updated_at: check.updated_at.clone(),
        entries: check.entries,
        delta,
        coverage: [check.coverage.covered, check.coverage.total],
        stale_projects: check.stale.iter().map(|s| s.name.clone()).collect(),
    }
}

/// Rebuilds the active configuration's database with the design-time extractor (method A).
pub async fn sync(
    workspace: &mut Workspace,
    force: bool,
    events: &mpsc::UnboundedSender<Event>,
    cancel: CancellationToken,
) -> Result<Outcome> {
    let profile = workspace.active_profile()?;
    let exe = match design_time::find(workspace.config.extractor.design_time.as_deref(), &workspace.root) {
        design_time::Lookup::Found { exe, .. } => exe,
        design_time::Lookup::Disabled => bail!("sync is turned off (extractor.design_time = \"off\")"),
        design_time::Lookup::Missing(searched) => return Err(SyncUnavailable(searched).into()),
    };
    let _lock = acquire(workspace)?;
    workspace.remember_solution()?;
    let started_at = time::now();
    let stamp = time::file_stamp();
    let log_path = workspace.logs_dir().join(format!("{stamp}.log"));
    let status = Status {
        state: State::Syncing,
        profile: Some(profile.to_string()),
        verb: Some("Sync".into()),
        solution: Some(workspace.solution.path.display().to_string()),
        started_at: Some(started_at.clone()),
        pid: Some(std::process::id()),
        log: Some(log_path.display().to_string()),
        db: status::read(&workspace.status_path()).and_then(|s| s.db),
        ..Default::default()
    };
    let mut run = Run::start(workspace, events, status, &log_path)?;

    std::fs::create_dir_all(workspace.tmp_dir())?;
    let output = workspace.tmp_dir().join(format!("design-time-{stamp}.json"));
    let config_file = design_time::ConfigFile::find(workspace.solution.dir(), &workspace.root);
    let plan = design_time::plan(&workspace.solution.path, &profile, config_file.as_ref(), &output);
    for note in &plan.notes {
        run.note(note.clone());
    }
    run.line(header(&exe, &plan.args));
    // Not the solution's directory: the extractor would load a msbuild-extractor.json from there
    // by itself, even when lazymsbuild decided not to use it.
    let spec = process::Spec {
        program: exe,
        args: plan.args,
        cwd: workspace.tmp_dir(),
        env: Vec::new(),
        code_page: code_page(workspace),
    };
    let exit = run.process(spec, cancel).await?;

    let mut outcome = Outcome {
        state: State::Succeeded,
        exit_code: exit.code,
        diagnostics: Vec::new(),
        report: None,
        check: None,
        db: None,
        publish_error: None,
        publish_needs_force: false,
        notes: Vec::new(),
        log: Some(log_path.clone()),
    };
    let result = if exit.cancelled {
        outcome.state = State::Cancelled;
        Ok(())
    } else if !exit.success() || !output.is_file() {
        outcome.state = State::Failed;
        let explanation = design_time::explain_failure(&run.tail.join("\n"));
        outcome.notes.push(match explanation {
            Some(text) => text,
            None => format!("the design-time extractor failed (exit code {:?}); see {}", exit.code, log_path.display()),
        });
        Ok(())
    } else {
        match design_time::read_output(&output) {
            Ok(entries) => {
                let (projects, _) = check::load_projects(workspace);
                let x = design_time::extraction(entries, &workspace.solution, &projects);
                apply_and_publish(workspace, &profile, &x, true, &started_at, None, force, &mut outcome)
            }
            Err(err) => {
                outcome.state = State::Failed;
                outcome.notes.push(format!("{err:#}"));
                Ok(())
            }
        }
    };
    let _ = std::fs::remove_file(&output);
    for note in outcome.notes.clone() {
        run.note(note);
    }
    run.status.db = outcome.db.clone().or(run.status.db.take());
    run.status.state = outcome.state;
    run.status.finished_at = Some(time::now());
    run.write_status()?;
    prune(&workspace.logs_dir(), "", workspace.config.logs.keep.max(1));
    result?;
    Ok(outcome)
}

/// The result of switching the active configuration.
#[derive(Clone, Debug)]
pub enum Switched {
    /// Its database was published.
    Published(PathBuf),
    /// It has no database yet: `sync` (when available) or a rebuild fills it.
    NoDatabase { sync_available: bool },
    /// Publishing was refused (see [`PublishError`]); the configuration is active anyway.
    NotPublished { error: String, needs_force: bool },
}

/// Makes `profile` active and publishes its database.
pub fn switch(workspace: &mut Workspace, profile: &Profile, force: bool) -> Result<Switched> {
    workspace.set_active_profile(profile)?;
    let db = ProfileDb::load(workspace, profile)?;
    if !db.exists {
        let sync_available = matches!(
            design_time::find(workspace.config.extractor.design_time.as_deref(), &workspace.root),
            design_time::Lookup::Found { .. }
        );
        return Ok(Switched::NoDatabase { sync_available });
    }
    match store::publish(workspace, &db, force) {
        Ok(path) => Ok(Switched::Published(path)),
        Err(err) => Ok(Switched::NotPublished {
            needs_force: matches!(err.downcast_ref::<PublishError>(), Some(PublishError::Foreign(_))),
            error: format!("{err:#}"),
        }),
    }
}

/// Publishes the active configuration's database again (after the user agreed to overwrite).
pub fn publish_active(workspace: &mut Workspace, force: bool) -> Result<PathBuf> {
    let profile = workspace.active_profile()?;
    let db = ProfileDb::load(workspace, &profile)?;
    if !db.exists {
        bail!("no database for {profile} yet");
    }
    store::publish(workspace, &db, force)
}

/// Deletes all but the newest `keep` files whose name starts with `prefix`.
fn prune(dir: &Path, prefix: &str, keep: usize) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix) && e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}
