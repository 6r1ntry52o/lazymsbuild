//! JSON Compilation Database entries (<https://clang.llvm.org/docs/JSONCompilationDatabase.html>).
//!
//! Inside lazymsbuild an entry always has absolute `directory` and `file` and a tokenized
//! `arguments` list. On output it is written as the specification recommends (`arguments`), or as
//! a `command` string for tools that only read that (`output.format = "command"`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{cmdline, paths};

/// One compile command: how `file` is compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Working directory of the compiler (absolute).
    pub directory: String,
    /// The source file (absolute).
    pub file: String,
    /// The compiler followed by its arguments; the source file is the last one.
    pub arguments: Vec<String>,
    /// The object file, when known.
    pub output: Option<String>,
}

impl Entry {
    /// The key entries are matched by: the normalized, lower-case source path.
    pub fn key(&self) -> String {
        paths::key(Path::new(&self.file))
    }

    pub fn file_path(&self) -> PathBuf {
        PathBuf::from(&self.file)
    }
}

/// How entries are written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// `"arguments": [...]`, the form the specification recommends.
    #[default]
    Arguments,
    /// `"command": "..."`, for tools that do not read `arguments`.
    Command,
}

#[derive(Deserialize)]
struct RawEntry {
    directory: String,
    file: String,
    #[serde(default)]
    arguments: Option<Vec<String>>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    output: Option<String>,
}

/// Parses a compilation database. Entries that only have `command` are tokenized; relative
/// `file` and `directory` values are made absolute. Entries without a command are skipped.
pub fn parse(json: &str) -> Result<Vec<Entry>> {
    let raw: Vec<RawEntry> = serde_json::from_str(json).context("not a JSON compilation database")?;
    let mut entries = Vec::with_capacity(raw.len());
    for item in raw {
        let arguments = match (item.arguments, item.command) {
            (Some(args), _) if !args.is_empty() => args,
            (_, Some(command)) => cmdline::split_args(&command),
            _ => continue,
        };
        if arguments.is_empty() {
            continue;
        }
        let directory = paths::normalize(Path::new(&item.directory));
        let file = paths::absolutize(&directory, Path::new(&item.file));
        entries.push(Entry {
            directory: directory.to_string_lossy().into_owned(),
            file: file.to_string_lossy().into_owned(),
            arguments,
            output: item.output,
        });
    }
    Ok(entries)
}

/// Reads a compilation database file.
pub fn read(path: &Path) -> Result<Vec<Entry>> {
    let text = crate::text::read_text(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse(&text).with_context(|| format!("cannot read {}", path.display()))
}

/// Writes entries as a compilation database: sorted by key, so the same entries always give the
/// same bytes, and one entry per block with the arguments on one line, so the file stays readable.
pub fn to_json(entries: &[Entry], format: Format) -> String {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by_cached_key(|e| e.key());
    let mut out = String::with_capacity(entries.len() * 512 + 4);
    out.push('[');
    for (i, entry) in sorted.iter().enumerate() {
        out.push_str(if i == 0 { "\n" } else { ",\n" });
        out.push_str("  {\n");
        let _ = writeln!(out, "    \"directory\": {},", json_string(&entry.directory));
        let _ = write!(out, "    \"file\": {}", json_string(&entry.file));
        match format {
            Format::Arguments => {
                out.push_str(",\n    \"arguments\": [");
                for (j, arg) in entry.arguments.iter().enumerate() {
                    if j > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&json_string(arg));
                }
                out.push(']');
            }
            Format::Command => {
                let _ = write!(out, ",\n    \"command\": {}", json_string(&cmdline::join_args(&entry.arguments)));
            }
        }
        if let Some(output) = &entry.output {
            let _ = write!(out, ",\n    \"output\": {}", json_string(output));
        }
        out.push_str("\n  }");
    }
    out.push_str(if sorted.is_empty() { "]\n" } else { "\n]\n" });
    out
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).expect("serializing a string cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str) -> Entry {
        Entry {
            directory: r"C:\repo\src".into(),
            file: file.into(),
            arguments: vec!["cl.exe".into(), "/DA=\"b c\"".into(), file.into()],
            output: None,
        }
    }

    #[test]
    fn writes_sorted_and_reads_back() {
        let entries = vec![entry(r"C:\repo\src\b.cpp"), entry(r"C:\repo\src\A.cpp")];
        let json = to_json(&entries, Format::Arguments);
        let back = parse(&json).unwrap();
        assert_eq!(back[0].file, r"C:\repo\src\A.cpp");
        assert_eq!(back[1], entries[0]);
        assert_eq!(json, to_json(&back, Format::Arguments), "output is deterministic");
    }

    #[test]
    fn command_format_round_trips() {
        let entries = vec![entry(r"C:\repo\src\with space.cpp")];
        let json = to_json(&entries, Format::Command);
        assert!(json.contains("\"command\""));
        assert_eq!(parse(&json).unwrap(), entries);
    }

    #[test]
    fn parse_resolves_relative_paths_and_skips_empty_commands() {
        let json = r#"[
          {"file": "empty.cpp", "directory": "C:\\x", "command": ""},
          {"directory": "C:\\repo\\src\\App", "file": "..\\Core\\core.cpp", "arguments": ["cl.exe", "core.cpp"]}
        ]"#;
        let entries = parse(json).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file, r"C:\repo\src\Core\core.cpp");
    }

    #[test]
    fn empty_database() {
        assert_eq!(to_json(&[], Format::Arguments), "[]\n");
        assert!(parse("[]").unwrap().is_empty());
    }
}
