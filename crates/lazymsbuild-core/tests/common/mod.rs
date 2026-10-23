//! Shared by the integration tests: sandboxes with a copy of a fixture, and running operations.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use lazymsbuild_core::engine::{self, BuildRequest, Event, Outcome};
use lazymsbuild_core::msbuild::{Verb, locate};
use lazymsbuild_core::workspace::{OpenOptions, Workspace};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Skips the test (returns) when MSBuild with the C++ tools is missing, unless
/// `LAZYMSBUILD_REQUIRE_MSBUILD=1`, which makes it a failure (CI).
#[macro_export]
macro_rules! require_msbuild {
    () => {
        if !$crate::common::msbuild_available() {
            if std::env::var("LAZYMSBUILD_REQUIRE_MSBUILD").as_deref() == Ok("1") {
                panic!("MSBuild with the C++ build tools is required (LAZYMSBUILD_REQUIRE_MSBUILD=1)");
            }
            eprintln!("skipped: MSBuild with the C++ build tools is not installed");
            return;
        }
    };
}

pub fn msbuild_available() -> bool {
    locate::locate(None, None).is_ok()
}

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\tests\fixtures")
}

/// A temporary copy of a fixture, in a directory whose name has spaces and non-ASCII
/// characters, with its own state and configuration directories.
pub struct Sandbox {
    _dir: tempfile::TempDir,
    pub repo: PathBuf,
    pub state: PathBuf,
    pub config: PathBuf,
}

pub fn sandbox(fixture: &str, git: bool) -> Sandbox {
    let dir = tempfile::Builder::new().prefix("lazymsbuild テスト ").tempdir().unwrap();
    let base = std::path::absolute(dir.path()).unwrap();
    let repo = base.join("repo dir");
    copy_dir(&fixtures().join(fixture), &repo);
    let state = base.join("state");
    let config = base.join("config");
    std::fs::create_dir_all(&config).unwrap();
    if git && git_available() {
        let run = |args: &[&str]| {
            let ok =
                Command::new("git").current_dir(&repo).args(args).output().map(|o| o.status.success()).unwrap_or(false);
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["add", "-A"]);
        run(&["-c", "user.name=test", "-c", "user.email=test@example.invalid", "commit", "-q", "-m", "fixture"]);
    }
    Sandbox { _dir: dir, repo, state, config }
}

pub fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
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

impl Sandbox {
    pub fn workspace(&self, solution: &str) -> Workspace {
        Workspace::open(&OpenOptions {
            start: self.repo.clone(),
            solution: Some(self.repo.join(solution)),
            state_root: Some(self.state.clone()),
            user_config_dir: Some(self.config.clone()),
        })
        .unwrap()
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.repo.join(rel)
    }
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
}

/// Runs a build and returns its outcome and every event.
pub fn build(workspace: &mut Workspace, verb: Verb, target: Option<&str>) -> (Outcome, Vec<Event>) {
    let request = BuildRequest { target: target.map(str::to_string), verb, parallel: None, force: false };
    runtime().block_on(async {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let outcome = engine::build(workspace, &request, &tx, CancellationToken::new()).await.unwrap();
        drop(tx);
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        (outcome, events)
    })
}

pub fn sync(workspace: &mut Workspace) -> Outcome {
    runtime().block_on(async {
        let (tx, _rx) = mpsc::unbounded_channel();
        engine::sync(workspace, false, &tx, CancellationToken::new()).await.unwrap()
    })
}

pub fn lines(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Line(l) => Some(l.as_str()),
            _ => None,
        })
        .collect()
}
