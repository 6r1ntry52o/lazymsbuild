//! The real binary in a pseudo console (ConPTY): keys in, screen out. Checks the acceptance
//! criteria of the TUI (spec §8, Phase 2): during a build the cursor answers within 100 ms, and a
//! cancelled build leaves no MSBuild process behind. Skipped without MSBuild.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn msbuild_available() -> bool {
    let vswhere = PathBuf::from(std::env::var("ProgramFiles(x86)").unwrap_or_default())
        .join(r"Microsoft Visual Studio\Installer\vswhere.exe");
    vswhere.is_file()
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

struct Tui {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    parser: Arc<Mutex<vt100::Parser>>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Tui {
    fn start(repo: &Path, state: &Path, config: &Path) -> Tui {
        let pair =
            native_pty_system().openpty(PtySize { rows: 40, cols: 140, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_lazymsbuild"));
        command.args(["--solution", "Basic.sln"]);
        command.cwd(repo);
        command.env("LAZYMSBUILD_STATE_DIR", state);
        command.env("LAZYMSBUILD_CONFIG_DIR", config);
        command.env_remove("EDITOR");
        command.env_remove("VISUAL");
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let size = |n: u16| std::num::NonZero::new(n).unwrap();
        let parser = Arc::new(Mutex::new(vt100::Parser::new(size(40), size(140), 0)));
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let (sink, answer) = (parser.clone(), writer.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut tail: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        // ConPTY asks where the cursor is (ESC[6n) and holds all output until a
                        // terminal answers. This harness is the terminal.
                        tail.extend_from_slice(&buf[..n]);
                        if tail.windows(4).any(|w| w == b"\x1b[6n") {
                            let mut w = answer.lock().unwrap();
                            let _ = w.write_all(b"\x1b[1;1R");
                            let _ = w.flush();
                            tail.clear();
                        } else if tail.len() > 3 {
                            tail.drain(..tail.len() - 3);
                        }
                        sink.lock().unwrap().process(&buf[..n]);
                    }
                }
            }
        });
        Tui { child, writer, parser, _master: pair.master }
    }

    /// The screen, one line per row (`contents()` joins full-width rows, which a TUI's all are).
    fn screen(&self) -> String {
        let parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        let (_, cols) = screen.size();
        screen.rows(0, cols.get()).collect::<Vec<_>>().join("\n")
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    }

    fn wait_for(&self, timeout: Duration, what: &str, until: impl Fn(&str) -> bool) -> Duration {
        let started = Instant::now();
        loop {
            let screen = self.screen();
            if until(&screen) {
                return started.elapsed();
            }
            assert!(started.elapsed() < timeout, "timed out waiting for {what}; the screen:\n{screen}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The row of the targets pane that has the cursor marker.
    fn cursor_row(&self) -> Option<usize> {
        self.screen().lines().position(|l| l.starts_with("│> "))
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        // Closing a pseudo console waits for its client: end lazymsbuild first, or a failed
        // assertion hangs the test instead of reporting.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// MSBuild.exe processes started since `since`, and the pre-build step's ping.
fn build_processes(since: &str) -> usize {
    count_processes(&format!(
        "($_.Name -eq 'MSBuild.exe' -and $_.CreationDate -gt [DateTime]::Parse('{since}')) -or $_.CommandLine -like '*127.0.0.78*'"
    ))
}

/// The running pre-build step (its ping).
fn pre_build_steps() -> usize {
    count_processes("$_.Name -eq 'PING.EXE' -and $_.CommandLine -like '*127.0.0.78*'")
}

fn count_processes(filter: &str) -> usize {
    let script =
        format!("@(Get-CimInstance Win32_Process | Where-Object {{ $_.ProcessId -ne $PID -and ({filter}) }}).Count");
    let output =
        Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim()
        .parse()
        .unwrap_or_else(|_| panic!("unexpected process count {text:?}: {}", String::from_utf8_lossy(&output.stderr)))
}

#[test]
fn the_tui_answers_during_a_build_and_cancel_leaves_nothing() {
    if !msbuild_available() {
        assert_ne!(std::env::var("LAZYMSBUILD_REQUIRE_MSBUILD").as_deref(), Ok("1"), "MSBuild is required");
        eprintln!("skipped: no Visual Studio installation");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let base = std::path::absolute(dir.path()).unwrap();
    let repo = base.join("repo");
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\tests\fixtures\basic"), &repo);
    // A two-minute pre-build step keeps the build running.
    let project = repo.join(r"src\App\App.vcxproj");
    let text = std::fs::read_to_string(&project).unwrap();
    let step = "<PreBuildEvent><Command>ping -n 120 127.0.0.78 &gt;nul</Command></PreBuildEvent>";
    let text = text.replacen("<Link>", &format!("{step}<Link>"), 1);
    std::fs::write(&project, text).unwrap();
    std::fs::create_dir_all(base.join("config")).unwrap();

    let started = chrono::Local::now().to_rfc3339();
    let mut tui = Tui::start(&repo, &base.join("state"), &base.join("config"));
    tui.wait_for(Duration::from_secs(30), "the first screen", |s| s.contains("[5] clangd DB Status"));
    // First start: init is offered. Not now.
    tui.wait_for(Duration::from_secs(10), "the init question", |s| s.contains("Run init?"));
    tui.send(b"n");
    tui.wait_for(Duration::from_secs(10), "the question to close", |s| !s.contains("Run init?"));

    tui.send(b"b");
    tui.wait_for(Duration::from_secs(30), "the build to start", |s| {
        s.contains("Build the solution (Debug|x64) running")
    });
    // Wait until MSBuild reaches the long pre-build step, so the build is really busy.
    let deadline = Instant::now() + Duration::from_secs(60);
    while pre_build_steps() == 0 {
        assert!(Instant::now() < deadline, "the pre-build step did not start");
        std::thread::sleep(Duration::from_millis(200));
    }

    // Cursor latency while the build runs: from writing the key to the screen showing the move.
    let mut latencies = Vec::new();
    for i in 0..12 {
        let before = tui.cursor_row().unwrap_or_else(|| panic!("no cursor in the targets pane:\n{}", tui.screen()));
        tui.send(if i % 2 == 0 { b"j" } else { b"k" });
        let latency = tui.wait_for(Duration::from_secs(5), "the cursor to move", |_| tui_cursor_moved(&tui, before));
        latencies.push(latency);
    }
    latencies.sort();
    let median = latencies[latencies.len() / 2];
    eprintln!("cursor latency during a build: median {median:?}, all {latencies:?}");
    assert!(median < Duration::from_millis(100), "median {median:?} (all {latencies:?})");

    // Ctrl+C asks, y cancels; the whole process tree ends.
    tui.send(&[0x03]);
    tui.wait_for(Duration::from_secs(5), "the cancel question", |s| s.contains("Cancel the build?"));
    tui.send(b"y");
    tui.wait_for(Duration::from_secs(30), "the build to end", |s| s.contains("Build cancelled"));
    assert_eq!(build_processes(&started), 0, "processes of the cancelled build are still running");

    tui.send(b"q");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = tui.child.try_wait().unwrap() {
            assert!(status.success(), "lazymsbuild exited with {status:?}");
            break;
        }
        assert!(Instant::now() < deadline, "q did not quit");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn tui_cursor_moved(tui: &Tui, before: usize) -> bool {
    tui.cursor_row().is_some_and(|row| row != before)
}
