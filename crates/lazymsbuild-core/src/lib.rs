//! Core of lazymsbuild: everything except the terminal UI and the command-line parsing.
//!
//! Data flows through the modules roughly in this order:
//! - [`workspace`] finds the solution, the repository root and where lazymsbuild keeps its state;
//! - [`solution`] and [`vcxproj`] read `.sln`/`.slnx` and `.vcxproj` files;
//! - [`msbuild`] finds MSBuild, runs builds and the design-time extractor, and reads their output;
//! - [`extract`] turns the extractor's output (method A) and the logger's events (method B) into entries;
//! - [`store`] keeps one compile database per configuration and publishes the active one;
//! - [`check`] and [`doctor`] answer "is the database complete and current, and will clangd use it?".
//!
//! `docs/design.md` in the repository explains the measurements behind these choices.

pub mod check;
pub mod clangd;
pub mod cmdline;
pub mod compdb;
pub mod config;
pub mod diag;
pub mod doctor;
pub mod engine;
pub mod extract;
pub mod fsutil;
pub mod git;
pub mod init;
pub mod lock;
pub mod msbuild;
pub mod paths;
pub mod solution;
pub mod status;
pub mod store;
pub mod text;
pub mod time;
pub mod vcxproj;
pub mod workspace;

#[cfg(windows)]
mod win;
