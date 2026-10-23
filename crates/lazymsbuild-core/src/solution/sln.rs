//! The text `.sln` format.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Result, bail};
use regex::Regex;

use super::{Folder, Profile, Project, ProjectConfig, Solution, SolutionFormat};
use crate::{paths, text};

pub(super) const SOLUTION_FOLDER: &str = "2150E333-8FDC-42A3-9474-1A3956D46DE8";
pub(super) const WEBSITE: &str = "E24C65DC-7377-472B-9ABA-BC803B73C61A";

static PROJECT_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^Project\("\{([^}]*)\}"\)\s*=\s*"([^"]*)"\s*,\s*"([^"]*)"\s*,\s*"([^"]*)""#).unwrap()
});

struct RawProject {
    type_guid: String,
    name: String,
    path: String,
    guid: String,
}

#[derive(Default)]
struct RawConfig {
    active: Option<String>,
    build: bool,
}

pub(super) fn parse(path: &Path, content: &str) -> Result<Solution> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut raw_projects = Vec::new();
    let mut profiles: Vec<Profile> = Vec::new();
    // Project GUID (upper case) -> solution configuration text -> mapping.
    let mut configs: HashMap<String, HashMap<String, RawConfig>> = HashMap::new();
    // Child GUID -> parent GUID (upper case).
    let mut nested: HashMap<String, String> = HashMap::new();
    let mut visual_studio_version = None;
    let mut section: Option<String> = None;
    let mut saw_header = false;

    for line in text::lines(content) {
        let line = line.trim();
        if line.starts_with("Microsoft Visual Studio Solution File") {
            saw_header = true;
            continue;
        }
        if let Some(name) = section.as_deref() {
            if line.starts_with("EndGlobalSection") {
                section = None;
                continue;
            }
            let Some((left, right)) = line.split_once('=') else { continue };
            let (left, right) = (left.trim(), right.trim());
            match name {
                "SolutionConfigurationPlatforms" => {
                    if let Some(profile) = Profile::parse(left)
                        && !profiles.iter().any(|p| p.eq_ignore_case(&profile))
                    {
                        profiles.push(profile);
                    }
                }
                "ProjectConfigurationPlatforms" => {
                    let Some((guid, rest)) = left.split_once('.') else { continue };
                    let guid = bare_guid(guid);
                    let entry = |suffix: &str| rest.strip_suffix(suffix).map(str::to_string);
                    if let Some(solution_config) = entry(".ActiveCfg") {
                        configs.entry(guid).or_default().entry(solution_config.to_lowercase()).or_default().active =
                            Some(right.to_string());
                    } else if let Some(solution_config) = entry(".Build.0") {
                        configs.entry(guid).or_default().entry(solution_config.to_lowercase()).or_default().build =
                            true;
                    }
                }
                "NestedProjects" => {
                    nested.insert(bare_guid(left), bare_guid(right));
                }
                _ => {}
            }
            continue;
        }
        if let Some(caps) = PROJECT_LINE.captures(line) {
            raw_projects.push(RawProject {
                type_guid: caps[1].to_ascii_uppercase(),
                name: caps[2].to_string(),
                path: caps[3].to_string(),
                guid: caps[4].to_string(),
            });
        } else if let Some(rest) = line.strip_prefix("GlobalSection(") {
            section = rest.split_once(')').map(|(name, _)| name.to_string());
        } else if let Some(rest) = line.strip_prefix("VisualStudioVersion") {
            visual_studio_version = rest.trim_start().strip_prefix('=').map(|v| v.trim().to_string());
        }
    }
    if !saw_header {
        bail!("not a Visual Studio solution file (no \"Microsoft Visual Studio Solution File\" header)");
    }

    let mut folders = Vec::new();
    let mut folder_by_guid: HashMap<String, usize> = HashMap::new();
    for raw in raw_projects.iter().filter(|r| r.type_guid == SOLUTION_FOLDER) {
        folder_by_guid.insert(bare_guid(&raw.guid), folders.len());
        folders.push(Folder { name: raw.name.clone(), parent: None });
    }
    for raw in raw_projects.iter().filter(|r| r.type_guid == SOLUTION_FOLDER) {
        let index = folder_by_guid[&bare_guid(&raw.guid)];
        folders[index].parent = nested.get(&bare_guid(&raw.guid)).and_then(|p| folder_by_guid.get(p)).copied();
    }

    let mut projects = Vec::new();
    for raw in raw_projects.iter().filter(|r| r.type_guid != SOLUTION_FOLDER) {
        let guid = bare_guid(&raw.guid);
        let is_file = !raw.path.contains("://");
        let path = if is_file { paths::absolutize(dir, Path::new(&raw.path)) } else { dir.join(&raw.name) };
        let is_cpp = is_file && raw.path.to_ascii_lowercase().ends_with(".vcxproj");
        let mut project_configs = BTreeMap::new();
        if let Some(by_solution_config) = configs.get(&guid) {
            for profile in &profiles {
                let Some(raw_config) = by_solution_config.get(&profile.to_string().to_lowercase()) else { continue };
                let Some(active) = raw_config.active.as_deref().and_then(Profile::parse) else { continue };
                project_configs.insert(profile.clone(), ProjectConfig { profile: active, build: raw_config.build });
            }
        }
        projects.push(Project {
            name: raw.name.clone(),
            path,
            guid: Some(raw.guid.clone()),
            is_cpp,
            folder: if raw.type_guid == WEBSITE {
                None
            } else {
                nested.get(&guid).and_then(|p| folder_by_guid.get(p)).copied()
            },
            target_name: String::new(),
            configs: project_configs,
        });
    }

    Ok(Solution {
        path: path.to_path_buf(),
        format: SolutionFormat::Sln,
        visual_studio_version,
        profiles,
        projects,
        folders,
    })
}

/// `{a1b2...}` -> `A1B2...`, for comparing GUIDs.
fn bare_guid(text: &str) -> String {
    text.trim().trim_start_matches('{').trim_end_matches('}').to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLN: &str = r#"
Microsoft Visual Studio Solution File, Format Version 12.00
# Visual Studio Version 17
VisualStudioVersion = 17.14.36429.23
MinimumVisualStudioVersion = 10.0.40219.1
Project("{8BC9CEB8-8B4A-11D0-8D11-00A0C91BC942}") = "App", "src\App\App.vcxproj", "{A1B2C3D4-0001-4000-8000-000000000001}"
EndProject
Project("{2150E333-8FDC-42A3-9474-1A3956D46DE8}") = "Libs", "Libs", "{F0000000-0001-4000-8000-000000000001}"
EndProject
Project("{8BC9CEB8-8B4A-11D0-8D11-00A0C91BC942}") = "MyApp.Core", "src\Core\MyApp.Core.vcxproj", "{a1b2c3d4-0002-4000-8000-000000000002}"
EndProject
Project("{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}") = "Tool", "tools\Tool.csproj", "{A1B2C3D4-0009-4000-8000-000000000009}"
EndProject
Global
	GlobalSection(SolutionConfigurationPlatforms) = preSolution
		Debug.Unicode|x64 = Debug.Unicode|x64
		Debug|x86 = Debug|x86
	EndGlobalSection
	GlobalSection(ProjectConfigurationPlatforms) = postSolution
		{A1B2C3D4-0001-4000-8000-000000000001}.Debug.Unicode|x64.ActiveCfg = Debug.Unicode|x64
		{A1B2C3D4-0001-4000-8000-000000000001}.Debug.Unicode|x64.Build.0 = Debug.Unicode|x64
		{A1B2C3D4-0001-4000-8000-000000000001}.Debug|x86.ActiveCfg = Debug|Win32
		{A1B2C3D4-0002-4000-8000-000000000002}.Debug|x86.ActiveCfg = Debug|Win32
		{A1B2C3D4-0002-4000-8000-000000000002}.Debug|x86.Build.0 = Debug|Win32
	EndGlobalSection
	GlobalSection(NestedProjects) = preSolution
		{A1B2C3D4-0002-4000-8000-000000000002} = {F0000000-0001-4000-8000-000000000001}
	EndGlobalSection
EndGlobal
"#;

    #[test]
    fn parses_projects_folders_and_configurations() {
        let s = parse(Path::new(r"C:\repo\Basic.sln"), SLN).unwrap();
        assert_eq!(s.visual_studio_version.as_deref(), Some("17.14.36429.23"));
        assert_eq!(s.profiles, [Profile::new("Debug.Unicode", "x64"), Profile::new("Debug", "x86")]);
        assert_eq!(s.folders.len(), 1);
        assert_eq!(s.projects.len(), 3);

        let app = &s.projects[0];
        assert!(app.is_cpp);
        assert_eq!(app.path, Path::new(r"C:\repo\src\App\App.vcxproj"));
        assert_eq!(app.folder, None);
        let x64 = app.config_for(&Profile::new("Debug.Unicode", "x64")).unwrap();
        assert_eq!((x64.profile.to_string(), x64.build), ("Debug.Unicode|x64".to_string(), true));
        let x86 = app.config_for(&Profile::new("Debug", "x86")).unwrap();
        assert_eq!((x86.profile.to_string(), x86.build), ("Debug|Win32".to_string(), false));

        let core = &s.projects[1];
        assert_eq!(core.folder, Some(0), "NestedProjects matches GUIDs in any case");
        assert!(core.config_for(&Profile::new("Debug.Unicode", "x64")).is_none());
        assert!(core.builds_in(&Profile::new("debug", "X86")));

        assert!(!s.projects[2].is_cpp);
    }

    #[test]
    fn rejects_other_files() {
        assert!(parse(Path::new(r"C:\x.sln"), "<Project />").is_err());
    }
}
