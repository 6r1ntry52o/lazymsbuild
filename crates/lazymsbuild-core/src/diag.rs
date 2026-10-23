//! Errors and warnings of a build, and `errors.txt`.
//!
//! The logger reports them as structured events, which is the primary source. When there is no
//! logger output, they are parsed from MSBuild's console output, which uses the MSVC canonical
//! format whatever the display language (only the message text is translated):
//!
//! ```text
//! C:\src\main.cpp(7,13): error C2065: 'y': undeclared identifier [C:\src\App.vcxproj]
//! LINK : fatal error LNK1104: cannot open file 'x.lib' [C:\src\App.vcxproj]
//! ```
//!
//! `errors.txt` uses the same format without the project suffix, which Neovim's default
//! `errorformat` (`:compiler msvc`) and VS Code's `$msCompile` problem matcher read as they are.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{fsutil, paths};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    /// The source file, absolute when known.
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub code: Option<String>,
    pub message: String,
    /// The project that reported it.
    pub project: Option<String>,
    /// The reporting tool when there is no file (`LINK`, `MSBUILD`).
    pub origin: Option<String>,
}

impl Diagnostic {
    /// The MSVC canonical form, as written to `errors.txt`.
    pub fn to_msvc(&self) -> String {
        let mut out = String::new();
        match (&self.file, self.line) {
            (Some(file), Some(line)) if line > 0 => {
                let _ = match self.column.filter(|c| *c > 0) {
                    Some(column) => write!(out, "{file}({line},{column}): "),
                    None => write!(out, "{file}({line}): "),
                };
            }
            (Some(file), _) => {
                let _ = write!(out, "{file} : ");
            }
            (None, _) => {
                let _ = write!(out, "{} : ", self.origin.as_deref().unwrap_or("MSBUILD"));
            }
        }
        out.push_str(match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        });
        if let Some(code) = &self.code {
            let _ = write!(out, " {code}");
        }
        let _ = write!(out, ": {}", self.message.replace(['\r', '\n'], " "));
        out
    }

    /// Where an editor should jump: the file and 1-based line and column.
    pub fn location(&self) -> Option<(PathBuf, u32, u32)> {
        let file = self.file.as_ref()?;
        Some((PathBuf::from(file), self.line.unwrap_or(1).max(1), self.column.unwrap_or(1).max(1)))
    }
}

static WITH_LOCATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:\d+>)?(?P<file>\S.*?)\((?P<line>\d+)(?:,(?P<col>\d+))?(?:,\d+,\d+)?\)\s*:\s*(?P<sev>fatal\s+error|error|warning)\s+(?P<code>[A-Za-z]+\d+)\s*:\s*(?P<msg>.*?)\s*(?:\[(?P<proj>[^\[\]]+\.[A-Za-z]+proj)\])?\s*$",
    )
    .unwrap()
});

static WITHOUT_LOCATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:\d+>)?(?P<origin>\S.*?)\s*:\s*(?P<sev>fatal\s+error|error|warning)\s+(?P<code>[A-Za-z]+\d+)\s*:\s*(?P<msg>.*?)\s*(?:\[(?P<proj>[^\[\]]+\.[A-Za-z]+proj)\])?\s*$",
    )
    .unwrap()
});

/// Parses one line of MSBuild console output.
pub fn parse_line(line: &str) -> Option<Diagnostic> {
    let severity = |s: &str| if s.ends_with("warning") { Severity::Warning } else { Severity::Error };
    if let Some(c) = WITH_LOCATION.captures(line) {
        return Some(Diagnostic {
            severity: severity(&c["sev"]),
            file: Some(c["file"].trim().to_string()),
            line: c["line"].parse().ok(),
            column: c.name("col").and_then(|m| m.as_str().parse().ok()),
            code: Some(c["code"].to_string()),
            message: c["msg"].to_string(),
            project: c.name("proj").map(|m| m.as_str().to_string()),
            origin: None,
        });
    }
    let c = WITHOUT_LOCATION.captures(line)?;
    let origin = c["origin"].trim().to_string();
    // "C:\x.cpp : error ..." names a file; "LINK : error ..." names a tool.
    let is_file = origin.contains(['\\', '/']) || Path::new(&origin).extension().is_some();
    Some(Diagnostic {
        severity: severity(&c["sev"]),
        file: is_file.then(|| origin.clone()),
        line: None,
        column: None,
        code: Some(c["code"].to_string()),
        message: c["msg"].to_string(),
        project: c.name("proj").map(|m| m.as_str().to_string()),
        origin: (!is_file).then_some(origin),
    })
}

/// What makes two diagnostics the same: severity, file key, line, column, code, message.
type DiagnosticKey = (Severity, String, Option<u32>, Option<u32>, Option<String>, String);

/// Collects diagnostics, dropping repeats (MSBuild prints each again in its summary).
#[derive(Clone, Debug, Default)]
pub struct Collector {
    seen: HashSet<DiagnosticKey>,
    pub list: Vec<Diagnostic>,
}

impl Collector {
    /// Adds a diagnostic; returns `false` for a repeat.
    pub fn add(&mut self, diagnostic: Diagnostic) -> bool {
        let file = diagnostic.file.as_deref().map(|f| paths::key(Path::new(f))).unwrap_or_default();
        let key = (
            diagnostic.severity,
            file,
            diagnostic.line,
            diagnostic.column,
            diagnostic.code.clone(),
            diagnostic.message.clone(),
        );
        if !self.seen.insert(key) {
            return false;
        }
        self.list.push(diagnostic);
        true
    }

    pub fn errors(&self) -> usize {
        self.list.iter().filter(|d| d.severity == Severity::Error).count()
    }

    pub fn warnings(&self) -> usize {
        self.list.iter().filter(|d| d.severity == Severity::Warning).count()
    }
}

/// Writes `errors.txt`: errors first, then warnings, one per line, MSVC format, UTF-8.
pub fn write_errors_file(path: &Path, diagnostics: &[Diagnostic]) -> Result<()> {
    let mut text = String::new();
    for severity in [Severity::Error, Severity::Warning] {
        for d in diagnostics.iter().filter(|d| d.severity == severity) {
            text.push_str(&d.to_msvc());
            text.push('\n');
        }
    }
    fsutil::write_atomic(path, text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compiler_linker_and_msbuild_lines() {
        let d = parse_line(r"  C:\src\dir with space\main.cpp(7,13): error C2065: 'y': 定義されていない識別子です。 [C:\src\App.vcxproj]").unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.file.as_deref(), Some(r"C:\src\dir with space\main.cpp"));
        assert_eq!((d.line, d.column), (Some(7), Some(13)));
        assert_eq!(d.code.as_deref(), Some("C2065"));
        assert_eq!(d.message, "'y': 定義されていない識別子です。");
        assert_eq!(d.project.as_deref(), Some(r"C:\src\App.vcxproj"));

        let d = parse_line(r"C:\x\a.h(3): warning C4100: 'p': unreferenced parameter").unwrap();
        assert_eq!((d.severity, d.line, d.column, d.project), (Severity::Warning, Some(3), None, None));

        let d = parse_line(r"LINK : fatal error LNK1104: cannot open file 'x.lib' [C:\p\App.vcxproj]").unwrap();
        assert_eq!((d.severity, d.file.clone(), d.origin.as_deref()), (Severity::Error, None, Some("LINK")));
        assert_eq!(d.to_msvc(), "LINK : error LNK1104: cannot open file 'x.lib'");

        let d = parse_line(r"C:\VS\Microsoft.CppBuild.targets(548,5): warning MSB8029: The Intermediate directory ... [C:\p\U (L).vcxproj]").unwrap();
        assert_eq!(d.code.as_deref(), Some("MSB8029"));
        assert_eq!(d.project.as_deref(), Some(r"C:\p\U (L).vcxproj"));

        let d = parse_line(r"C:\src\c.cpp : fatal error C1083: Cannot open include file: 'nope.h'").unwrap();
        assert_eq!((d.file.as_deref(), d.line), (Some(r"C:\src\c.cpp"), None));

        assert!(parse_line("  main.cpp").is_none());
        assert!(parse_line("Build succeeded.").is_none());
        assert!(parse_line("    0 Error(s)").is_none());
    }

    #[test]
    fn msvc_format_round_trips_through_the_parser() {
        let line = r"C:\src\main.cpp(7,13): error C2065: 'y': undeclared identifier";
        assert_eq!(parse_line(line).unwrap().to_msvc(), line);
    }

    #[test]
    fn vs_codes_ms_compile_matcher_reads_the_format() {
        // The `$msCompile` problem matcher of VS Code (src/vs/workbench/contrib/tasks/common/problemMatcher.ts).
        let matcher = Regex::new(
            r"^(?:\s*\d+>)?(\S.*)\((\d+|\d+,\d+|\d+,\d+,\d+,\d+)\)\s*:\s+((?:fatal +)?error|warning|info)\s+(\w+\d+)\s*:\s*(.*)$",
        )
        .unwrap();
        let d = parse_line(r"C:\src\dir with space\main.cpp(7,13): fatal error C1083: Cannot open include file: 'x.h'")
            .unwrap();
        let line = d.to_msvc();
        let caps = matcher.captures(&line).expect("errors.txt lines match $msCompile");
        assert_eq!(&caps[1], r"C:\src\dir with space\main.cpp");
        assert_eq!((&caps[2], &caps[3], &caps[4]), ("7,13", "error", "C1083"));
    }

    #[test]
    fn collector_drops_repeats() {
        let mut c = Collector::default();
        let d = parse_line(r"C:\a.cpp(1,1): warning C4100: x").unwrap();
        assert!(c.add(d.clone()));
        let mut same_file_other_case = d.clone();
        same_file_other_case.file = Some(r"c:\A.CPP".into());
        assert!(!c.add(same_file_other_case));
        assert!(c.add(parse_line(r"C:\a.cpp(2,1): error C2065: y").unwrap()));
        assert_eq!((c.errors(), c.warnings()), (1, 1));
    }
}
