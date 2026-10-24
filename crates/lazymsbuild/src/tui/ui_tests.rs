//! The TUI against a copy of the fixture solution, drawn into ratatui's test backend.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use lazymsbuild_core::engine::Event;
use lazymsbuild_core::solution::Profile;
use lazymsbuild_core::workspace::{OpenOptions, Workspace};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::UnboundedReceiver;

use super::app::{App, Done, LogTab, Pane, Popup};
use super::view;

struct Harness {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    runtime: Runtime,
    app: App,
    events: UnboundedReceiver<Event>,
    done: UnboundedReceiver<Done>,
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let base = std::path::absolute(dir.path()).unwrap();
    let repo = base.join("repo");
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\tests\fixtures\basic"), &repo);
    std::fs::create_dir_all(base.join("config")).unwrap();
    let workspace = Workspace::open(&OpenOptions {
        start: repo.clone(),
        solution: Some(repo.join("Basic.sln")),
        state_root: Some(base.join("state")),
        user_config_dir: Some(base.join("config")),
    })
    .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
    let (done_tx, done) = tokio::sync::mpsc::unbounded_channel();
    let app = {
        let _guard = runtime.enter();
        App::new(workspace, events_tx, done_tx).unwrap()
    };
    let mut h = Harness { _dir: dir, repo, runtime, app, events, done };
    // The first start offers init; most tests do not want it.
    assert!(matches!(h.app.popup, Some(Popup::Confirm { .. })), "init is offered on first start");
    h.key(KeyCode::Char('n'));
    h.settle();
    h
}

impl Harness {
    fn key(&mut self, code: KeyCode) {
        self.key_with(code, KeyModifiers::NONE);
    }

    fn key_with(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let _guard = self.runtime.enter();
        self.app.on_key(KeyEvent { code, modifiers, kind: KeyEventKind::Press, state: KeyEventState::NONE });
    }

    fn keys(&mut self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
    }

    /// Feeds finished tasks and events to the app until `until` holds (or a timeout).
    fn wait_for(&mut self, timeout: Duration, until: impl Fn(&App) -> bool) {
        let deadline = std::time::Instant::now() + timeout;
        while !until(&self.app) {
            assert!(std::time::Instant::now() < deadline, "timed out");
            let _guard = self.runtime.enter();
            while let Ok(event) = self.events.try_recv() {
                self.app.on_event(event);
            }
            match self
                .runtime
                .block_on(async { tokio::time::timeout(Duration::from_millis(50), self.done.recv()).await })
            {
                Ok(Some(done)) => self.app.on_done(done),
                Ok(None) => panic!("the task channel closed"),
                Err(_) => {}
            }
        }
    }

    /// Waits for the background check the app starts.
    fn settle(&mut self) {
        self.wait_for(Duration::from_secs(30), |app| !app.checking && app.check.is_some());
    }

    fn screen(&mut self, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| view::draw(frame, &mut self.app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                let mut line = String::new();
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    line.push_str(symbol);
                    // A wide character covers the next cell too.
                    x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
                }
                line
            })
            .collect()
    }
}

fn contains(screen: &[String], text: &str) -> bool {
    screen.iter().any(|line| line.contains(text))
}

#[test]
fn draws_the_five_panes() {
    let mut h = harness();
    let screen = h.screen(120, 36);
    let dump = screen.join("\n");
    for text in [
        "[1] Solution & Targets",
        "[2] Configuration / Platform",
        "[3] Actions & Options",
        "[4] Log | Errors (E:0 W:0)",
        "[5] clangd DB Status",
        "[Solution] Basic.sln",
        "Libs/",
        "MyApp.Core",
        "Native.Ext/",
        "Util (Legacy)",
        "App",
        "x64  x86",
        " Debug     ●    ·",
        " Release   ·    ·",
        "● active  ◆ database",
        "Parallel (-m) : ON",
        "No database for Debug|x64 yet.",
        "b build · r rebuild · c clean · s sync · x menu · ? help · q quit",
    ] {
        assert!(dump.contains(text), "missing {text:?} in\n{dump}");
    }
    // Folders first, then projects; the solution row has the cursor.
    let row = |text: &str| screen.iter().position(|l| l.contains(text)).unwrap();
    assert!(row("Libs/") < row("MyApp.Core") && row("MyApp.Core") < row("─ App"), "\n{dump}");
    assert!(contains(&screen, "target: [Solution] Basic.sln"), "\n{dump}");
    assert!(screen[row("[Solution] Basic.sln")].contains("> [Solution] Basic.sln"));
}

#[test]
fn a_narrow_terminal_shows_only_the_focused_pane() {
    let mut h = harness();
    let screen = h.screen(60, 20);
    assert!(contains(&screen, "[1] Solution & Targets"));
    assert!(!contains(&screen, "[5] clangd DB Status"));
    h.key(KeyCode::Char('5'));
    let screen = h.screen(60, 20);
    assert!(contains(&screen, "[5] clangd DB Status"));
    assert!(!contains(&screen, "[1] Solution & Targets"));
}

#[test]
fn panes_cursors_and_target_selection() {
    let mut h = harness();
    h.key(KeyCode::Tab);
    assert_eq!(h.app.focus, Pane::Configs);
    h.key_with(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(h.app.focus, Pane::Targets);
    h.keys("4");
    assert_eq!(h.app.focus, Pane::Log);
    h.keys("1");

    // Select Util (Legacy): the last folder's project.
    let util = h.app.rows.iter().position(|r| r.label == "Util (Legacy)").unwrap();
    for _ in 0..util {
        h.keys("j");
    }
    h.key(KeyCode::Enter);
    let index = h.app.ws.solution.projects.iter().position(|p| p.name == "Util (Legacy)").unwrap();
    assert_eq!(h.app.target, Some(index));
    assert!(contains(&h.screen(120, 36), "target: Util (Legacy)"));
    h.keys("g");
    assert_eq!(h.app.tree_cursor, 0);
    h.key(KeyCode::Char(' '));
    assert_eq!(h.app.target, None, "space selects too; the solution row selects the whole solution");
}

#[test]
fn switching_the_configuration() {
    let mut h = harness();
    // Down a row is Release; ] moves across to x86; [ back; Enter on Debug|x86.
    h.keys("2j]][k");
    assert_eq!(h.app.config_cursor, (0, 0), "[ and ] move across and stop at the ends");
    h.keys("]");
    assert_eq!(h.app.focus, Pane::Configs, "[ and ] stay in the pane");
    h.key(KeyCode::Enter);
    h.wait_for(Duration::from_secs(30), |app| app.active == Profile::new("Debug", "x86"));
    // No database yet: the app offers a sync (or explains when there is no extractor).
    assert!(matches!(h.app.popup, Some(Popup::Confirm { .. }) | Some(Popup::Message { .. })));
    h.key(KeyCode::Esc);
    let screen = h.screen(120, 36);
    assert!(contains(&screen, " Debug     ·    ●"), "{}", screen.join("\n"));
    assert_eq!(h.app.ws.active_profile().unwrap(), Profile::new("Debug", "x86"), "remembered in state.json");
}

#[test]
fn the_filter_narrows_the_tree() {
    let mut h = harness();
    h.keys("/uti");
    assert!(h.app.filtering);
    let labels: Vec<&str> = h.app.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["[Solution] Basic.sln", "Libs/", "Native.Ext/", "Util (Legacy)"]);
    assert!(contains(&h.screen(120, 36), "/uti_"));
    h.key(KeyCode::Enter);
    assert!(!h.app.filtering);
    assert_eq!(h.app.rows.len(), 4, "Enter keeps the filter");
    h.keys("/");
    h.key(KeyCode::Esc);
    assert_eq!(h.app.rows.len(), 6, "Esc clears it");
}

#[test]
fn help_menu_and_confirmations() {
    let mut h = harness();
    h.keys("?");
    let screen = h.screen(120, 36);
    assert!(contains(&screen, "Keys (Esc closes)"));
    assert!(contains(&screen, "Cancel the build (asks first); quit when idle"));
    h.key(KeyCode::Esc);
    assert!(h.app.popup.is_none());

    h.keys("x");
    assert!(contains(&h.screen(120, 36), "Sync (fill the database without building)"));
    // A key of an action in the menu runs it.
    h.keys("m");
    assert!(h.app.popup.is_none());
    assert!(!h.app.parallel);

    h.keys("c");
    assert!(matches!(h.app.popup, Some(Popup::Confirm { .. })), "Clean asks first");
    h.keys("n");
    assert!(h.app.popup.is_none() && h.app.running.is_none());

    h.keys("q");
    assert!(h.app.quit, "q quits when idle");
}

#[test]
fn long_log_lines_wrap_and_scrolling_holds_still() {
    assert_eq!(view::wrap("abcdef", 4), ["abcd", "ef"]);
    assert_eq!(view::wrap("コードを", 5), ["コー", "ドを"], "wide characters count two columns");
    assert_eq!(view::wrap("", 4), [""]);

    let mut h = harness();
    h.keys("4");
    let long = format!("C:\\src\\{}\\main.cpp(7,13): error C2065: 'y': undeclared identifier", "dir".repeat(40));
    h.app.on_event(Event::Line(long));
    let screen = h.screen(120, 36);
    assert!(contains(&screen, "undeclared identifier"), "the end of a long line is visible:\n{}", screen.join("\n"));

    for i in 0..100 {
        h.app.on_event(Event::Line(format!("line {i}")));
    }
    h.screen(120, 36);
    h.keys("kkk");
    let before = h.screen(120, 36);
    for i in 100..110 {
        h.app.on_event(Event::Line(format!("line {i}")));
    }
    let after = h.screen(120, 36);
    let log_rows = |s: &[String]| s.iter().filter(|l| l.contains("line ")).cloned().collect::<Vec<_>>();
    assert_eq!(log_rows(&before), log_rows(&after), "a scrolled-back view does not move while lines arrive");
    h.keys("g");
    assert!(contains(&h.screen(120, 36), "lazymsbuild 0.1.0"), "g goes to the first line");
    h.keys("G");
    assert!(contains(&h.screen(120, 36), "line 109"), "G follows the output again");
    assert_eq!(h.app.log_scroll, 0);
}

/// Prints the screen after a rebuild, to look at the layout:
/// `cargo test -p lazymsbuild print_the_screen -- --ignored --nocapture`.
#[test]
#[ignore = "prints the screen for a person to look at"]
fn print_the_screen() {
    let mut h = harness();
    if lazymsbuild_core::msbuild::locate::locate(None, None).is_ok() {
        h.keys("r");
        h.wait_for(Duration::from_secs(300), |app| app.running.is_none());
    }
    let width = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(120);
    let height = std::env::var("LINES").ok().and_then(|c| c.parse().ok()).unwrap_or(36);
    println!("{}", h.screen(width, height).join("\n"));
    h.keys("4]");
    println!("{}", h.screen(width, height).join("\n"));
}

#[test]
fn building_from_the_tui_updates_the_status_pane() {
    if lazymsbuild_core::msbuild::locate::locate(None, None).is_err() {
        assert_ne!(std::env::var("LAZYMSBUILD_REQUIRE_MSBUILD").as_deref(), Ok("1"), "MSBuild is required");
        eprintln!("skipped: no MSBuild");
        return;
    }
    let mut h = harness();
    h.keys("r");
    assert!(h.app.running.is_some());
    assert!(contains(&h.screen(120, 36), "Rebuild the solution (Debug|x64) running"));
    // Keys still work while it runs; another build waits.
    h.keys("2jk1");
    assert_eq!(h.app.focus, Pane::Targets);
    h.keys("b");
    assert!(h.app.notice.as_ref().is_some_and(|n| n.text.contains("is running")));
    h.wait_for(Duration::from_secs(300), |app| app.running.is_none());
    let check = h.app.check.clone().unwrap();
    let screen = h.screen(120, 36);
    let dump = screen.join("\n");
    let log = std::fs::read_dir(h.app.ws.state_dir.join("logs"))
        .into_iter()
        .flatten()
        .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!((check.coverage.covered, check.coverage.total), (7, 7), "{dump}\n{log}");
    assert!(dump.contains("Entries  : 7 (+7)"), "{dump}");
    assert!(dump.contains("Coverage : 7/7"), "{dump}");
    assert!(dump.contains("✔ published"), "{dump}");
    assert!(dump.contains("Rebuild succeeded"), "{dump}");
    assert_eq!(h.app.tab, LogTab::Log);
    assert!(h.repo.join("compile_commands.json").is_file());
}

#[test]
fn the_configuration_table_keeps_the_solution_order() {
    let profiles: Vec<Profile> =
        ["Debug|ARM64", "Debug|x64", "Release|x64", "Profile|x64"].iter().map(|p| Profile::parse(p).unwrap()).collect();
    let grid = super::app::ConfigGrid::new(&profiles);
    assert_eq!(grid.configurations, ["Debug", "Release", "Profile"]);
    assert_eq!(grid.platforms, ["ARM64", "x64"]);
    assert_eq!(grid.profile((2, 1)), Profile::new("Profile", "x64"));
    assert_eq!(grid.position(&Profile::new("Release", "x64")), Some((1, 1)));
    assert_eq!(grid.position(&Profile::new("Release", "Win32")), None);
}
