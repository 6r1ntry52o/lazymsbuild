//! The terminal UI.

use anyhow::Result;

use crate::cli::Global;

pub async fn run(_global: &Global) -> Result<i32> {
    anyhow::bail!("the TUI is not built yet; use the subcommands (lazymsbuild --help)")
}
