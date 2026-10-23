//! Cancelling a build ends its whole process tree. In its own test binary, so no other test's
//! MSBuild runs at the same time.

mod common;

use std::process::Command;
use std::time::Duration;

use lazymsbuild_core::engine::{self, BuildRequest};
use lazymsbuild_core::msbuild::Verb;
use lazymsbuild_core::status::{self, State};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[test]
fn cancel_leaves_no_process_behind() {
    require_msbuild!();
    let sb = common::sandbox("basic", false);
    // A pre-build step that would take two minutes, started by MSBuild through cmd.exe.
    let project = sb.path(r"src\App\App.vcxproj");
    let text = std::fs::read_to_string(&project).unwrap().replace(
        "<Link>\r\n      <SubSystem>Console</SubSystem>",
        "<PreBuildEvent><Command>ping -n 120 127.0.0.77 &gt;nul</Command></PreBuildEvent>\r\n    <Link>\r\n      <SubSystem>Console</SubSystem>",
    );
    let text = text.replace(
        "<Link>\n      <SubSystem>Console</SubSystem>",
        "<PreBuildEvent><Command>ping -n 120 127.0.0.77 &gt;nul</Command></PreBuildEvent>\n    <Link>\n      <SubSystem>Console</SubSystem>",
    );
    assert!(text.contains("127.0.0.77"));
    std::fs::write(&project, text).unwrap();

    let mut workspace = sb.workspace("Basic.sln");
    let started = chrono::Local::now();
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let (outcome, saw_ping) = common::runtime().block_on(async {
        let watcher = tokio::spawn(async move {
            // Cancel once the pre-build step runs.
            let mut seen = false;
            for _ in 0..60 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if count_processes(&started.to_rfc3339(), "ping") > 0 {
                    seen = true;
                    break;
                }
            }
            trigger.cancel();
            seen
        });
        let (tx, _rx) = mpsc::unbounded_channel();
        let request = BuildRequest { target: None, verb: Verb::Rebuild, parallel: None, force: false };
        let outcome = engine::build(&mut workspace, &request, &tx, cancel).await.unwrap();
        (outcome, watcher.await.unwrap())
    });
    assert!(saw_ping, "the pre-build step never ran, so the cancel tested nothing");
    assert_eq!(outcome.state, State::Cancelled);
    assert_eq!(status::read(&workspace.status_path()).unwrap().state, State::Cancelled);
    assert_eq!(count_processes(&started.to_rfc3339(), "all"), 0, "processes of the cancelled build are still running");
}

/// MSBuild.exe processes started since `since`, and the pre-build step's ping.
fn count_processes(since: &str, which: &str) -> usize {
    let filter = match which {
        "ping" => "$_.CommandLine -like '*127.0.0.77*'".to_string(),
        _ => format!(
            "($_.Name -eq 'MSBuild.exe' -and $_.CreationDate -gt [DateTime]::Parse('{since}')) -or $_.CommandLine -like '*127.0.0.77*'"
        ),
    };
    let script =
        format!("@(Get-CimInstance Win32_Process | Where-Object {{ $_.ProcessId -ne $PID -and ({filter}) }}).Count");
    let output =
        Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim().parse().unwrap_or(usize::MAX)
}
