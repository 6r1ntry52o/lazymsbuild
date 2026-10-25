//! Method A: Microsoft's msbuild-extractor-sample
//! (<https://github.com/microsoft/msbuild-extractor-sample>), which evaluates the projects at
//! design time and writes a compilation database without building.
//!
//! lazymsbuild finds it where the Neovim `:CompileCommands` command of dotfiles-windows does
//! (`PATH`, then `<repo>\.tools`, then `%LOCALAPPDATA%\msbuild-extractor`) and follows a
//! repository's `msbuild-extractor.json`, as described in `docs/design.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{Captured, Extraction, Method};
use crate::compdb::{self, Entry};
use crate::solution::{Profile, Solution};
use crate::vcxproj::ProjectFile;
use crate::{fsutil, paths, text};

pub const EXE_NAME: &str = "msbuild-extractor-sample.exe";
pub const CONFIG_FILE: &str = "msbuild-extractor.json";

/// Where the extractor was found.
#[derive(Clone, Debug)]
pub enum Lookup {
    Found {
        exe: PathBuf,
        origin: &'static str,
    },
    /// `extractor.design_time = "off"`.
    Disabled,
    /// Not found; the places that were searched.
    Missing(Vec<String>),
}

/// Finds the extractor: the configured path, `PATH`, `<root>\.tools`, `%LOCALAPPDATA%\msbuild-extractor`.
pub fn find(configured: Option<&str>, root: &Path) -> Lookup {
    match configured.map(str::trim) {
        Some("off") => return Lookup::Disabled,
        Some(path) if !path.is_empty() => {
            let path = PathBuf::from(path);
            return if path.is_file() {
                Lookup::Found { exe: paths::normalize(&path), origin: "configuration" }
            } else {
                Lookup::Missing(vec![path.display().to_string()])
            };
        }
        _ => {}
    }
    if let Some(exe) = fsutil::which(EXE_NAME) {
        return Lookup::Found { exe: paths::normalize(&exe), origin: "PATH" };
    }
    let tools = root.join(".tools").join(EXE_NAME);
    if tools.is_file() {
        return Lookup::Found { exe: paths::normalize(&tools), origin: ".tools" };
    }
    let mut searched = vec!["PATH".to_string(), tools.display().to_string()];
    if let Some(local) = dirs::data_local_dir() {
        let installed = local.join("msbuild-extractor").join(EXE_NAME);
        if installed.is_file() {
            return Lookup::Found { exe: paths::normalize(&installed), origin: "%LOCALAPPDATA%" };
        }
        searched.push(installed.display().to_string());
    }
    Lookup::Missing(searched)
}

/// A repository's `msbuild-extractor.json`, as far as lazymsbuild needs to understand it.
#[derive(Clone, Debug, Default)]
pub struct ConfigFile {
    pub path: PathBuf,
    pub configuration: Option<String>,
    pub platform: Option<String>,
    pub all_configurations: bool,
}

impl ConfigFile {
    /// Looks next to the solution, then at the repository root.
    pub fn find(solution_dir: &Path, root: &Path) -> Option<ConfigFile> {
        [solution_dir, root]
            .into_iter()
            .map(|d| d.join(CONFIG_FILE))
            .find(|p| p.is_file())
            .and_then(|p| ConfigFile::read(&p))
    }

    /// Reads the file with the extractor's leniency: keys in any case, `//` and `/* */` comments,
    /// trailing commas.
    pub fn read(path: &Path) -> Option<ConfigFile> {
        let content = text::read_text(path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&strip_json_extras(&content)).ok()?;
        let object = value.as_object()?;
        let get = |name: &str| object.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v);
        Some(ConfigFile {
            path: paths::normalize(path),
            configuration: get("configuration").and_then(|v| v.as_str()).map(str::to_string),
            platform: get("platform").and_then(|v| v.as_str()).map(str::to_string),
            all_configurations: get("allConfigurations").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    }

    /// The configuration the file declares, with the extractor's defaults (Debug, x64) for a half
    /// that is missing. `None` when it declares neither.
    pub fn declared_profile(&self) -> Option<Profile> {
        if self.configuration.is_none() && self.platform.is_none() {
            return None;
        }
        Some(Profile::new(
            self.configuration.clone().unwrap_or_else(|| "Debug".into()),
            self.platform.clone().unwrap_or_else(|| "x64".into()),
        ))
    }
}

/// Removes comments and trailing commas outside strings.
fn strip_json_extras(content: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match (c, chars.get(i + 1)) {
            ('"', _) => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            (',', _) => {
                let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
                if !matches!(next, Some('}' | ']')) {
                    out.push(c);
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// How to run the extractor for one configuration.
#[derive(Clone, Debug)]
pub struct Plan {
    pub args: Vec<String>,
    /// What the plan does differently from the plain case, for the log.
    pub notes: Vec<String>,
}

/// The extractor's arguments. With a `msbuild-extractor.json`, the file is passed with `--config`
/// (it can carry MSBuild properties and environment the repository needs); `-o` and
/// `-f standard` are always given, and `-c`/`-a` only when the profile differs from the file's.
pub fn plan(solution: &Path, profile: &Profile, config_file: Option<&ConfigFile>, output: &Path) -> Plan {
    let mut args = Vec::new();
    let mut notes = Vec::new();
    let mut pass_profile = true;
    match config_file {
        Some(file) if file.all_configurations => notes.push(format!(
            "{} sets allConfigurations, which mixes configurations in one list; running without it",
            file.path.display()
        )),
        Some(file) => {
            args.push("--config".to_string());
            args.push(file.path.display().to_string());
            match file.declared_profile() {
                Some(declared) if declared.eq_ignore_case(profile) => pass_profile = false,
                Some(declared) => notes.push(format!("{} declares {declared}; using {profile}", file.path.display())),
                None => {}
            }
        }
        None => {}
    }
    args.push("--solution".into());
    args.push(solution.display().to_string());
    if pass_profile {
        args.push("-c".into());
        args.push(profile.configuration.clone());
        args.push("-a".into());
        args.push(profile.platform.clone());
    }
    args.push("-f".into());
    args.push("standard".into());
    args.push("-o".into());
    args.push(output.display().to_string());
    Plan { args, notes }
}

/// Reads the extractor's output, without its marker entry (`"file": ".msbuild-extractor-sample"`).
pub fn read_output(path: &Path) -> Result<Vec<Entry>> {
    let content = text::read_text(path).with_context(|| format!("the extractor wrote no {}", path.display()))?;
    let mut entries = compdb::parse(&content)?;
    entries.retain(|e| {
        let name = Path::new(&e.file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        name != ".msbuild-extractor-sample" && !e.arguments.first().is_some_and(|a| a.starts_with("generated-by:"))
    });
    for entry in &mut entries {
        super::cl::disable_pch(&mut entry.arguments);
        super::cl::disable_warnings_as_errors(&mut entry.arguments);
    }
    Ok(entries)
}

/// Explains a known failure from the extractor's output.
pub fn explain_failure(output: &str) -> Option<String> {
    if output.contains("DllNotFoundException") || output.contains("hostfxr") {
        return Some(
            "the extractor could not load hostfxr.dll. It is a self-contained .NET program that needs \
             hostfxr.dll next to it when it cannot find Visual Studio's MSBuild: copy it from \
             %ProgramFiles%\\dotnet\\host\\fxr\\<version>\\ (scripts/install-extractor.ps1 does this)."
                .into(),
        );
    }
    None
}

/// Turns the extractor's entries into an [`Extraction`], assigning each entry to its project:
/// the project that lists the source and lives in the entry's directory, otherwise any project
/// that lists it, otherwise the project in that directory.
pub fn extraction(entries: Vec<Entry>, solution: &Solution, projects: &HashMap<String, ProjectFile>) -> Extraction {
    let mut out = Extraction::new(Method::DesignTime);
    let cpp: Vec<&crate::solution::Project> = solution.cpp_projects().collect();
    for entry in entries {
        let key = entry.key();
        let directory = paths::key(Path::new(&entry.directory));
        let lists = |p: &&crate::solution::Project| projects.get(&p.key()).is_some_and(|f| f.lists(&key));
        let in_directory = |p: &&crate::solution::Project| paths::key(p.dir()) == directory;
        let project = cpp
            .iter()
            .find(|p| lists(p) && in_directory(p))
            .or_else(|| cpp.iter().find(|p| lists(p)))
            .or_else(|| cpp.iter().find(|p| in_directory(p)))
            .map(|p| p.path.clone());
        out.entries.push(Captured { entry, project });
    }
    // The extractor evaluated every project, so every project's entries are complete and current.
    out.complete_projects = cpp.iter().map(|p| p.path.clone()).collect();
    out.current_projects = out.complete_projects.clone();
    out
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn lenient_json() {
        let text = r#"{
  // comment
  "Configuration": "Release", /* block */
  "platform": "Win32",
  "allConfigurations": false,
  "url": "http://x//y",
}"#;
        let value: serde_json::Value = serde_json::from_str(&strip_json_extras(text)).unwrap();
        assert_eq!(value["url"], "http://x//y");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE);
        std::fs::write(&path, text).unwrap();
        let file = ConfigFile::read(&path).unwrap();
        assert_eq!(file.declared_profile(), Some(Profile::new("Release", "Win32")));
    }

    #[test]
    fn plans() {
        let out = Path::new(r"C:\state\tmp\a.json");
        let sln = Path::new(r"C:\repo\A.sln");
        let debug = Profile::new("Debug", "x64");
        let p = plan(sln, &debug, None, out);
        assert_eq!(
            p.args,
            [
                "--solution",
                r"C:\repo\A.sln",
                "-c",
                "Debug",
                "-a",
                "x64",
                "-f",
                "standard",
                "-o",
                r"C:\state\tmp\a.json"
            ]
        );

        let file = ConfigFile {
            path: PathBuf::from(r"C:\repo\msbuild-extractor.json"),
            configuration: Some("Debug".into()),
            ..Default::default()
        };
        let p = plan(sln, &debug, Some(&file), out);
        assert_eq!(&p.args[..2], ["--config", r"C:\repo\msbuild-extractor.json"]);
        assert!(!p.args.contains(&"-c".to_string()), "the file declares Debug|x64 already");
        let p = plan(sln, &Profile::new("Release", "x64"), Some(&file), out);
        assert!(p.args.contains(&"-c".to_string()));
        assert_eq!(p.notes.len(), 1);

        let all = ConfigFile { all_configurations: true, ..file };
        let p = plan(sln, &debug, Some(&all), out);
        assert!(!p.args.contains(&"--config".to_string()));
    }

    #[test]
    fn drops_the_marker_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        std::fs::write(
            &path,
            r#"[{"file": ".msbuild-extractor-sample", "directory": "C:\\x", "command": "generated-by:msbuild-extractor-sample/1.0.0"},
                {"file": "C:\\r\\a.cpp", "directory": "C:\\r", "arguments": ["cl.exe", "C:\\r\\a.cpp"]}]"#,
        )
        .unwrap();
        let entries = read_output(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file, r"C:\r\a.cpp");
    }
}
