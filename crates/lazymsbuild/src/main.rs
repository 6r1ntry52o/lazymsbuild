//! lazymsbuild: a lazygit-style terminal UI for MSBuild that keeps compile_commands.json complete
//! for clangd. See the README for what it does and docs/design.md for why it works this way.

mod cli;
mod commands;
mod style;
mod tui;

use clap::Parser;

use crate::cli::{Cli, Command};

fn main() {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("lazymsbuild: cannot start: {err}");
            std::process::exit(1);
        }
    };
    let global = &cli.global;
    let result = runtime.block_on(async {
        match &cli.command {
            None => tui::run(global).await,
            Some(Command::Init(args)) => commands::init(global, args),
            Some(Command::Sync(args)) => commands::sync(global, args).await,
            Some(Command::Build(args)) => commands::build(global, args).await,
            Some(Command::Check(args)) => commands::check(global, args),
            Some(Command::Doctor(args)) => commands::doctor(global, args),
            Some(Command::Path(args)) => commands::path(global, args),
            Some(Command::Completions(args)) => commands::completions(args),
        }
    });
    let code = match result {
        Ok(code) => code,
        Err(err) => {
            if global.json {
                style::say!("{}", serde_json::json!({ "error": format!("{err:#}") }));
            } else {
                eprintln!("{} {err:#}", style::red("error:"));
            }
            1
        }
    };
    // The runtime may hold blocked reader threads of ended processes; do not wait for them.
    runtime.shutdown_timeout(std::time::Duration::from_millis(200));
    std::process::exit(code);
}
