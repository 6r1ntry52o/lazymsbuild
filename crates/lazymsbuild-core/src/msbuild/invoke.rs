//! The MSBuild command lines lazymsbuild runs (spec §3).
//!
//! - The solution is always the entry point; a single project is built through its solution
//!   target (`-t:Libs\MyApp_Core:Rebuild`), so the solution's configuration mapping applies.
//! - Switches use the `-` form, as MSBuild's documentation does.
//! - Always `-nologo` and `-nodeReuse:false` (no worker node outlives the build).
//! - Every argument is passed on its own, never through a shell. MSBuild splits its own command
//!   line and does not understand `\"`, so no argument may contain `"`; arguments with spaces are
//!   quoted as a whole, which MSBuild handles.

use std::fmt;
use std::path::Path;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::solution::Profile;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Verb {
    Build,
    Rebuild,
    Clean,
}

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Verb::Build => "Build",
            Verb::Rebuild => "Rebuild",
            Verb::Clean => "Clean",
        })
    }
}

/// `-t:` for a verb on the whole solution (`target` = `None`) or on one project.
pub fn target_switch(target: Option<&str>, verb: Verb) -> String {
    match (target, verb) {
        (None, verb) => format!("-t:{verb}"),
        (Some(name), Verb::Build) => format!("-t:{name}"),
        (Some(name), verb) => format!("-t:{name}:{verb}"),
    }
}

/// Everything that goes into a build command line.
#[derive(Clone, Debug)]
pub struct BuildArgs<'a> {
    pub solution: &'a Path,
    /// The project's solution target name; `None` for the whole solution.
    pub target: Option<&'a str>,
    pub verb: Verb,
    pub profile: &'a Profile,
    pub parallel: bool,
    pub max_cpu_count: u32,
    pub verbosity: &'a str,
    pub binlog: Option<&'a Path>,
    /// Arguments the build-time extractor adds (the logger switch).
    pub extractor_args: Vec<String>,
    /// `build.extra_args`.
    pub extra_args: &'a [String],
}

/// The arguments (without MSBuild.exe itself).
pub fn build_args(a: &BuildArgs<'_>) -> Result<Vec<String>> {
    let mut args = vec![
        a.solution.display().to_string(),
        "-nologo".into(),
        "-nodeReuse:false".into(),
        target_switch(a.target, a.verb),
        format!("-p:Configuration={}", a.profile.configuration),
        format!("-p:Platform={}", a.profile.platform),
        format!("-v:{}", a.verbosity),
    ];
    if a.parallel {
        args.push(if a.max_cpu_count > 0 { format!("-m:{}", a.max_cpu_count) } else { "-m".into() });
    }
    if let Some(binlog) = a.binlog {
        args.push(format!("-bl:{}", binlog.display()));
    }
    args.extend(a.extractor_args.iter().cloned());
    args.extend(a.extra_args.iter().cloned());
    for arg in &args {
        if arg.contains('"') {
            bail!("cannot pass an argument containing '\"' to MSBuild: {arg}");
        }
    }
    for value in [&a.profile.configuration, &a.profile.platform] {
        if value.contains([';', ',']) {
            bail!("MSBuild cannot take a configuration or platform containing ';' or ',': {value}");
        }
    }
    Ok(args)
}

/// Environment for MSBuild. With `english`, MSBuild 17 (`DOTNET_CLI_UI_LANGUAGE`) and the
/// compiler and linker (`VSLANG`, when their English resources are installed) print English.
pub fn environment(english: bool) -> Vec<(String, String)> {
    if !english {
        return Vec::new();
    }
    vec![("DOTNET_CLI_UI_LANGUAGE".into(), "en-US".into()), ("VSLANG".into(), "1033".into())]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn target_switches() {
        assert_eq!(target_switch(None, Verb::Rebuild), "-t:Rebuild");
        assert_eq!(target_switch(Some(r"Libs\MyApp_Core"), Verb::Build), r"-t:Libs\MyApp_Core");
        assert_eq!(
            target_switch(Some(r"Libs\Native_Ext\Util _Legacy_"), Verb::Rebuild),
            r"-t:Libs\Native_Ext\Util _Legacy_:Rebuild"
        );
        assert_eq!(target_switch(Some("App"), Verb::Clean), "-t:App:Clean");
    }

    #[test]
    fn build_command_line() {
        let profile = Profile::new("Debug", "x64");
        let extra = vec!["-p:Foo=1".to_string()];
        let solution = PathBuf::from(r"C:\repo dir\A.sln");
        let binlog = PathBuf::from(r"C:\state\logs\1.binlog");
        let a = BuildArgs {
            solution: &solution,
            target: Some("App"),
            verb: Verb::Rebuild,
            profile: &profile,
            parallel: true,
            max_cpu_count: 0,
            verbosity: "minimal",
            binlog: Some(&binlog),
            extractor_args: vec!["-logger:X,C:\\l.dll;C:\\e.jsonl".into()],
            extra_args: &extra,
        };
        assert_eq!(
            build_args(&a).unwrap(),
            [
                r"C:\repo dir\A.sln",
                "-nologo",
                "-nodeReuse:false",
                "-t:App:Rebuild",
                "-p:Configuration=Debug",
                "-p:Platform=x64",
                "-v:minimal",
                "-m",
                r"-bl:C:\state\logs\1.binlog",
                r"-logger:X,C:\l.dll;C:\e.jsonl",
                "-p:Foo=1"
            ]
        );
        let quoted = vec![r#"-p:X="a""#.to_string()];
        assert!(build_args(&BuildArgs { extra_args: &quoted, ..a.clone() }).is_err());
        let bad = Profile::new("Debug;x", "x64");
        assert!(build_args(&BuildArgs { profile: &bad, extra_args: &[], ..a }).is_err());
    }
}
