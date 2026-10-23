//! Settings: built-in defaults, then the user's `config.toml`, then the repository's
//! `.lazymsbuild.toml` (more specific wins), then command-line options.
//!
//! The user file is looked for in `%LAZYMSBUILD_CONFIG_DIR%`, then `%XDG_CONFIG_HOME%\lazymsbuild`,
//! then `%APPDATA%\lazymsbuild`. Unknown keys are reported as warnings, never as errors, so a newer
//! configuration still loads in an older lazymsbuild. `docs/config.md` documents every key.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::compdb::Format;

/// The configuration schema version this lazymsbuild writes and understands.
pub const SCHEMA_VERSION: u32 = 1;

pub const USER_FILE: &str = "config.toml";
pub const REPO_FILE: &str = ".lazymsbuild.toml";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: Option<u32>,
    /// The solution to use, relative to the repository root (when a repository has several).
    pub solution: Option<String>,
    /// The configuration to start with when none was chosen yet (`Debug|x64`).
    pub profile: Option<String>,
    pub build: BuildConfig,
    pub extractor: ExtractorConfig,
    pub output: OutputConfig,
    pub init: InitConfig,
    pub clangd: ClangdConfig,
    pub logs: LogsConfig,
    /// Key bindings of the terminal UI: action name -> key or list of keys.
    pub keys: BTreeMap<String, Keys>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildConfig {
    /// MSBuild.exe to use. Empty: the newest Visual Studio with the C++ build tools (vswhere).
    pub msbuild: Option<PathBuf>,
    /// Build projects in parallel (`-m`).
    pub parallel: bool,
    /// Maximum number of MSBuild nodes (`-m:<n>`); 0 lets MSBuild decide.
    pub max_cpu_count: u32,
    /// Console verbosity of the build log (`quiet`, `minimal`, `normal`, `detailed`, `diagnostic`).
    pub verbosity: String,
    /// Arguments added to every MSBuild command line.
    pub extra_args: Vec<String>,
    /// Ask MSBuild and the compiler for English messages (`DOTNET_CLI_UI_LANGUAGE=en-US`, `VSLANG=1033`).
    pub english: bool,
    /// Keep a binary log (`-bl`) of each build next to its text log.
    pub keep_binlog: bool,
    /// Code page of build output that is not UTF-8. 0: the OEM code page, which MSBuild writes in.
    pub code_page: u32,
}

impl Default for BuildConfig {
    fn default() -> Self {
        BuildConfig {
            msbuild: None,
            parallel: true,
            max_cpu_count: 0,
            verbosity: "minimal".into(),
            extra_args: Vec::new(),
            english: true,
            keep_binlog: false,
            code_page: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtractorConfig {
    /// msbuild-extractor-sample.exe. Empty: PATH, then `<repo>\.tools`, then
    /// `%LOCALAPPDATA%\msbuild-extractor`. `"off"`: never run it (sync is unavailable).
    pub design_time: Option<String>,
    /// LazyMsBuild.Logger.dll. Empty: next to lazymsbuild.exe. `"off"`: builds do not update the database.
    pub logger: Option<String>,
}

/// Where `compile_commands.json` goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputMode {
    /// In the repository (`output.path`), where clangd, clang-tidy and editors find it themselves.
    #[default]
    RepoRoot,
    /// In lazymsbuild's state directory; the editor passes `--compile-commands-dir` to clangd.
    External,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub mode: OutputMode,
    /// The database path in repo-root mode, relative to the repository root.
    pub path: PathBuf,
    pub format: Format,
}

impl Default for OutputConfig {
    fn default() -> Self {
        OutputConfig {
            mode: OutputMode::RepoRoot,
            path: PathBuf::from("compile_commands.json"),
            format: Format::Arguments,
        }
    }
}

/// How `init` keeps generated files out of `git status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IgnoreMode {
    /// `.git/info/exclude`: local to the clone, changes no tracked file.
    #[default]
    Exclude,
    /// `.gitignore`: shared with the team (asks first).
    Gitignore,
    None,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InitConfig {
    pub ignore: IgnoreMode,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClangdConfig {
    /// clangd.exe used by `doctor`. Empty: PATH, Mason, Visual Studio's LLVM, the VS Code extension.
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LogsConfig {
    /// How many build logs to keep.
    pub keep: usize,
}

impl Default for LogsConfig {
    fn default() -> Self {
        LogsConfig { keep: 20 }
    }
}

/// One key or several.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Keys {
    One(String),
    Many(Vec<String>),
}

impl Keys {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            Keys::One(key) => vec![key.clone()],
            Keys::Many(keys) => keys.clone(),
        }
    }
}

/// A loaded configuration with what was found along the way.
#[derive(Clone, Debug, Default)]
pub struct Loaded {
    pub config: Config,
    /// Files that were read, in order.
    pub files: Vec<PathBuf>,
    /// Unknown keys and other non-fatal problems.
    pub warnings: Vec<String>,
}

/// Candidate directories for the user configuration, in the order they are searched.
pub fn user_config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("LAZYMSBUILD_CONFIG_DIR").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(dir).join("lazymsbuild"));
    }
    if let Some(dir) = dirs::config_dir() {
        dirs.push(dir.join("lazymsbuild"));
    }
    dirs
}

/// The user configuration directory: the first candidate that has a `config.toml`, otherwise the
/// first candidate.
pub fn user_config_dir() -> Option<PathBuf> {
    let candidates = user_config_dirs();
    candidates.iter().find(|d| d.join(USER_FILE).is_file()).cloned().or_else(|| candidates.into_iter().next())
}

/// Loads the user configuration and, when `repo_root` is given, the repository's.
pub fn load(repo_root: Option<&Path>) -> Result<Loaded> {
    let mut loaded = Loaded::default();
    let mut merged = toml::Table::new();
    let mut files = Vec::new();
    if let Some(dir) = user_config_dir() {
        files.push(dir.join(USER_FILE));
    }
    if let Some(root) = repo_root {
        files.push(root.join(REPO_FILE));
    }
    for file in files {
        if !file.is_file() {
            continue;
        }
        let text = crate::text::read_text(&file).with_context(|| format!("cannot read {}", file.display()))?;
        let table: toml::Table =
            toml::from_str(&text).with_context(|| format!("invalid TOML in {}", file.display()))?;
        check_keys(&table, &file, &mut loaded.warnings)?;
        merge(&mut merged, table);
        loaded.files.push(file);
    }
    loaded.config = merged.try_into().context("invalid configuration")?;
    if let Some(version) = loaded.config.schema_version.filter(|v| *v > SCHEMA_VERSION) {
        loaded.warnings.push(format!(
            "the configuration has schema_version {version}; this lazymsbuild knows version {SCHEMA_VERSION}, so newer settings are ignored"
        ));
    }
    Ok(loaded)
}

/// Parses one configuration text (for tests and `doctor`).
pub fn parse(text: &str) -> Result<(Config, Vec<String>)> {
    let table: toml::Table = toml::from_str(text)?;
    let mut warnings = Vec::new();
    check_keys(&table, Path::new("<text>"), &mut warnings)?;
    Ok((table.try_into()?, warnings))
}

fn check_keys(table: &toml::Table, file: &Path, warnings: &mut Vec<String>) -> Result<()> {
    let value = toml::Value::Table(table.clone());
    let mut unknown = Vec::new();
    let _: Config = serde_ignored::deserialize(value, |path| unknown.push(path.to_string()))
        .with_context(|| format!("invalid configuration in {}", file.display()))?;
    for key in unknown {
        warnings.push(format!("{}: unknown setting `{key}` (ignored)", file.display()));
    }
    Ok(())
}

/// Deep-merges `overlay` into `base`: tables merge, everything else is replaced.
fn merge(base: &mut toml::Table, overlay: toml::Table) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base_table)), toml::Value::Table(overlay_table)) => {
                merge(base_table, overlay_table)
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Where lazymsbuild keeps per-repository state: `%LAZYMSBUILD_STATE_DIR%` when set (tests,
/// portable setups), otherwise `%LOCALAPPDATA%\lazymsbuild`.
pub fn state_root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("LAZYMSBUILD_STATE_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::data_local_dir().map(|d| d.join("lazymsbuild")).context("cannot find %LOCALAPPDATA%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let (config, warnings) = parse("").unwrap();
        assert!(warnings.is_empty());
        assert!(config.build.parallel);
        assert!(config.build.english);
        assert_eq!(config.build.verbosity, "minimal");
        assert_eq!(config.output.mode, OutputMode::RepoRoot);
        assert_eq!(config.output.path, PathBuf::from("compile_commands.json"));
        assert_eq!(config.init.ignore, IgnoreMode::Exclude);
        assert_eq!(config.logs.keep, 20);
    }

    #[test]
    fn unknown_keys_are_warnings() {
        let (config, warnings) = parse(
            r#"
schema_version = 1
colour = "blue"
[build]
parallel = false
extra_args = ["-p:Foo=1"]
speed = 3
[output]
mode = "external"
format = "command"
[keys]
build = "B"
quit = ["q", "ctrl+q"]
"#,
        )
        .unwrap();
        assert!(!config.build.parallel);
        assert_eq!(config.build.extra_args, ["-p:Foo=1"]);
        assert_eq!(config.output.mode, OutputMode::External);
        assert_eq!(config.output.format, Format::Command);
        assert_eq!(config.keys["quit"].to_vec(), ["q", "ctrl+q"]);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("`colour`")));
        assert!(warnings.iter().any(|w| w.contains("`build.speed`")));
    }

    #[test]
    fn invalid_values_are_errors() {
        assert!(parse("[output]\nmode = \"elsewhere\"").is_err());
    }

    #[test]
    fn merge_is_deep() {
        let mut base: toml::Table = toml::from_str("[build]\nparallel = false\nverbosity = \"normal\"").unwrap();
        let overlay: toml::Table = toml::from_str("[build]\nverbosity = \"quiet\"").unwrap();
        merge(&mut base, overlay);
        let config: Config = base.try_into().unwrap();
        assert!(!config.build.parallel);
        assert_eq!(config.build.verbosity, "quiet");
    }
}
