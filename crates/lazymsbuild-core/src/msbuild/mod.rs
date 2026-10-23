//! MSBuild: finding it, the command lines lazymsbuild runs, and running processes.

pub mod invoke;
pub mod locate;
pub mod process;

pub use invoke::Verb;
