//! The TUI's state and what keys and finished tasks do to it. Nothing here blocks: builds, syncs,
//! checks and configuration switches run as tasks and report back through [`Done`].

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lazymsbuild_core::check::{self, Report};
use lazymsbuild_core::config::OutputMode;
use lazymsbuild_core::diag::{Diagnostic, Severity};
use lazymsbuild_core::engine::{self, BuildRequest, Busy, Event, Outcome, Switched};
use lazymsbuild_core::msbuild::Verb;
use lazymsbuild_core::solution::Profile;
use lazymsbuild_core::status::State;
use lazymsbuild_core::workspace::{LocalState, Workspace};
use lazymsbuild_core::{clangd, init, time};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::keys::{Action, Keymap};
use super::tree::{self, Row, RowKind};

/// Lines kept in the log pane (the log file has everything).
const LOG_LIMIT: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Targets,
    Configs,
    Actions,
    Log,
    Db,
}

/// The configurations as a table: configurations down, platforms across, each in the order the
/// solution first names it. A cell the solution does not have stays empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigGrid {
    pub configurations: Vec<String>,
    pub platforms: Vec<String>,
    /// The cells the solution has, in the order the cursor visits them: down a platform's column,
    /// then on to the next, so that `j` and `k` alone reach every one.
    pub cells: Vec<(usize, usize)>,
}

impl ConfigGrid {
    pub fn new(profiles: &[Profile]) -> ConfigGrid {
        let mut grid = ConfigGrid::default();
        for profile in profiles {
            if !grid.configurations.contains(&profile.configuration) {
                grid.configurations.push(profile.configuration.clone());
            }
            if !grid.platforms.contains(&profile.platform) {
                grid.platforms.push(profile.platform.clone());
            }
        }
        grid.cells = profiles.iter().filter_map(|p| grid.position(p)).collect();
        grid.cells.sort_by_key(|&(row, column)| (column, row));
        grid.cells.dedup();
        grid
    }

    pub fn profile(&self, (row, column): (usize, usize)) -> Profile {
        Profile::new(
            self.configurations.get(row).cloned().unwrap_or_default(),
            self.platforms.get(column).cloned().unwrap_or_default(),
        )
    }

    pub fn position(&self, profile: &Profile) -> Option<(usize, usize)> {
        let row = self.configurations.iter().position(|c| *c == profile.configuration)?;
        let column = self.platforms.iter().position(|p| *p == profile.platform)?;
        Some((row, column))
    }
}

impl Pane {
    const ORDER: [Pane; 5] = [Pane::Targets, Pane::Configs, Pane::Actions, Pane::Log, Pane::Db];

    fn next(self, step: isize) -> Pane {
        let i = Pane::ORDER.iter().position(|p| *p == self).unwrap_or(0) as isize;
        Pane::ORDER[(i + step).rem_euclid(Pane::ORDER.len() as isize) as usize]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogTab {
    Log,
    Errors,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Normal,
    Command,
    Note,
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct LogLine {
    pub text: String,
    pub kind: LineKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    Build(Verb),
    Sync,
}

/// The build or sync in progress.
#[derive(Debug)]
pub struct Running {
    /// `Build App`, `Rebuild the solution`, `Sync`.
    pub label: String,
    pub profile: Profile,
    pub started: Instant,
    pub cancel: CancellationToken,
    pub cancelling: bool,
    pub errors: u32,
    pub warnings: u32,
}

/// What a task reports when it ends.
#[allow(clippy::large_enum_variant, reason = "one message per task, never stored in bulk")]
pub enum Done {
    Operation { kind: RunKind, result: Result<Outcome>, local: LocalState, elapsed: Duration },
    Check { profile: Profile, result: Result<Report> },
    Switched { profile: Profile, result: Result<Switched>, local: LocalState },
    Published { result: Result<PathBuf>, local: LocalState },
    Init { result: Result<init::Report>, local: LocalState },
}

/// Actions that ask first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmed {
    Clean,
    CancelBuild,
    Overwrite,
    Sync,
    Init,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Build,
    Rebuild,
    Clean,
    Sync,
    Parallel,
    Refresh,
    OpenLog,
    Init,
    Help,
    Quit,
}

impl MenuItem {
    pub const ALL: [MenuItem; 10] = [
        MenuItem::Build,
        MenuItem::Rebuild,
        MenuItem::Clean,
        MenuItem::Sync,
        MenuItem::Parallel,
        MenuItem::Refresh,
        MenuItem::OpenLog,
        MenuItem::Init,
        MenuItem::Help,
        MenuItem::Quit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MenuItem::Build => "Build",
            MenuItem::Rebuild => "Rebuild",
            MenuItem::Clean => "Clean",
            MenuItem::Sync => "Sync (fill the database without building)",
            MenuItem::Parallel => "Parallel builds (-m) on/off",
            MenuItem::Refresh => "Refresh the database status",
            MenuItem::OpenLog => "Open the last build log",
            MenuItem::Init => "Init (.clangd, keep generated files out of Git)",
            MenuItem::Help => "Keys",
            MenuItem::Quit => "Quit",
        }
    }

    pub fn action(self) -> Option<Action> {
        match self {
            MenuItem::Build => Some(Action::Build),
            MenuItem::Rebuild => Some(Action::Rebuild),
            MenuItem::Clean => Some(Action::Clean),
            MenuItem::Sync => Some(Action::Sync),
            MenuItem::Parallel => Some(Action::Parallel),
            MenuItem::Help => Some(Action::Help),
            MenuItem::Quit => Some(Action::Quit),
            MenuItem::Refresh | MenuItem::OpenLog | MenuItem::Init => None,
        }
    }
}

/// The entries of the Actions pane.
pub const ACTIONS: [Action; 5] = [Action::Build, Action::Rebuild, Action::Clean, Action::Sync, Action::Parallel];

pub enum Popup {
    Help {
        scroll: usize,
    },
    Menu {
        cursor: usize,
    },
    Confirm {
        message: String,
        action: Confirmed,
    },
    /// Quitting during a build: cancel it and quit, or keep waiting.
    Quit {
        cursor: usize,
    },
    Message {
        title: String,
        lines: Vec<String>,
    },
}

/// Where the editor should open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorRequest {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
}

pub struct Notice {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct App {
    pub ws: Workspace,
    pub keymap: Keymap,
    pub focus: Pane,
    pub rows: Vec<Row>,
    pub tree_cursor: usize,
    /// The selected project (an index into the solution's projects); `None` is the solution.
    pub target: Option<usize>,
    pub filter: String,
    pub filtering: bool,
    /// The configurations at least one C++ project builds in.
    pub profiles: Vec<Profile>,
    pub grid: ConfigGrid,
    /// Row (configuration) and column (platform) in [`App::grid`].
    pub config_cursor: (usize, usize),
    /// The configurations that have a database.
    pub with_db: Vec<Profile>,
    pub active: Profile,
    pub parallel: bool,
    pub action_cursor: usize,
    pub log: VecDeque<LogLine>,
    /// Lines scrolled up from the end; 0 follows the output.
    pub log_scroll: usize,
    /// Width of the log pane at the last draw, for counting wrapped rows.
    pub log_width: usize,
    pub diagnostics: Vec<Diagnostic>,
    pub diag_cursor: usize,
    pub tab: LogTab,
    pub check: Option<Report>,
    pub checking: bool,
    pub delta: Option<i64>,
    pub running: Option<Running>,
    pub popup: Option<Popup>,
    pub notice: Option<Notice>,
    pub quit: bool,
    quit_when_done: bool,
    pub editor_request: Option<EditorRequest>,
    pub last_log: Option<PathBuf>,
    /// Lines a page moves (set from the pane height while drawing).
    pub page: usize,
    events: mpsc::UnboundedSender<Event>,
    done: mpsc::UnboundedSender<Done>,
}

impl App {
    pub fn new(ws: Workspace, events: mpsc::UnboundedSender<Event>, done: mpsc::UnboundedSender<Done>) -> Result<App> {
        let (keymap, key_warnings) = Keymap::new(&ws.config.keys);
        let active = ws.active_profile()?;
        // Any CPU and the like, where only .NET projects build, mean nothing here.
        let mut profiles: Vec<Profile> = ws
            .solution
            .profiles
            .iter()
            .filter(|p| ws.solution.cpp_projects().any(|project| project.builds_in(p)))
            .cloned()
            .collect();
        if profiles.is_empty() {
            profiles = ws.solution.profiles.clone();
        }
        if !profiles.contains(&active) {
            profiles.push(active.clone());
        }
        let grid = ConfigGrid::new(&profiles);
        let config_cursor = grid.position(&active).unwrap_or((0, 0));
        let rows = tree::rows(&ws.solution, None);
        let parallel = ws.config.build.parallel;
        let mut app = App {
            keymap,
            focus: Pane::Targets,
            rows,
            tree_cursor: 0,
            target: None,
            filter: String::new(),
            filtering: false,
            profiles,
            grid,
            config_cursor,
            with_db: Vec::new(),
            active,
            parallel,
            action_cursor: 0,
            log: VecDeque::new(),
            log_scroll: 0,
            log_width: 80,
            diagnostics: Vec::new(),
            diag_cursor: 0,
            tab: LogTab::Log,
            check: None,
            checking: false,
            delta: None,
            running: None,
            popup: None,
            notice: None,
            quit: false,
            quit_when_done: false,
            editor_request: None,
            last_log: None,
            page: 10,
            events,
            done,
            ws,
        };
        app.refresh_databases();
        let cpp = app.ws.solution.cpp_projects().count();
        app.note(format!(
            "lazymsbuild {} · {} ({cpp} C++ projects)",
            env!("CARGO_PKG_VERSION"),
            app.ws.display_path(&app.ws.solution.path)
        ));
        for warning in app.ws.warnings.clone().into_iter().chain(key_warnings) {
            app.push_line(warning, LineKind::Warning);
        }
        app.refresh_check();
        app.offer_init();
        Ok(app)
    }

    // --- log ----------------------------------------------------------------------------------

    pub fn push_line(&mut self, text: String, kind: LineKind) {
        if self.log.len() == LOG_LIMIT {
            self.log.pop_front();
        }
        let text = if text.contains('\t') { text.replace('\t', "    ") } else { text };
        if self.log_scroll > 0 {
            // Keep the view where it is while new lines arrive: scrolling counts screen rows.
            self.log_scroll += super::view::wrap(&text, self.log_width).len();
        }
        self.log.push_back(LogLine { text, kind });
    }

    fn note(&mut self, text: String) {
        self.push_line(text, LineKind::Note);
    }

    pub fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.notice = Some(Notice { text: text.into(), error, at: Instant::now() });
    }

    pub fn errors(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.severity == Severity::Error).count()
    }

    pub fn warnings(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.severity == Severity::Warning).count()
    }

    // --- events from the running operation ----------------------------------------------------

    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::Line(line) => {
                let kind = if line.starts_with("> ") {
                    LineKind::Command
                } else if line.contains(": error ") || line.contains(": fatal error ") {
                    LineKind::Error
                } else if line.contains(": warning ") {
                    LineKind::Warning
                } else {
                    LineKind::Normal
                };
                self.push_line(line, kind);
            }
            Event::Note(note) => self.note(format!("lazymsbuild: {note}")),
            Event::Diagnostic(d) => self.diagnostics.push(d),
            Event::Status(status) => {
                if let Some(running) = &mut self.running {
                    running.errors = status.errors;
                    running.warnings = status.warnings;
                }
            }
        }
    }

    /// Which configurations have a database, for the configuration table.
    fn refresh_databases(&mut self) {
        self.with_db = self.profiles.iter().filter(|p| self.ws.profile_db_path(p).is_file()).cloned().collect();
    }

    pub fn on_done(&mut self, done: Done) {
        self.refresh_databases();
        match done {
            Done::Operation { kind, result, local, elapsed } => {
                self.running = None;
                self.ws.local = local;
                self.finish_operation(kind, result, elapsed);
                if self.quit_when_done {
                    self.quit = true;
                }
            }
            Done::Check { profile, result } => {
                self.checking = false;
                if profile != self.active {
                    return; // an older check
                }
                match result {
                    Ok(report) => self.check = Some(report),
                    Err(err) => self.notify(format!("cannot check the database: {err:#}"), true),
                }
            }
            Done::Switched { profile, result, local } => {
                self.ws.local = local;
                match result {
                    Ok(switched) => {
                        self.active = profile.clone();
                        self.delta = None;
                        match switched {
                            Switched::Published(path) => {
                                self.notify(format!("{profile}: published {}", self.ws.display_path(&path)), false)
                            }
                            Switched::NoDatabase { sync_available: true } => {
                                self.popup = Some(Popup::Confirm {
                                    message: format!("{profile} has no database yet. Sync it now (no build needed)?"),
                                    action: Confirmed::Sync,
                                })
                            }
                            Switched::NoDatabase { sync_available: false } => {
                                self.popup = Some(Popup::Message {
                                    title: format!("{profile} has no database yet"),
                                    lines: vec![
                                        "The design-time extractor is not installed, so sync is not available.".into(),
                                        format!(
                                            "Rebuild the solution ({}) to fill the database.",
                                            self.keymap.label(Action::Rebuild)
                                        ),
                                    ],
                                })
                            }
                            Switched::NotPublished { error, needs_force } => self.offer_overwrite(error, needs_force),
                        }
                        self.refresh_check();
                    }
                    Err(err) => self.notify(format!("cannot switch to {profile}: {err:#}"), true),
                }
            }
            Done::Published { result, local } => {
                self.ws.local = local;
                match result {
                    Ok(path) => self.notify(format!("published {}", self.ws.display_path(&path)), false),
                    Err(err) => self.notify(format!("{err:#}"), true),
                }
                self.refresh_check();
            }
            Done::Init { result, local } => {
                self.ws.local = local;
                match result {
                    Ok(report) => {
                        let mut lines: Vec<String> = report.done.iter().map(|l| format!("✔ {l}")).collect();
                        lines.extend(report.warnings.iter().map(|l| format!("⚠ {l}")));
                        lines.extend(
                            report.suggestions.iter().flat_map(|l| l.lines().map(str::to_string).collect::<Vec<_>>()),
                        );
                        self.popup = Some(Popup::Message { title: "init".into(), lines });
                    }
                    Err(err) => self.notify(format!("init failed: {err:#}"), true),
                }
                self.refresh_check();
            }
        }
    }

    fn finish_operation(&mut self, kind: RunKind, result: Result<Outcome>, elapsed: Duration) {
        let name = match kind {
            RunKind::Build(verb) => verb.to_string(),
            RunKind::Sync => "Sync".to_string(),
        };
        match result {
            Ok(outcome) => {
                // The final list comes from the logger: complete, and in the right encoding.
                self.diagnostics = outcome.diagnostics.clone();
                self.diag_cursor = self.diag_cursor.min(self.diagnostics.len().saturating_sub(1));
                if let Some(check) = outcome.check.clone() {
                    self.check = Some(check);
                }
                if let Some(report) = &outcome.report {
                    self.delta = Some(report.delta());
                }
                self.last_log = outcome.log.clone();
                let state = match outcome.state {
                    State::Succeeded => "succeeded",
                    State::Cancelled => "cancelled",
                    _ => "failed",
                };
                let text = format!(
                    "{name} {state} in {:.1}s: {} error(s), {} warning(s)",
                    elapsed.as_secs_f64(),
                    outcome.errors(),
                    outcome.warnings()
                );
                self.note(text.clone());
                self.notify(text, outcome.state != State::Succeeded);
                if outcome.state == State::Failed && self.errors() > 0 {
                    self.tab = LogTab::Errors;
                }
                if let Some(error) = outcome.publish_error {
                    self.offer_overwrite(error, outcome.publish_needs_force);
                }
            }
            Err(err) => {
                let busy = err.downcast_ref::<Busy>().is_some();
                let text = format!("{err:#}");
                self.push_line(text.clone(), LineKind::Error);
                self.popup = Some(Popup::Message {
                    title: if busy { "Another build is running".into() } else { format!("{name} could not run") },
                    lines: text.lines().map(str::to_string).collect(),
                });
            }
        }
    }

    fn offer_overwrite(&mut self, error: String, needs_force: bool) {
        if needs_force {
            self.popup = Some(Popup::Confirm {
                message: format!("{error}\n\nOverwrite it with lazymsbuild's database?"),
                action: Confirmed::Overwrite,
            });
        } else {
            self.popup = Some(Popup::Message {
                title: "Not published".into(),
                lines: error.lines().map(str::to_string).collect(),
            });
        }
    }

    /// On first start in a repository without `.clangd`, offer `init` once.
    fn offer_init(&mut self) {
        if self.ws.local.init_offered || self.ws.output_mode() != OutputMode::RepoRoot {
            return;
        }
        if self.ws.root.join(clangd::FILE_NAME).exists() {
            return;
        }
        self.ws.local.init_offered = true;
        let _ = self.ws.save_local_state();
        self.popup = Some(Popup::Confirm {
            message: "This repository is not set up yet. Run init? It writes .clangd and lists the generated files in .git/info/exclude (no tracked file changes).".into(),
            action: Confirmed::Init,
        });
    }

    // --- background tasks ---------------------------------------------------------------------

    pub fn refresh_check(&mut self) {
        self.checking = true;
        let ws = self.ws.clone();
        let profile = self.active.clone();
        let done = self.done.clone();
        tokio::task::spawn_blocking(move || {
            let result = check::run(&ws, &profile);
            let _ = done.send(Done::Check { profile, result });
        });
    }

    fn busy(&mut self) -> bool {
        if let Some(running) = &self.running {
            let cancel = self.keymap.label(Action::Cancel);
            self.notify(format!("{} is running ({cancel} cancels it)", running.label), true);
            return true;
        }
        false
    }

    fn start_build(&mut self, verb: Verb) {
        if self.busy() {
            return;
        }
        let project = self.target.map(|i| self.ws.solution.projects[i].clone());
        if let Some(project) = &project
            && !project.builds_in(&self.active)
        {
            self.notify(format!("{} is not built in {}", project.name, self.active), true);
            return;
        }
        let label = match &project {
            Some(p) => format!("{verb} {}", p.name),
            None => format!("{verb} the solution"),
        };
        let request =
            BuildRequest { target: project.map(|p| p.target_name), verb, parallel: Some(self.parallel), force: false };
        self.log.clear();
        self.log_scroll = 0;
        self.diagnostics.clear();
        self.diag_cursor = 0;
        self.tab = LogTab::Log;
        let cancel = CancellationToken::new();
        let mut ws = self.ws.clone();
        let (events, done, token) = (self.events.clone(), self.done.clone(), cancel.clone());
        tokio::spawn(async move {
            let started = Instant::now();
            let result = engine::build(&mut ws, &request, &events, token).await;
            let _ = done.send(Done::Operation {
                kind: RunKind::Build(verb),
                result,
                local: ws.local,
                elapsed: started.elapsed(),
            });
        });
        self.running = Some(Running {
            label,
            profile: self.active.clone(),
            started: Instant::now(),
            cancel,
            cancelling: false,
            errors: 0,
            warnings: 0,
        });
    }

    fn start_sync(&mut self) {
        if self.busy() {
            return;
        }
        self.log.clear();
        self.log_scroll = 0;
        self.diagnostics.clear();
        self.tab = LogTab::Log;
        let cancel = CancellationToken::new();
        let mut ws = self.ws.clone();
        let (events, done, token) = (self.events.clone(), self.done.clone(), cancel.clone());
        tokio::spawn(async move {
            let started = Instant::now();
            let result = engine::sync(&mut ws, false, &events, token).await;
            let _ =
                done.send(Done::Operation { kind: RunKind::Sync, result, local: ws.local, elapsed: started.elapsed() });
        });
        self.running = Some(Running {
            label: "Sync".into(),
            profile: self.active.clone(),
            started: Instant::now(),
            cancel,
            cancelling: false,
            errors: 0,
            warnings: 0,
        });
    }

    fn switch_profile(&mut self, profile: Profile) {
        if !self.profiles.contains(&profile) {
            self.notify(format!("{profile} is not in the solution"), true);
            return;
        }
        if profile == self.active || self.busy() {
            return;
        }
        let mut ws = self.ws.clone();
        let done = self.done.clone();
        tokio::task::spawn_blocking(move || {
            let result = engine::switch(&mut ws, &profile, false);
            let _ = done.send(Done::Switched { profile, result, local: ws.local });
        });
    }

    fn publish_forced(&mut self) {
        let mut ws = self.ws.clone();
        let done = self.done.clone();
        tokio::task::spawn_blocking(move || {
            let result = engine::publish_active(&mut ws, true);
            let _ = done.send(Done::Published { result, local: ws.local });
        });
    }

    fn run_init(&mut self) {
        if self.busy() {
            return;
        }
        let mut ws = self.ws.clone();
        let done = self.done.clone();
        tokio::task::spawn_blocking(move || {
            let result = init::run(&mut ws, &init::Options::default(), &mut |_| false);
            let _ = done.send(Done::Init { result, local: ws.local });
        });
    }

    fn cancel_build(&mut self) {
        if let Some(running) = &mut self.running {
            running.cancelling = true;
            running.cancel.cancel();
            self.notify("cancelling: ending the build's process tree", false);
        }
    }

    // --- keys ---------------------------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.filtering {
            self.filter_key(key);
            return;
        }
        if self.popup.is_some() {
            self.popup_key(key);
            return;
        }
        if let Some(action) = self.keymap.action(&key) {
            self.act(action);
        }
    }

    fn filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filtering = false;
                self.filter.clear();
                self.rebuild_rows();
            }
            KeyCode::Enter => self.filtering = false,
            KeyCode::Backspace => {
                self.filter.pop();
                self.rebuild_rows();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(c);
                self.rebuild_rows();
            }
            _ => {}
        }
    }

    fn rebuild_rows(&mut self) {
        let current = self.rows.get(self.tree_cursor).map(|r| r.kind);
        self.rows = tree::rows(&self.ws.solution, Some(&self.filter));
        // Keep the cursor on the same row if it is still there, otherwise on the first project.
        self.tree_cursor = current
            .and_then(|kind| self.rows.iter().position(|r| r.kind == kind))
            .or_else(|| self.rows.iter().position(|r| matches!(r.kind, RowKind::Project(_))))
            .unwrap_or(0);
    }

    fn popup_key(&mut self, key: KeyEvent) {
        let action = self.keymap.action(&key);
        let Some(popup) = self.popup.take() else { return };
        match popup {
            Popup::Help { scroll } => match action {
                Some(Action::Down) => self.popup = Some(Popup::Help { scroll: scroll + 1 }),
                Some(Action::Up) => self.popup = Some(Popup::Help { scroll: scroll.saturating_sub(1) }),
                _ if is_close(&key) || matches!(action, Some(Action::Close | Action::Quit | Action::Help)) => {}
                _ => self.popup = Some(Popup::Help { scroll }),
            },
            Popup::Message { title, lines } => {
                if !(is_close(&key) || matches!(action, Some(Action::Close | Action::Quit | Action::Select))) {
                    self.popup = Some(Popup::Message { title, lines });
                }
            }
            Popup::Menu { cursor } => match action {
                Some(Action::Down) => self.popup = Some(Popup::Menu { cursor: (cursor + 1) % MenuItem::ALL.len() }),
                Some(Action::Up) => {
                    self.popup = Some(Popup::Menu { cursor: (cursor + MenuItem::ALL.len() - 1) % MenuItem::ALL.len() })
                }
                Some(Action::Select) => self.menu(MenuItem::ALL[cursor]),
                Some(Action::Close | Action::Menu | Action::Quit) => {}
                Some(other) => {
                    // A key of an action in the menu runs it, as in lazygit.
                    if let Some(item) = MenuItem::ALL.iter().find(|i| i.action() == Some(other)) {
                        self.menu(*item);
                    } else {
                        self.popup = Some(Popup::Menu { cursor });
                    }
                }
                None => self.popup = Some(Popup::Menu { cursor }),
            },
            Popup::Confirm { message, action: confirmed } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.confirmed(confirmed),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.popup = Some(Popup::Confirm { message, action: confirmed }),
            },
            Popup::Quit { cursor } => match action {
                Some(Action::Down | Action::Up) => self.popup = Some(Popup::Quit { cursor: 1 - cursor }),
                Some(Action::Select) => {
                    if cursor == 0 {
                        self.quit_when_done = true;
                        self.cancel_build();
                    }
                }
                Some(Action::Close) => {}
                _ if key.code == KeyCode::Esc => {}
                _ => self.popup = Some(Popup::Quit { cursor }),
            },
        }
    }

    fn confirmed(&mut self, action: Confirmed) {
        match action {
            Confirmed::Clean => self.start_build(Verb::Clean),
            Confirmed::CancelBuild => self.cancel_build(),
            Confirmed::Overwrite => self.publish_forced(),
            Confirmed::Sync => self.start_sync(),
            Confirmed::Init => self.run_init(),
        }
    }

    fn menu(&mut self, item: MenuItem) {
        match item {
            MenuItem::Refresh => {
                self.refresh_check();
                self.notify("refreshing the database status", false);
            }
            MenuItem::OpenLog => match self.last_log.clone().or_else(|| newest_log(&self.ws)) {
                Some(log) => self.editor_request = Some(EditorRequest { file: log, line: 1, column: 1 }),
                None => self.notify("no build log yet", true),
            },
            MenuItem::Init => self.run_init(),
            other => {
                if let Some(action) = other.action() {
                    self.act(action);
                }
            }
        }
    }

    pub fn act(&mut self, action: Action) {
        match action {
            Action::Pane1 => self.focus = Pane::Targets,
            Action::Pane2 => self.focus = Pane::Configs,
            Action::Pane3 => self.focus = Pane::Actions,
            Action::Pane4 => self.focus = Pane::Log,
            Action::Pane5 => self.focus = Pane::Db,
            Action::NextPane => self.focus = self.focus.next(1),
            Action::PrevPane => self.focus = self.focus.next(-1),
            Action::Up => self.move_cursor(-1),
            Action::Down => self.move_cursor(1),
            Action::PageUp => self.move_cursor(-(self.page.max(2) as isize / 2)),
            Action::PageDown => self.move_cursor(self.page.max(2) as isize / 2),
            Action::Top => self.move_cursor(isize::MIN / 2),
            Action::Bottom => self.move_cursor(isize::MAX / 2),
            Action::PrevTab | Action::NextTab => {
                self.tab = if self.tab == LogTab::Log { LogTab::Errors } else { LogTab::Log };
                self.focus = Pane::Log;
            }
            Action::Select => self.select(),
            Action::Filter => {
                self.focus = Pane::Targets;
                self.filtering = true;
            }
            Action::Help => self.popup = Some(Popup::Help { scroll: 0 }),
            Action::Menu => self.popup = Some(Popup::Menu { cursor: 0 }),
            Action::Build => self.start_build(Verb::Build),
            Action::Rebuild => self.start_build(Verb::Rebuild),
            Action::Clean => {
                if !self.busy() {
                    let what = self
                        .target
                        .map(|i| self.ws.solution.projects[i].name.clone())
                        .unwrap_or_else(|| "the solution".into());
                    self.popup = Some(Popup::Confirm {
                        message: format!(
                            "Clean {what} ({})? The build output is deleted; the database is kept.",
                            self.active
                        ),
                        action: Confirmed::Clean,
                    });
                }
            }
            Action::Sync => self.start_sync(),
            Action::Parallel => {
                self.parallel = !self.parallel;
                self.notify(format!("parallel builds (-m): {}", if self.parallel { "on" } else { "off" }), false);
            }
            Action::Edit => self.edit_selected(),
            Action::Cancel => {
                if self.running.is_some() {
                    self.popup = Some(Popup::Confirm {
                        message: "Cancel the build? Its whole process tree is ended.".into(),
                        action: Confirmed::CancelBuild,
                    });
                } else {
                    self.quit = true;
                }
            }
            Action::Close => {}
            Action::Quit => {
                if self.running.is_some() {
                    self.popup = Some(Popup::Quit { cursor: 1 });
                } else {
                    self.quit = true;
                }
            }
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        let clamp = |value: usize, len: usize| -> usize {
            if len == 0 { 0 } else { (value as isize + delta).clamp(0, len as isize - 1) as usize }
        };
        match self.focus {
            Pane::Targets => self.tree_cursor = clamp(self.tree_cursor, self.rows.len()),
            Pane::Configs => {
                let cells = &self.grid.cells;
                let at = cells.iter().position(|c| *c == self.config_cursor).unwrap_or(0);
                if let Some(cell) = cells.get(clamp(at, cells.len())) {
                    self.config_cursor = *cell;
                }
            }
            Pane::Actions => self.action_cursor = clamp(self.action_cursor, ACTIONS.len()),
            Pane::Log => match self.tab {
                // Scrolling counts screen rows up from the end: "up" scrolls back. Drawing stops
                // it at the first line, since only drawing knows how lines wrap.
                LogTab::Log => self.log_scroll = (self.log_scroll as isize - delta).max(0) as usize,
                LogTab::Errors => self.diag_cursor = clamp(self.diag_cursor, self.diagnostics.len()),
            },
            Pane::Db => {}
        }
    }

    fn select(&mut self) {
        match self.focus {
            Pane::Targets => match self.rows.get(self.tree_cursor).map(|r| r.kind) {
                Some(RowKind::Solution) => self.target = None,
                Some(RowKind::Project(index)) => self.target = Some(index),
                _ => {}
            },
            Pane::Configs => self.switch_profile(self.grid.profile(self.config_cursor)),
            Pane::Actions => self.act(ACTIONS[self.action_cursor]),
            Pane::Log if self.tab == LogTab::Errors => self.edit_selected(),
            Pane::Log => {}
            Pane::Db => {
                self.refresh_check();
                self.notify("refreshing the database status", false);
            }
        }
    }

    fn edit_selected(&mut self) {
        if self.tab != LogTab::Errors {
            self.tab = LogTab::Errors;
            self.focus = Pane::Log;
            return;
        }
        match self.diagnostics.get(self.diag_cursor).and_then(Diagnostic::location) {
            Some((file, line, column)) => self.editor_request = Some(EditorRequest { file, line, column }),
            None => self.notify("this message has no file to open", true),
        }
    }

    /// The target's name for titles.
    pub fn target_label(&self) -> String {
        match self.target {
            Some(i) => self.ws.solution.projects[i].name.clone(),
            None => format!("[Solution] {}", self.ws.solution.file_name()),
        }
    }

    /// Drops a notice after a while.
    pub fn on_tick(&mut self) {
        if self.notice.as_ref().is_some_and(|n| n.at.elapsed() > Duration::from_secs(6)) {
            self.notice = None;
        }
    }

    pub fn db_updated(&self) -> Option<String> {
        self.check.as_ref().and_then(|c| c.updated_at.as_deref()).map(time::clock)
    }
}

fn is_close(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
}

fn newest_log(ws: &Workspace) -> Option<PathBuf> {
    let read = std::fs::read_dir(ws.logs_dir()).ok()?;
    read.filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}
