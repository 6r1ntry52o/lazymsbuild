//! Command-line arguments.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "lazymsbuild",
    version,
    about = "A lazygit-style TUI for MSBuild that keeps compile_commands.json complete for clangd",
    long_about = "A lazygit-style TUI for MSBuild that keeps compile_commands.json complete for clangd.\n\n\
                  Without a command, starts the TUI. Run `lazymsbuild init` once in a repository, then \
                  `lazymsbuild sync` (or a rebuild) to fill the database.",
    after_help = "Configurations are written Configuration|Platform; quote them in a shell: --profile \"Release|x64\"."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub global: Global,
}

#[derive(Debug, Args)]
pub struct Global {
    /// Run as if lazymsbuild was started in DIR (like `git -C`)
    #[arg(short = 'C', global = true, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// The solution (.sln or .slnx) to use, when there are several
    #[arg(long, global = true, value_name = "FILE")]
    pub solution: Option<PathBuf>,

    /// Print machine-readable JSON instead of text
    #[arg(long, global = true)]
    pub json: bool,

    /// Print less (repeat for even less)
    #[arg(short, long, global = true, action = ArgAction::Count, conflicts_with = "verbose")]
    pub quiet: u8,

    /// Print more
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write .clangd and keep the generated files out of `git status`
    Init(InitArgs),
    /// Fill the database without building (design-time extraction)
    Sync(SyncArgs),
    /// Build without the TUI and update the database
    Build(BuildArgs),
    /// Check that the database is complete and current
    Check(ProfileArg),
    /// Check everything clangd needs to use the database
    Doctor(DoctorArgs),
    /// Print where lazymsbuild keeps things
    Path(PathArgs),
    /// Print a shell completion script
    Completions(CompletionsArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Where compile_commands.json goes: in the repository (default) or in lazymsbuild's state directory
    #[arg(long, value_enum)]
    pub mode: Option<ModeArg>,

    /// How to keep generated files out of Git
    #[arg(long, value_enum)]
    pub ignore: Option<IgnoreArg>,

    /// Do not ask before changing .gitignore
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ModeArg {
    RepoRoot,
    External,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum IgnoreArg {
    /// .git/info/exclude: local to this clone, changes no tracked file
    Exclude,
    /// .gitignore: shared with the team
    Gitignore,
    None,
}

#[derive(Debug, Args)]
pub struct ProfileArg {
    /// The configuration, as Configuration|Platform (default: the active one)
    #[arg(long, value_name = "CONFIG|PLATFORM")]
    pub profile: Option<String>,
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// The configuration, as Configuration|Platform; it becomes the active one
    #[arg(long, value_name = "CONFIG|PLATFORM")]
    pub profile: Option<String>,

    /// Overwrite a compile_commands.json that lazymsbuild did not write
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct BuildArgs {
    /// A project (its name or its solution target name); default: the whole solution
    #[arg(long, value_name = "PROJECT")]
    pub target: Option<String>,

    /// The configuration, as Configuration|Platform; it becomes the active one
    #[arg(long, value_name = "CONFIG|PLATFORM")]
    pub profile: Option<String>,

    /// Rebuild instead of an incremental build
    #[arg(long, conflicts_with = "clean")]
    pub rebuild: bool,

    /// Clean instead of building (the database is not changed)
    #[arg(long)]
    pub clean: bool,

    /// Build projects one at a time (no -m)
    #[arg(long)]
    pub no_parallel: bool,

    /// Overwrite a compile_commands.json that lazymsbuild did not write
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Output for an issue report: paths masked, no project or file names
    #[arg(long)]
    pub report: bool,
}

#[derive(Debug, Args)]
#[group(multiple = false)]
pub struct PathArgs {
    /// The directory of the published compile_commands.json (for clangd --compile-commands-dir)
    #[arg(long)]
    pub db: bool,

    /// The state directory (status.json, errors.txt, logs)
    #[arg(long)]
    pub state: bool,

    /// The user configuration directory
    #[arg(long)]
    pub config: bool,
}

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    pub shell: clap_complete::Shell,
}
