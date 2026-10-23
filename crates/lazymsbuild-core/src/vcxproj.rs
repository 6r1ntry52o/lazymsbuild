//! Project files (`.vcxproj`): which sources a configuration compiles, and which files can change
//! the compile commands.
//!
//! This is not an MSBuild evaluator. It reads the `ClCompile` items of the project and of the
//! files it imports directly from the repository (shared items projects, `.props`), and it
//! understands only the conditions Visual Studio writes for configurations:
//! `'$(Configuration)|$(Platform)'=='Debug|x64'`, `'$(Configuration)'=='Debug'`,
//! `'$(Platform)'=='x64'` (with `==` or `!=`). Any other condition counts as true: an item behind it
//! is counted, an `ExcludedFromBuild` behind it is not applied. Item paths that use properties other
//! than the few well-known ones below are reported as unresolved instead of guessed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;

use crate::solution::Profile;
use crate::{paths, text};

/// What a project file says about the sources it compiles.
#[derive(Clone, Debug)]
pub struct ProjectFile {
    pub path: PathBuf,
    /// `ProjectConfiguration` items (`Debug|Win32`, ...).
    pub configurations: Vec<Profile>,
    pub sources: Vec<Source>,
    /// Files whose change can change compile commands: the project file, the files it imports
    /// directly from the repository, and the `Directory.Build.props` / `.targets` MSBuild picks up.
    pub inputs: Vec<PathBuf>,
    /// `ClCompile` paths that could not be resolved (unknown properties, item transforms).
    pub unresolved: Vec<String>,
}

/// A `ClCompile` item.
#[derive(Clone, Debug)]
pub struct Source {
    pub path: PathBuf,
    pub key: String,
    conditions: Vec<Condition>,
    excluded: Vec<(Condition, bool)>,
}

/// Whether a configuration compiles a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceState {
    Built,
    /// `ExcludedFromBuild` is true for the configuration.
    Excluded,
    /// The item's condition is false for the configuration.
    NotInConfiguration,
}

impl Source {
    pub fn state(&self, profile: &Profile) -> SourceState {
        if self.conditions.iter().any(|c| c.holds(profile) == Some(false)) {
            return SourceState::NotInConfiguration;
        }
        // The last ExcludedFromBuild whose condition is known to hold decides.
        let excluded = self
            .excluded
            .iter()
            .rev()
            .find(|(condition, _)| condition.holds(profile) == Some(true))
            .is_some_and(|(_, value)| *value);
        if excluded { SourceState::Excluded } else { SourceState::Built }
    }
}

impl ProjectFile {
    /// Reads a project file. `solution_dir` resolves `$(SolutionDir)`; `repo_root` limits which
    /// imported files are followed.
    pub fn load(path: &Path, solution_dir: Option<&Path>, repo_root: Option<&Path>) -> Result<ProjectFile> {
        let path = paths::normalize(path);
        let project_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let content = text::read_text(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let doc = roxmltree::Document::parse_with_options(&content, xml_options())
            .with_context(|| format!("cannot parse {}", path.display()))?;

        let mut project = ProjectFile {
            path: path.clone(),
            configurations: Vec::new(),
            sources: Vec::new(),
            inputs: vec![path.clone()],
            unresolved: Vec::new(),
        };
        let context = Expander { project_dir: &project_dir, solution_dir };
        let boundary = repo_root.or(solution_dir).unwrap_or(&project_dir).to_path_buf();

        for node in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "ProjectConfiguration") {
            if let Some(profile) = node.attribute("Include").and_then(Profile::parse) {
                project.configurations.push(profile);
            }
        }

        project.read_items(&doc, &path, &context);

        // Direct imports from the repository: inputs, and their items (shared items projects).
        for import in imports(&doc, &path, &context) {
            if !import.is_file() || paths::relative_to(&import, &boundary).is_none() {
                continue;
            }
            push_unique_path(&mut project.inputs, &import);
            let Ok(imported_text) = text::read_text(&import) else { continue };
            let Ok(imported) = roxmltree::Document::parse_with_options(&imported_text, xml_options()) else { continue };
            project.read_items(&imported, &import, &context);
        }

        // Directory.Build.props / .targets, which MSBuild imports implicitly, and what they import.
        for name in ["Directory.Build.props", "Directory.Build.targets"] {
            let Some(found) = find_above(&project_dir, name) else { continue };
            push_unique_path(&mut project.inputs, &found);
            let Ok(found_text) = text::read_text(&found) else { continue };
            let Ok(found_doc) = roxmltree::Document::parse_with_options(&found_text, xml_options()) else { continue };
            for import in imports(&found_doc, &found, &context) {
                if import.is_file() {
                    push_unique_path(&mut project.inputs, &import);
                }
            }
        }
        Ok(project)
    }

    /// The sources a project configuration compiles.
    pub fn built_sources(&self, profile: &Profile) -> impl Iterator<Item = &Source> {
        self.sources.iter().filter(move |s| s.state(profile) == SourceState::Built)
    }

    /// Whether the project lists a source at all, in any configuration.
    pub fn lists(&self, key: &str) -> bool {
        self.sources.iter().any(|s| s.key == key)
    }

    fn read_items(&mut self, doc: &roxmltree::Document<'_>, file: &Path, context: &Expander<'_>) {
        let file_dir = file.parent().unwrap_or(Path::new("."));
        for group in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "ItemGroup") {
            let group_condition = Condition::parse(group.attribute("Condition").unwrap_or_default());
            for item in group.children().filter(|n| n.is_element() && n.tag_name().name() == "ClCompile") {
                let item_condition = Condition::parse(item.attribute("Condition").unwrap_or_default());
                let mut excluded: Vec<(Condition, bool)> = Vec::new();
                if let Some(value) = item.attribute("ExcludedFromBuild") {
                    excluded.push((Condition::Always, parse_bool(value)));
                }
                for meta in item.children().filter(|n| n.is_element() && n.tag_name().name() == "ExcludedFromBuild") {
                    let condition = Condition::parse(meta.attribute("Condition").unwrap_or_default());
                    excluded.push((condition, parse_bool(meta.text().unwrap_or_default())));
                }

                if let Some(include) = item.attribute("Include") {
                    let exclude: HashSet<String> = item
                        .attribute("Exclude")
                        .map(|e| self.resolve(e, file_dir, context).into_iter().map(|p| paths::key(&p)).collect())
                        .unwrap_or_default();
                    for path in self.resolve(include, file_dir, context) {
                        let key = paths::key(&path);
                        if exclude.contains(&key) {
                            continue;
                        }
                        self.sources.push(Source {
                            path,
                            key,
                            conditions: vec![group_condition.clone(), item_condition.clone()],
                            excluded: excluded.clone(),
                        });
                    }
                } else if let Some(remove) = item.attribute("Remove") {
                    let keys: HashSet<String> =
                        self.resolve(remove, file_dir, context).iter().map(|p| paths::key(p)).collect();
                    self.sources.retain(|s| !keys.contains(&s.key));
                } else if let Some(update) = item.attribute("Update") {
                    let keys: HashSet<String> =
                        self.resolve(update, file_dir, context).iter().map(|p| paths::key(p)).collect();
                    for source in self.sources.iter_mut().filter(|s| keys.contains(&s.key)) {
                        source.excluded.extend(excluded.iter().cloned());
                    }
                }
            }
        }
    }

    /// Resolves an item specification (`a.cpp;src\*.cpp`) to files. Unresolvable parts are recorded.
    fn resolve(&mut self, spec: &str, file_dir: &Path, context: &Expander<'_>) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for part in spec.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let Some(expanded) = context.expand(part, file_dir) else {
                self.unresolved.push(part.to_string());
                continue;
            };
            // Relative item paths are relative to the project, even in imported files.
            let full = paths::absolutize(context.project_dir, Path::new(&expanded));
            if expanded.contains(['*', '?']) {
                out.extend(glob_files(&full));
            } else {
                out.push(full);
            }
        }
        out
    }
}

fn xml_options<'input>() -> roxmltree::ParsingOptions<'input> {
    roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }
}

fn parse_bool(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("true")
}

fn push_unique_path(list: &mut Vec<PathBuf>, path: &Path) {
    if !list.iter().any(|p| paths::same(p, path)) {
        list.push(paths::normalize(path));
    }
}

/// Files a document imports with `<Import Project="...">`, when the path can be resolved.
fn imports(doc: &roxmltree::Document<'_>, file: &Path, context: &Expander<'_>) -> Vec<PathBuf> {
    let file_dir = file.parent().unwrap_or(Path::new("."));
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "Import")
        .filter_map(|n| n.attribute("Project"))
        .filter_map(|spec| context.expand(spec.trim(), file_dir))
        .filter(|spec| !spec.contains(['*', '?']))
        // An import path is relative to the file that has the <Import>.
        .map(|spec| paths::absolutize(file_dir, Path::new(&spec)))
        .collect()
}

/// The first `name` in `dir` or a directory above it, as MSBuild searches for Directory.Build.props.
fn find_above(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut current = Some(dir);
    while let Some(d) = current {
        let candidate = d.join(name);
        if candidate.is_file() {
            return Some(paths::normalize(&candidate));
        }
        current = d.parent();
    }
    None
}

fn glob_files(pattern: &Path) -> Vec<PathBuf> {
    let options = glob::MatchOptions {
        case_sensitive: false,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    let Ok(matches) = glob::glob_with(&pattern.to_string_lossy(), options) else { return Vec::new() };
    let mut files: Vec<PathBuf> =
        matches.filter_map(|m| m.ok()).filter(|p| p.is_file()).map(|p| paths::normalize(&p)).collect();
    files.sort();
    files
}

struct Expander<'a> {
    project_dir: &'a Path,
    solution_dir: Option<&'a Path>,
}

static PROPERTY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$\(([A-Za-z_][A-Za-z0-9_.-]*)\)").unwrap());

impl Expander<'_> {
    /// Expands the properties lazymsbuild knows without evaluating MSBuild. `None` if anything else
    /// remains (other properties, `@()` item lists, `%()` metadata, property functions).
    fn expand(&self, spec: &str, file_dir: &Path) -> Option<String> {
        let with_sep = |p: &Path| format!("{}\\", p.display());
        let mut failed = false;
        let expanded =
            PROPERTY.replace_all(spec, |caps: &regex::Captures<'_>| match caps[1].to_ascii_lowercase().as_str() {
                "msbuildthisfiledirectory" => with_sep(file_dir),
                "msbuildprojectdirectory" => self.project_dir.display().to_string(),
                "projectdir" => with_sep(self.project_dir),
                "solutiondir" => match self.solution_dir {
                    Some(dir) => with_sep(dir),
                    None => {
                        failed = true;
                        String::new()
                    }
                },
                _ => {
                    failed = true;
                    String::new()
                }
            });
        if failed || expanded.contains("$(") || expanded.contains("@(") || expanded.contains("%(") {
            return None;
        }
        Some(expanded.into_owned())
    }
}

/// A condition on an item or on its metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Condition {
    Always,
    Config { configuration: Option<String>, platform: Option<String>, equal: bool },
    Unknown,
}

static SIMPLE_CONDITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*'\s*(?P<lhs>[^']*?)\s*'\s*(?P<op>==|!=)\s*'\s*(?P<rhs>[^']*?)\s*'\s*$"#).unwrap()
});

impl Condition {
    fn parse(text: &str) -> Condition {
        if text.trim().is_empty() {
            return Condition::Always;
        }
        let Some(caps) = SIMPLE_CONDITION.captures(text) else { return Condition::Unknown };
        let lhs: String = caps["lhs"].chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        let rhs = caps["rhs"].trim();
        let equal = &caps["op"] == "==";
        match lhs.as_str() {
            "$(configuration)|$(platform)" => match rhs.split_once('|') {
                Some((c, p)) => {
                    Condition::Config { configuration: Some(c.trim().into()), platform: Some(p.trim().into()), equal }
                }
                None => Condition::Unknown,
            },
            "$(configuration)" => Condition::Config { configuration: Some(rhs.into()), platform: None, equal },
            "$(platform)" => Condition::Config { configuration: None, platform: Some(rhs.into()), equal },
            _ => Condition::Unknown,
        }
    }

    /// `Some(result)` when the condition can be decided for the configuration, `None` when not.
    fn holds(&self, profile: &Profile) -> Option<bool> {
        match self {
            Condition::Always => Some(true),
            Condition::Unknown => None,
            Condition::Config { configuration, platform, equal } => {
                let matches = configuration.as_ref().is_none_or(|c| c.eq_ignore_ascii_case(&profile.configuration))
                    && platform.as_ref().is_none_or(|p| p.eq_ignore_ascii_case(&profile.platform));
                Some(matches == *equal)
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::fs;

    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn conditions() {
        let debug_x64 = Profile::new("Debug", "x64");
        let release_x64 = Profile::new("Release", "x64");
        let c = Condition::parse("'$(Configuration)|$(Platform)'=='Debug|x64'");
        assert_eq!((c.holds(&debug_x64), c.holds(&release_x64)), (Some(true), Some(false)));
        let c = Condition::parse(" '$(Configuration)' != 'debug' ");
        assert_eq!((c.holds(&debug_x64), c.holds(&release_x64)), (Some(false), Some(true)));
        let c = Condition::parse("'$( Platform )'=='X64'");
        assert_eq!(c.holds(&release_x64), Some(true));
        assert_eq!(Condition::parse("exists('a.props')").holds(&debug_x64), None);
        assert_eq!(Condition::parse("'$(Configuration)'=='Debug' and '$(X)'=='1'").holds(&debug_x64), None);
        assert_eq!(Condition::parse("").holds(&debug_x64), Some(true));
    }

    #[test]
    fn reads_items_exclusions_imports_and_wildcards() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("Directory.Build.props"),
            r#"<Project><Import Project="$(MSBuildThisFileDirectory)build\common.props" /></Project>"#,
        );
        write(&root.join(r"build\common.props"), "<Project />");
        write(
            &root.join(r"shared\Shared.vcxitems"),
            r#"<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
  <ItemGroup><ClCompile Include="$(MSBuildThisFileDirectory)shared.cpp" /></ItemGroup>
</Project>"#,
        );
        for f in ["a.cpp", "b.cpp", r"gen\g1.cpp", r"gen\g2.cpp", r"gen\skip.cpp", r"shared\shared.cpp"] {
            write(&root.join(r"proj").join(f), "");
        }
        write(&root.join(r"shared\shared.cpp"), "");
        let project_path = root.join(r"proj\P.vcxproj");
        write(
            &project_path,
            r#"<?xml version="1.0" encoding="utf-8"?>
<Project DefaultTargets="Build" xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
  <ItemGroup Label="ProjectConfigurations">
    <ProjectConfiguration Include="Debug|x64" />
    <ProjectConfiguration Include="Release|x64" />
  </ItemGroup>
  <Import Project="$(VCTargetsPath)\Microsoft.Cpp.Default.props" />
  <Import Project="..\shared\Shared.vcxitems" Label="Shared" />
  <ItemGroup>
    <ClCompile Include="a.cpp" />
    <ClCompile Include="b.cpp">
      <ExcludedFromBuild Condition="'$(Configuration)|$(Platform)'=='Release|x64'">true</ExcludedFromBuild>
      <ExcludedFromBuild Condition="exists('nothing')">true</ExcludedFromBuild>
    </ClCompile>
    <ClCompile Include="gen\*.cpp" Exclude="gen\skip.cpp" />
    <ClCompile Include="$(GeneratedDir)x.cpp" />
  </ItemGroup>
  <ItemGroup Condition="'$(Configuration)'=='Debug'">
    <ClCompile Include="debug_only.cpp" />
  </ItemGroup>
  <ItemGroup>
    <ClCompile Remove="gen\g2.cpp" />
    <ClCompile Update="a.cpp" ExcludedFromBuild="false" />
  </ItemGroup>
</Project>"#,
        );

        let p = ProjectFile::load(&project_path, Some(root), Some(root)).unwrap();
        assert_eq!(p.configurations, [Profile::new("Debug", "x64"), Profile::new("Release", "x64")]);
        let names = |profile: &Profile| -> Vec<String> {
            let mut v: Vec<String> = p
                .built_sources(profile)
                .map(|s| paths::relative_to(&s.path, root).unwrap().display().to_string())
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            names(&Profile::new("Debug", "x64")),
            [r"proj\a.cpp", r"proj\b.cpp", r"proj\debug_only.cpp", r"proj\gen\g1.cpp", r"shared\shared.cpp"]
        );
        assert_eq!(names(&Profile::new("Release", "x64")), [r"proj\a.cpp", r"proj\gen\g1.cpp", r"shared\shared.cpp"]);
        assert_eq!(p.unresolved, ["$(GeneratedDir)x.cpp"]);

        let inputs: Vec<String> =
            p.inputs.iter().map(|i| paths::relative_to(i, root).unwrap().display().to_string()).collect();
        assert_eq!(
            inputs,
            [r"proj\P.vcxproj", r"shared\Shared.vcxitems", "Directory.Build.props", r"build\common.props"]
        );
    }
}
