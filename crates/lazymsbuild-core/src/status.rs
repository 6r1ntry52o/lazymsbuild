//! `status.json`: what lazymsbuild is doing, for anyone to read without talking to it (an editor's
//! status line, a notification when a build finishes).
//!
//! It is written while a build or sync runs (every second and on every state change) and once
//! when it ends, always atomically. It is a public format with a version: see
//! `docs/formats.md`. Additions are backwards compatible; anything else bumps `format_version`.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::fsutil;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Idle,
    Syncing,
    Building,
    Succeeded,
    Failed,
    Cancelled,
}

impl State {
    /// A build or sync is running.
    pub fn is_running(self) -> bool {
        matches!(self, State::Syncing | State::Building)
    }
}

/// The database after the last change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DbStatus {
    /// The published `compile_commands.json`.
    pub path: String,
    pub updated_at: Option<String>,
    pub entries: usize,
    /// Entries added (positive) or removed (negative) by the last change.
    pub delta: i64,
    /// Sources in the database / sources the configuration compiles.
    pub coverage: [usize; 2],
    /// Projects whose project files changed after their entries were captured.
    pub stale_projects: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub format_version: u32,
    pub state: State,
    /// `Debug|x64`.
    pub profile: Option<String>,
    /// The MSBuild target name of the project (`Libs\MyApp_Core`); null for the whole solution.
    pub target: Option<String>,
    /// `Build`, `Rebuild`, `Clean` or `Sync`.
    pub verb: Option<String>,
    pub solution: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub errors: u32,
    pub warnings: u32,
    /// Process id of the lazymsbuild that runs the build.
    pub pid: Option<u32>,
    /// The build's log file.
    pub log: Option<String>,
    pub db: Option<DbStatus>,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            format_version: FORMAT_VERSION,
            state: State::Idle,
            profile: None,
            target: None,
            verb: None,
            solution: None,
            started_at: None,
            finished_at: None,
            errors: 0,
            warnings: 0,
            pid: None,
            log: None,
            db: None,
        }
    }
}

/// Reads `status.json`. `None` when there is none or it cannot be read.
pub fn read(path: &Path) -> Option<Status> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes `status.json` atomically.
pub fn write(path: &Path, status: &Status) -> Result<()> {
    let mut json = serde_json::to_string_pretty(status)?;
    json.push('\n');
    fsutil::write_atomic(path, json.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_the_documented_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.json");
        let status = Status {
            state: State::Building,
            profile: Some("Debug|x64".into()),
            target: Some(r"Libs\MyApp_Core".into()),
            verb: Some("Build".into()),
            started_at: Some("2026-10-23T12:00:00.000+09:00".into()),
            warnings: 3,
            db: Some(DbStatus { path: "x".into(), entries: 342, delta: 2, coverage: [342, 350], ..Default::default() }),
            ..Default::default()
        };
        write(&path, &status).unwrap();
        assert_eq!(read(&path), Some(status));
        let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["format_version"], 1);
        assert_eq!(value["state"], "building");
        assert_eq!(value["db"]["coverage"], serde_json::json!([342, 350]));
        assert!(value["finished_at"].is_null());
    }
}
