//! MSBuild target names of solution projects (`-t:"Libs\MyApp_Core:Rebuild"`).
//!
//! The rules follow MSBuild's `ProjectInSolution.GetUniqueProjectName` and the collision handling in
//! `SolutionFile`, checked against the metaproject MSBuild writes with `MSBuildEmitSolution=1`:
//! - a project in a solution folder is named `Folder\Sub\Project`;
//! - `. % $ @ ; ( ) '` become `_`; spaces stay;
//! - when two names collide, the project whose name was changed by the rules above gets
//!   `_<GUID without braces>` appended.

use std::collections::HashMap;

use super::Solution;

const CLEANSED: [char; 8] = ['%', '$', '@', ';', '.', '(', ')', '\''];

fn cleanse(name: &str) -> String {
    name.chars().map(|c| if CLEANSED.contains(&c) { '_' } else { c }).collect()
}

pub(super) fn assign_target_names(solution: &mut Solution) {
    let mut folder_names: Vec<Option<String>> = vec![None; solution.folders.len()];
    for index in 0..solution.folders.len() {
        folder_name(solution, index, &mut folder_names, 0);
    }

    let mut taken: HashMap<String, usize> = HashMap::new();
    for i in 0..solution.projects.len() {
        let project = &solution.projects[i];
        let mut unique = match project.folder.and_then(|f| folder_names.get(f).cloned().flatten()) {
            Some(folder) => format!("{folder}\\{}", cleanse(&project.name)),
            None => cleanse(&project.name),
        };
        if let Some(&previous) = taken.get(&unique.to_lowercase()) {
            if unique != solution.projects[i].name {
                unique = format!("{unique}_{}", bare_guid(&solution.projects[i]));
            } else if unique != solution.projects[previous].name {
                let renamed = format!("{unique}_{}", bare_guid(&solution.projects[previous]));
                taken.remove(&unique.to_lowercase());
                taken.insert(renamed.to_lowercase(), previous);
                solution.projects[previous].target_name = renamed;
            }
        }
        taken.entry(unique.to_lowercase()).or_insert(i);
        solution.projects[i].target_name = unique;
    }
}

fn folder_name(solution: &Solution, index: usize, names: &mut Vec<Option<String>>, depth: usize) -> String {
    if let Some(name) = &names[index] {
        return name.clone();
    }
    let folder = &solution.folders[index];
    let own = cleanse(&folder.name);
    let name = match folder.parent {
        Some(parent) if depth < solution.folders.len() => {
            format!("{}\\{own}", folder_name(solution, parent, names, depth + 1))
        }
        _ => own,
    };
    names[index] = Some(name.clone());
    name
}

fn bare_guid(project: &super::Project) -> String {
    project.guid.as_deref().unwrap_or_default().trim_matches(['{', '}']).to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::super::{Folder, Project, SolutionFormat};
    use super::*;

    fn project(name: &str, guid: &str, folder: Option<usize>) -> Project {
        Project {
            name: name.into(),
            path: PathBuf::from(format!(r"C:\r\{name}.vcxproj")),
            guid: Some(format!("{{{guid}}}")),
            is_cpp: true,
            folder,
            target_name: String::new(),
            configs: BTreeMap::new(),
        }
    }

    fn solution(projects: Vec<Project>, folders: Vec<Folder>) -> Solution {
        let mut s = Solution {
            path: PathBuf::from(r"C:\r\a.sln"),
            format: SolutionFormat::Sln,
            visual_studio_version: None,
            profiles: Vec::new(),
            projects,
            folders,
        };
        assign_target_names(&mut s);
        s
    }

    #[test]
    fn folders_prefix_and_characters_are_cleansed() {
        let folders =
            vec![Folder { name: "Libs".into(), parent: None }, Folder { name: "Native.Ext".into(), parent: Some(0) }];
        let s = solution(
            vec![
                project("App", "1", None),
                project("MyApp.Core", "2", Some(0)),
                project("Util (Legacy)", "3", Some(1)),
            ],
            folders,
        );
        let names: Vec<&str> = s.projects.iter().map(|p| p.target_name.as_str()).collect();
        assert_eq!(names, ["App", r"Libs\MyApp_Core", r"Libs\Native_Ext\Util _Legacy_"]);
    }

    #[test]
    fn collisions_rename_the_cleansed_project() {
        // MSBuild 17.14 metaproject for this order: My_Lib_C0000000-...-0001 and My_Lib.
        let s = solution(
            vec![
                project("My.Lib", "C0000000-0000-4000-8000-000000000001", None),
                project("My_Lib", "C0000000-0000-4000-8000-000000000002", None),
            ],
            Vec::new(),
        );
        assert_eq!(s.projects[0].target_name, "My_Lib_C0000000-0000-4000-8000-000000000001");
        assert_eq!(s.projects[1].target_name, "My_Lib");

        let s = solution(vec![project("My_Lib", "A", None), project("My.Lib", "B", None)], Vec::new());
        assert_eq!(s.projects[0].target_name, "My_Lib");
        assert_eq!(s.projects[1].target_name, "My_Lib_B");
    }
}
