//! Method B: the MSBuild logger (`logger/` in the repository) and the events it writes.
//!
//! The logger writes one JSON object per line (format documented in `CompileCommandLogger.cs`)
//! and flushes each, so a cancelled build still leaves every complete line. All interpretation
//! happens here: which command lines are compiles, which project they belong to, which projects
//! were rebuilt and whether they succeeded.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::cl::{self, CompileContext, DirCache};
use super::{Captured, CommandExtractor, Extraction, Method};
use crate::diag::{Diagnostic, Severity};
use crate::{cmdline, paths, text};

pub const LOGGER_CLASS: &str = "LazyMsBuild.Logger.CompileCommandLogger";
pub const LOGGER_DLL: &str = "LazyMsBuild.Logger.dll";
/// The event format this lazymsbuild reads.
pub const FORMAT_VERSION: u32 = 1;

/// Where the logger is, if anywhere.
#[derive(Clone, Debug)]
pub enum Lookup {
    Found(PathBuf),
    /// `extractor.logger = "off"`.
    Disabled,
    /// Not found; the paths that were tried.
    Missing(Vec<PathBuf>),
}

/// Finds the logger: the configured path, otherwise next to lazymsbuild.exe (how releases ship it).
/// Debug builds also look in the repository's `logger\bin`, for `cargo run`.
pub fn find_logger(configured: Option<&str>) -> Lookup {
    match configured.map(str::trim) {
        Some("off") => return Lookup::Disabled,
        Some(path) if !path.is_empty() => {
            let path = PathBuf::from(path);
            return if path.is_file() { Lookup::Found(paths::normalize(&path)) } else { Lookup::Missing(vec![path]) };
        }
        _ => {}
    }
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join(LOGGER_DLL));
    }
    if cfg!(debug_assertions) {
        candidates
            .push(Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\logger\bin\Release\net472").join(LOGGER_DLL));
    }
    match candidates.iter().find(|c| c.is_file()) {
        Some(found) => Lookup::Found(paths::normalize(found)),
        None => Lookup::Missing(candidates),
    }
}

/// Method B as a [`CommandExtractor`].
#[derive(Clone, Debug)]
pub struct BuildLogger {
    pub dll: PathBuf,
}

impl CommandExtractor for BuildLogger {
    fn method(&self) -> Method {
        Method::Build
    }

    /// `-logger:<class>,<dll>;<output>`. No `"` inside: MSBuild splits its own command line and
    /// does not understand `\"`; the whole argument is quoted when it has spaces.
    fn msbuild_args(&self, output: &Path) -> Vec<String> {
        vec![format!("-logger:{LOGGER_CLASS},{};{}", self.dll.display(), output.display())]
    }

    fn collect(&self, output: &Path) -> Result<Extraction> {
        let bytes = std::fs::read(output).with_context(|| {
            format!("the build logger wrote nothing to {} (was the build started?)", output.display())
        })?;
        Ok(parse_events(&text::decode(&bytes))?.extraction())
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Line {
    t: String,
    v: Option<u32>,
    msbuild: Option<String>,
    node: Option<i64>,
    ctx: Option<i64>,
    eval: Option<i64>,
    project: Option<String>,
    targets: Option<String>,
    cfg: Option<String>,
    props: Option<HashMap<String, String>>,
    task: Option<String>,
    cmd: Option<String>,
    ok: Option<bool>,
    sev: Option<String>,
    file: Option<String>,
    line: Option<i64>,
    col: Option<i64>,
    code: Option<String>,
    msg: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct Instance {
    project: Option<String>,
    targets: String,
    platform: Option<String>,
    eval: Option<(i64, i64)>,
    props: Option<HashMap<String, String>>,
    ok: Option<bool>,
}

#[derive(Clone, Debug)]
struct Command {
    instance: (i64, i64),
    project: Option<String>,
    line: String,
}

/// The events of one build.
#[derive(Clone, Debug, Default)]
pub struct Events {
    /// MSBuild's version, as the logger saw it.
    pub msbuild: Option<String>,
    /// The build's result, when it got that far.
    pub succeeded: Option<bool>,
    /// Lines that could not be read (the last one, when the build was killed while writing).
    pub skipped_lines: usize,
    evals: HashMap<(i64, i64), HashMap<String, String>>,
    instances: HashMap<(i64, i64), Instance>,
    commands: Vec<Command>,
    diagnostics: Vec<Diagnostic>,
}

/// Parses the logger's output.
pub fn parse_events(content: &str) -> Result<Events> {
    let mut events = Events::default();
    let mut version = None;
    for raw in text::lines(content).filter(|l| !l.trim().is_empty()) {
        let Ok(line) = serde_json::from_str::<Line>(raw) else {
            events.skipped_lines += 1;
            continue;
        };
        let node = line.node.unwrap_or(0);
        match line.t.as_str() {
            "hello" => {
                version = line.v;
                events.msbuild = line.msbuild;
            }
            "eval" => {
                if let (Some(id), Some(props)) = (line.eval, line.props) {
                    events.evals.insert((node, id), props);
                }
            }
            "pstart" => {
                let Some(ctx) = line.ctx else { continue };
                let platform = line.cfg.as_deref().and_then(|c| c.split_once('|')).map(|(_, p)| p.to_string());
                events.instances.insert(
                    (node, ctx),
                    Instance {
                        project: line.project,
                        targets: line.targets.unwrap_or_default(),
                        platform: platform.filter(|p| !p.is_empty()),
                        eval: line.eval.map(|e| (node, e)),
                        props: line.props,
                        ok: None,
                    },
                );
            }
            "pend" => {
                if let Some(instance) = line.ctx.and_then(|ctx| events.instances.get_mut(&(node, ctx))) {
                    instance.ok = line.ok;
                }
            }
            "cmd" => {
                if let Some(cmd) = line.cmd {
                    events.commands.push(Command {
                        instance: (node, line.ctx.unwrap_or(-1)),
                        project: line.project,
                        line: cmd,
                    });
                }
            }
            "diag" => {
                let severity = if line.sev.as_deref() == Some("warning") { Severity::Warning } else { Severity::Error };
                let project = line.project.filter(|p| !p.is_empty());
                let file = line.file.filter(|f| !f.is_empty()).map(|f| match &project {
                    Some(project) => {
                        paths::absolutize(Path::new(project).parent().unwrap_or(Path::new(".")), Path::new(&f))
                            .to_string_lossy()
                            .into_owned()
                    }
                    None => f,
                });
                let origin = if file.is_none() { line.code.as_deref().map(origin_of) } else { None };
                events.diagnostics.push(Diagnostic {
                    severity,
                    file,
                    line: line.line.filter(|l| *l > 0).map(|l| l as u32),
                    column: line.col.filter(|c| *c > 0).map(|c| c as u32),
                    code: line.code.filter(|c| !c.is_empty()),
                    message: line.msg.unwrap_or_default(),
                    project,
                    origin,
                });
            }
            "end" => events.succeeded = line.ok,
            _ => {}
        }
    }
    match version {
        Some(FORMAT_VERSION) => Ok(events),
        Some(other) => bail!(
            "the build logger writes format {other}, this lazymsbuild reads format {FORMAT_VERSION}; use the LazyMsBuild.Logger.dll that came with it"
        ),
        None => bail!("the build logger's output has no header line; it is not from LazyMsBuild.Logger"),
    }
}

/// The tool a code without a file comes from (`LNK1104` -> `LINK`).
fn origin_of(code: &str) -> String {
    let letters: String = code.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    match letters.as_str() {
        "LNK" => "LINK".into(),
        "MSB" => "MSBUILD".into(),
        "C" | "D" => "CL".into(),
        other => other.into(),
    }
}

impl Events {
    fn props_of<'s>(&'s self, instance: &'s Instance) -> Option<&'s HashMap<String, String>> {
        let by_eval = instance.eval.and_then(|key| {
            self.evals.get(&key).or_else(|| {
                // The evaluation may have been reported by another node.
                let mut found = self.evals.iter().filter(|((_, id), _)| *id == key.1);
                match (found.next(), found.next()) {
                    (Some((_, props)), None) => Some(props),
                    _ => None,
                }
            })
        });
        by_eval.or(instance.props.as_ref())
    }

    /// Turns the events into entries and project outcomes.
    pub fn extraction(&self) -> Extraction {
        let mut out = Extraction::new(Method::Build);
        let mut dirs = DirCache::default();
        let empty = Instance::default();
        for command in &self.commands {
            let Some((program, args)) = cmdline::split_command(&command.line) else { continue };
            if cl::compiler(&program).is_none() {
                continue;
            }
            let instance = self.instances.get(&command.instance).unwrap_or(&empty);
            let Some(project) = command.project.as_deref().or(instance.project.as_deref()).filter(|p| !p.is_empty())
            else {
                out.warnings.push(format!("a compiler call without a project was skipped: {}", short(&command.line)));
                continue;
            };
            let project = paths::normalize(Path::new(project));
            let directory = project.parent().unwrap_or(Path::new(".")).to_path_buf();
            let props = self.props_of(instance);
            let get = |name: &str| {
                props.and_then(|p| p.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()))
            };
            let context = CompileContext {
                directory: &directory,
                platform: instance.platform.as_deref(),
                include_path: get("IncludePath"),
                external_include_path: get("ExternalIncludePath"),
            };
            for entry in cl::entries(&program, &args, &context, &mut dirs) {
                out.entries.push(Captured { entry, project: Some(project.clone()) });
            }
        }

        // Project outcomes. A project counts as built when Build, Rebuild or its default targets
        // ran and finished successfully; as complete when that was a Rebuild.
        let mut built: HashMap<String, (PathBuf, bool)> = HashMap::new();
        for instance in self.instances.values() {
            let Some(project) = instance.project.as_deref().filter(|p| is_project_file(p)) else { continue };
            let targets: Vec<String> =
                instance.targets.split(';').map(|t| t.trim().to_ascii_lowercase()).filter(|t| !t.is_empty()).collect();
            let builds = targets.is_empty() || targets.iter().any(|t| t == "build" || t == "rebuild");
            if !builds || instance.ok != Some(true) {
                continue;
            }
            let rebuilt = targets.iter().any(|t| t == "rebuild");
            let path = paths::normalize(Path::new(project));
            let slot = built.entry(paths::key(&path)).or_insert((path, false));
            slot.1 |= rebuilt;
        }
        for (path, rebuilt) in built.into_values() {
            if rebuilt {
                out.complete_projects.push(path.clone());
            }
            out.current_projects.push(path);
        }
        out.complete_projects.sort();
        out.current_projects.sort();
        out.diagnostics = self.diagnostics.clone();
        if self.skipped_lines > 0 {
            out.warnings
                .push(format!("{} unreadable line(s) in the build logger's output were skipped", self.skipped_lines));
        }
        out
    }
}

/// A project file, not the solution metaproject.
fn is_project_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with("proj") && !lower.ends_with(".metaproj")
}

fn short(line: &str) -> String {
    if line.chars().count() > 120 {
        format!("{}...", line.chars().take(120).collect::<String>())
    } else {
        line.to_string()
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn events() -> String {
        // Abridged from a real Rebuild of tests/fixtures/basic (Debug|x64, -m).
        [
            r#"{"t":"hello","v":1,"msbuild":"17.14.51.32402"}"#,
            r#"{"t":"pstart","node":1,"ctx":4,"eval":1,"project":"C:\\r\\Basic.sln","targets":"Rebuild","cfg":"Debug|x64"}"#,
            r#"{"t":"eval","node":2,"eval":20,"project":"C:\\r\\src\\Util\\Util (Legacy).vcxproj","props":{"IncludePath":"C:\\r\\vendor;;","ExternalIncludePath":"C:\\r\\ext"}}"#,
            r#"{"t":"pstart","node":2,"ctx":20,"eval":20,"parent":4,"project":"C:\\r\\src\\Util\\Util (Legacy).vcxproj","targets":"Rebuild","cfg":"Debug|x64"}"#,
            r#"{"t":"cmd","node":2,"ctx":20,"project":"C:\\r\\src\\Util\\Util (Legacy).vcxproj","task":"CL","cmd":"C:\\VS\\bin\\HostX64\\x64\\CL.exe /c /W4 /D UTIL_LEGACY /Fo\"x64\\Debug\\\\\" /TC util.c \"sub dir\\extra file.c\""}"#,
            r#"{"t":"cmd","node":2,"ctx":20,"project":"C:\\r\\src\\Util\\Util (Legacy).vcxproj","task":"LIB","cmd":"C:\\VS\\bin\\HostX64\\x64\\Lib.exe /OUT:x.lib x64\\Debug\\util.obj"}"#,
            r#"{"t":"diag","sev":"warning","node":2,"ctx":20,"project":"C:\\r\\src\\Util\\Util (Legacy).vcxproj","file":"util.c","line":3,"col":5,"code":"C4100","msg":"unused"}"#,
            r#"{"t":"pend","node":2,"ctx":20,"project":"C:\\r\\src\\Util\\Util (Legacy).vcxproj","ok":true}"#,
            r#"{"t":"pstart","node":1,"ctx":30,"eval":31,"parent":4,"project":"C:\\r\\src\\App\\App.vcxproj","targets":"GetTargetPath","cfg":"Debug|x64"}"#,
            r#"{"t":"pend","node":1,"ctx":30,"project":"C:\\r\\src\\App\\App.vcxproj","ok":true}"#,
            r#"{"t":"pend","node":1,"ctx":4,"project":"C:\\r\\Basic.sln","ok":true}"#,
            r#"{"t":"end","ok":true}"#,
            r#"{"t":"cmd","node":1,"ctx":40,"proj"#,
        ]
        .join("\n")
    }

    #[test]
    fn turns_events_into_entries_and_outcomes() {
        let events = parse_events(&events()).unwrap();
        assert_eq!(events.msbuild.as_deref(), Some("17.14.51.32402"));
        assert_eq!(events.succeeded, Some(true));
        assert_eq!(events.skipped_lines, 1, "the cut-off last line");
        let x = events.extraction();
        let files: Vec<&str> = x.entries.iter().map(|c| c.entry.file.as_str()).collect();
        assert_eq!(files, [r"C:\r\src\Util\util.c", r"C:\r\src\Util\sub dir\extra file.c"]);
        let first = &x.entries[0];
        assert_eq!(first.project.as_deref(), Some(Path::new(r"C:\r\src\Util\Util (Legacy).vcxproj")));
        assert_eq!(first.entry.directory, r"C:\r\src\Util");
        assert_eq!(first.entry.arguments[1], "--target=x86_64-pc-windows-msvc");
        // The include directories do not exist on the test machine, so none are added.
        assert!(!first.entry.arguments.iter().any(|a| a.starts_with("/imsvc")));
        assert_eq!(x.complete_projects, [PathBuf::from(r"C:\r\src\Util\Util (Legacy).vcxproj")]);
        assert_eq!(
            x.current_projects,
            [PathBuf::from(r"C:\r\src\Util\Util (Legacy).vcxproj")],
            "GetTargetPath is not a build"
        );
        assert_eq!(x.diagnostics.len(), 1);
        assert_eq!(x.diagnostics[0].file.as_deref(), Some(r"C:\r\src\Util\util.c"));
        assert_eq!(x.warnings.len(), 1);
    }

    #[test]
    fn rejects_unknown_formats() {
        assert!(parse_events(r#"{"t":"hello","v":99}"#).is_err());
        assert!(parse_events(r#"{"t":"end","ok":true}"#).is_err());
    }

    #[test]
    fn logger_switch_has_no_inner_quotes() {
        let logger = BuildLogger { dll: PathBuf::from(r"C:\dir with space\LazyMsBuild.Logger.dll") };
        let args = logger.msbuild_args(Path::new(r"C:\out dir\e.jsonl"));
        assert_eq!(
            args,
            [
                r"-logger:LazyMsBuild.Logger.CompileCommandLogger,C:\dir with space\LazyMsBuild.Logger.dll;C:\out dir\e.jsonl"
            ]
        );
        assert!(!args[0].contains('"'));
    }
}
