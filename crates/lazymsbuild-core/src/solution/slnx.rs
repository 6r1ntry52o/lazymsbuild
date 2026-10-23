//! The XML `.slnx` format.
//!
//! A `.slnx` does not list project configurations. MSBuild derives them with the rules of
//! Microsoft.VisualStudio.SolutionPersistence, reproduced here: built-in rules, then the project
//! type's rules, then the project's own `<BuildType>`, `<Platform>`, `<Build>` and `<Deploy>`
//! elements; the last rule that applies wins.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Result, bail};

use super::{Folder, Profile, Project, ProjectConfig, Solution, SolutionFormat};
use crate::paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dimension {
    BuildType,
    Platform,
    Build,
    Deploy,
}

/// Maps a solution configuration to a project value. Empty build type or platform means "all".
#[derive(Clone, Debug)]
struct Rule {
    dimension: Dimension,
    build_type: String,
    platform: String,
    value: String,
}

impl Rule {
    fn new(dimension: Dimension, build_type: &str, platform: &str, value: &str) -> Rule {
        Rule { dimension, build_type: build_type.into(), platform: platform.into(), value: value.into() }
    }
}

/// The rules every project starts with: the solution's build type and platform, built, not deployed.
fn base_rules() -> Vec<Rule> {
    vec![
        Rule::new(Dimension::BuildType, "", "", "*"),
        Rule::new(Dimension::Platform, "", "", "*"),
        Rule::new(Dimension::Build, "", "", "True"),
        Rule::new(Dimension::Deploy, "", "", "False"),
    ]
}

/// Rules of the built-in project types, chosen by the project file's extension.
fn type_rules(extension: &str) -> Vec<Rule> {
    match extension {
        "vcxproj" => vec![
            Rule::new(Dimension::Platform, "", "AnyCPU", "x64"),
            Rule::new(Dimension::Platform, "", "x86", "Win32"),
        ],
        "csproj" | "vbproj" | "fsproj" | "webproj" => vec![Rule::new(Dimension::Platform, "", "", "AnyCPU")],
        "rptproj" => vec![Rule::new(Dimension::Platform, "", "", "?")],
        "shproj" | "vcxitems" | "exe" | "esproj" | "njsproj" | "deployproj" | "vsproj" | "wixproj" | "sqlproj"
        | "usqldbproj" | "usqlproj" | "sfproj" | "ccproj" | "dcproj" => {
            vec![Rule::new(Dimension::Build, "", "", "False")]
        }
        _ => Vec::new(),
    }
}

/// "Any CPU" and "AnyCPU" are the same platform.
fn canonical_platform(platform: &str) -> &str {
    if platform.eq_ignore_ascii_case("Any CPU") { "AnyCPU" } else { platform }
}

fn rule_applies(rule: &Rule, dimension: Dimension, build_type: &str, platform: &str) -> bool {
    if rule.dimension != dimension {
        return false;
    }
    let all_build_types = rule.build_type.is_empty();
    let all_platforms = rule.platform.is_empty();
    let same_build_type = || rule.build_type.eq_ignore_ascii_case(build_type);
    let same_platform = || canonical_platform(&rule.platform).eq_ignore_ascii_case(canonical_platform(platform));
    if all_build_types && all_platforms {
        // "*" takes the solution's value, so it needs one.
        return rule.value != "*"
            || ((dimension != Dimension::BuildType || !build_type.is_empty())
                && (dimension != Dimension::Platform || !platform.is_empty()));
    }
    if all_platforms && !build_type.is_empty() {
        return same_build_type();
    }
    if all_build_types && !platform.is_empty() {
        return same_platform();
    }
    !build_type.is_empty() && !platform.is_empty() && same_build_type() && same_platform()
}

fn value<'r>(rules: &'r [Rule], dimension: Dimension, build_type: &str, platform: &str) -> Option<&'r str> {
    rules.iter().rev().find(|r| rule_applies(r, dimension, build_type, platform)).map(|r| r.value.as_str())
}

fn project_config(rules: &[Rule], solution: &Profile) -> Option<ProjectConfig> {
    let (build_type, platform) = (solution.configuration.as_str(), solution.platform.as_str());
    let configuration = match value(rules, Dimension::BuildType, build_type, platform)? {
        "*" => build_type,
        other => other,
    };
    let project_platform = match value(rules, Dimension::Platform, build_type, platform)? {
        "*" => platform,
        "?" => return None,
        other => other,
    };
    if configuration.is_empty() || project_platform.is_empty() {
        return None;
    }
    let build = value(rules, Dimension::Build, build_type, platform).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    Some(ProjectConfig { profile: Profile::new(configuration, project_platform), build })
}

pub(super) fn parse(path: &Path, content: &str) -> Result<Solution> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let doc = roxmltree::Document::parse(content)?;
    let root = doc.root_element();
    if root.tag_name().name() != "Solution" {
        bail!("the root element is <{}>, not <Solution>", root.tag_name().name());
    }

    let mut build_types: Vec<String> = Vec::new();
    let mut platforms: Vec<String> = Vec::new();
    let mut folders: Vec<Folder> = Vec::new();
    let mut folder_by_path: HashMap<String, usize> = HashMap::new();
    let mut projects = Vec::new();

    for child in root.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "Configurations" => {
                for item in child.children().filter(|n| n.is_element()) {
                    let Some(name) = item.attribute("Name").map(str::trim).filter(|n| !n.is_empty()) else { continue };
                    match item.tag_name().name() {
                        "BuildType" => push_unique(&mut build_types, name),
                        "Platform" => push_unique(&mut platforms, name),
                        _ => {}
                    }
                }
            }
            "Folder" => {
                let folder = child.attribute("Name").map(|name| ensure_folder(name, &mut folders, &mut folder_by_path));
                for item in child.children().filter(|n| n.is_element() && n.tag_name().name() == "Project") {
                    projects.push(read_project(item, dir, folder.flatten()));
                }
            }
            "Project" => projects.push(read_project(child, dir, None)),
            _ => {}
        }
    }

    if build_types.is_empty() {
        build_types = vec!["Debug".into(), "Release".into()];
    }
    if platforms.is_empty() {
        platforms = vec!["Any CPU".into()];
    }
    let profiles: Vec<Profile> =
        build_types.iter().flat_map(|b| platforms.iter().map(move |p| Profile::new(b.clone(), p.clone()))).collect();

    let projects = projects
        .into_iter()
        .map(|(mut project, rules)| {
            project.configs = profiles
                .iter()
                .filter_map(|profile| project_config(&rules, profile).map(|c| (profile.clone(), c)))
                .collect::<BTreeMap<_, _>>();
            project
        })
        .collect();

    Ok(Solution {
        path: path.to_path_buf(),
        format: SolutionFormat::Slnx,
        visual_studio_version: None,
        profiles,
        projects,
        folders,
    })
}

fn push_unique(list: &mut Vec<String>, name: &str) {
    if !list.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        list.push(name.to_string());
    }
}

/// Registers the folder `/a/b/` (and `/a/`), returning the index of the innermost one.
fn ensure_folder(name: &str, folders: &mut Vec<Folder>, by_path: &mut HashMap<String, usize>) -> Option<usize> {
    let mut parent = None;
    let mut path = String::from("/");
    for segment in name.split(['/', '\\']).map(str::trim).filter(|s| !s.is_empty()) {
        path.push_str(&segment.to_lowercase());
        path.push('/');
        let index = match by_path.get(&path) {
            Some(&index) => index,
            None => {
                folders.push(Folder { name: segment.to_string(), parent });
                by_path.insert(path.clone(), folders.len() - 1);
                folders.len() - 1
            }
        };
        parent = Some(index);
    }
    parent
}

fn read_project(node: roxmltree::Node<'_, '_>, dir: &Path, folder: Option<usize>) -> (Project, Vec<Rule>) {
    let raw_path = node.attribute("Path").unwrap_or_default();
    let path = paths::absolutize(dir, Path::new(&raw_path.replace('/', "\\")));
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let name = node
        .attribute("DisplayName")
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());

    let mut rules = base_rules();
    rules.extend(type_rules(&extension));
    for item in node.children().filter(|n| n.is_element()) {
        let dimension = match item.tag_name().name() {
            "BuildType" => Dimension::BuildType,
            "Platform" => Dimension::Platform,
            "Build" => Dimension::Build,
            "Deploy" => Dimension::Deploy,
            _ => continue,
        };
        let project_value = item.attribute("Project").map(str::trim).unwrap_or_default();
        let project_value = match (project_value.is_empty(), dimension) {
            (true, Dimension::Build | Dimension::Deploy) => "True",
            (true, _) => continue,
            (false, _) => project_value,
        };
        let solution = item.attribute("Solution").map(str::trim).unwrap_or_default();
        let (build_type, platform) = if solution.is_empty() {
            ("", "")
        } else {
            match solution.split_once('|').map(|(b, p)| (b.trim(), p.trim())) {
                Some((b, p)) if !b.is_empty() && !p.is_empty() => {
                    (if b == "*" { "" } else { b }, if p == "*" { "" } else { p })
                }
                _ => continue,
            }
        };
        rules.push(Rule::new(dimension, build_type, platform, project_value));
    }

    let project = Project {
        name,
        path,
        guid: node.attribute("Id").map(|id| format!("{{{}}}", id.trim_matches(['{', '}']))),
        is_cpp: extension == "vcxproj",
        folder,
        target_name: String::new(),
        configs: BTreeMap::new(),
    };
    (project, rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLNX: &str = r#"<Solution>
  <Configurations>
    <BuildType Name="Debug" />
    <BuildType Name="Release" />
    <Platform Name="x64" />
    <Platform Name="x86" />
  </Configurations>
  <Folder Name="/Libs/">
    <Project Path="src/Core/MyApp.Core.vcxproj" Id="a1b2c3d4-0002-4000-8000-000000000002" />
  </Folder>
  <Folder Name="/Libs/Native.Ext/">
    <Project Path="src/Util/Util (Legacy).vcxproj">
      <Build Solution="Release|x64" Project="false" />
    </Project>
  </Folder>
  <Project Path="src/App/App.vcxproj">
    <BuildType Solution="Release|*" Project="Debug" />
  </Project>
  <Project Path="tools/Tool.csproj" />
</Solution>"#;

    #[test]
    fn maps_configurations_like_msbuild() {
        let s = parse(Path::new(r"C:\repo\Basic.slnx"), SLNX).unwrap();
        assert_eq!(
            s.profiles.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            ["Debug|x64", "Debug|x86", "Release|x64", "Release|x86"]
        );
        let names: Vec<&str> = s.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Libs", "Native.Ext"]);
        assert_eq!(s.folders[1].parent, Some(0));

        let core = &s.projects[0];
        assert_eq!(core.folder, Some(0));
        assert_eq!(core.path, Path::new(r"C:\repo\src\Core\MyApp.Core.vcxproj"));
        let c = core.config_for(&Profile::new("Debug", "x86")).unwrap();
        assert_eq!((c.profile.to_string(), c.build), ("Debug|Win32".to_string(), true), "x86 maps to Win32");

        let util = &s.projects[1];
        assert_eq!(util.name, "Util (Legacy)");
        assert_eq!(util.folder, Some(1));
        assert!(!util.builds_in(&Profile::new("Release", "x64")));
        assert!(util.builds_in(&Profile::new("Release", "x86")));

        let app = &s.projects[2];
        assert_eq!(app.config_for(&Profile::new("Release", "x64")).unwrap().profile, Profile::new("Debug", "x64"));

        let tool = &s.projects[3];
        assert!(!tool.is_cpp);
        assert_eq!(tool.config_for(&Profile::new("Debug", "x64")).unwrap().profile, Profile::new("Debug", "AnyCPU"));
    }

    #[test]
    fn any_cpu_maps_to_x64_for_cpp() {
        let s = parse(Path::new(r"C:\r\a.slnx"), r#"<Solution><Project Path="a.vcxproj" /></Solution>"#).unwrap();
        assert_eq!(s.profiles.iter().map(|p| p.to_string()).collect::<Vec<_>>(), ["Debug|Any CPU", "Release|Any CPU"]);
        let c = s.projects[0].config_for(&Profile::new("Debug", "Any CPU")).unwrap();
        assert_eq!(c.profile, Profile::new("Debug", "x64"));
        assert!(c.build);
    }

    #[test]
    fn the_last_applicable_rule_wins() {
        let rules = vec![
            Rule::new(Dimension::Build, "", "", "True"),
            Rule::new(Dimension::Build, "Release", "", "false"),
            Rule::new(Dimension::Build, "", "x64", "True"),
        ];
        assert_eq!(value(&rules, Dimension::Build, "Release", "x86"), Some("false"));
        assert_eq!(value(&rules, Dimension::Build, "Release", "x64"), Some("True"));
        assert_eq!(value(&rules, Dimension::Build, "Debug", "x86"), Some("True"));
    }
}
