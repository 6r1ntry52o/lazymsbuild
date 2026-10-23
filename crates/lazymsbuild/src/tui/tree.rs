//! The rows of the Solution & Targets pane: the solution, its folders and its C++ projects, as a
//! tree. Folders come first, then projects, each in alphabetical order (as in Solution Explorer);
//! folders without a C++ project are left out.

use lazymsbuild_core::solution::Solution;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Solution,
    Folder(usize),
    /// An index into `Solution::projects`.
    Project(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    /// Tree lines before the label (`│   ├─ `).
    pub prefix: String,
    pub label: String,
}

/// Builds the rows. With a filter, only projects whose name contains it (ignoring case) and the
/// folders above them are kept.
pub fn rows(solution: &Solution, filter: Option<&str>) -> Vec<Row> {
    let filter = filter.map(str::to_lowercase).filter(|f| !f.is_empty());
    let keep = |index: usize| -> bool {
        let project = &solution.projects[index];
        project.is_cpp && filter.as_ref().is_none_or(|f| project.name.to_lowercase().contains(f))
    };
    let mut out = vec![Row {
        kind: RowKind::Solution,
        prefix: String::new(),
        label: format!("[Solution] {}", solution.file_name()),
    }];
    children(solution, None, &keep, &mut Vec::new(), &mut out);
    out
}

enum Child {
    Folder(usize),
    Project(usize),
}

fn has_project(solution: &Solution, folder: usize, keep: &dyn Fn(usize) -> bool, depth: usize) -> bool {
    if depth > solution.folders.len() {
        return false;
    }
    solution.projects.iter().enumerate().any(|(i, p)| p.folder == Some(folder) && keep(i))
        || solution
            .folders
            .iter()
            .enumerate()
            .any(|(i, f)| f.parent == Some(folder) && i != folder && has_project(solution, i, keep, depth + 1))
}

fn children(
    solution: &Solution,
    parent: Option<usize>,
    keep: &dyn Fn(usize) -> bool,
    lasts: &mut Vec<bool>,
    out: &mut Vec<Row>,
) {
    if lasts.len() > solution.folders.len() {
        return; // a cycle in a hand-edited solution
    }
    let mut folders: Vec<usize> = (0..solution.folders.len())
        .filter(|&i| solution.folders[i].parent == parent && Some(i) != parent && has_project(solution, i, keep, 0))
        .collect();
    folders.sort_by_key(|&i| solution.folders[i].name.to_lowercase());
    let mut projects: Vec<usize> =
        (0..solution.projects.len()).filter(|&i| solution.projects[i].folder == parent && keep(i)).collect();
    projects.sort_by_key(|&i| solution.projects[i].name.to_lowercase());
    let items: Vec<Child> =
        folders.into_iter().map(Child::Folder).chain(projects.into_iter().map(Child::Project)).collect();

    let count = items.len();
    for (n, item) in items.into_iter().enumerate() {
        let last = n + 1 == count;
        let mut prefix: String = lasts.iter().map(|&l| if l { "    " } else { "│   " }).collect();
        prefix.push_str(if last { "└─ " } else { "├─ " });
        match item {
            Child::Folder(index) => {
                out.push(Row {
                    kind: RowKind::Folder(index),
                    prefix,
                    label: format!("{}/", solution.folders[index].name),
                });
                lasts.push(last);
                children(solution, Some(index), keep, lasts, out);
                lasts.pop();
            }
            Child::Project(index) => {
                out.push(Row { kind: RowKind::Project(index), prefix, label: solution.projects[index].name.clone() });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use lazymsbuild_core::solution::{Folder, Project, SolutionFormat};

    use super::*;

    fn project(name: &str, folder: Option<usize>, is_cpp: bool) -> Project {
        Project {
            name: name.into(),
            path: PathBuf::from(format!(r"C:\r\{name}.vcxproj")),
            guid: None,
            is_cpp,
            folder,
            target_name: name.into(),
            configs: BTreeMap::new(),
        }
    }

    fn solution() -> Solution {
        Solution {
            path: PathBuf::from(r"C:\r\Basic.sln"),
            format: SolutionFormat::Sln,
            visual_studio_version: None,
            profiles: Vec::new(),
            projects: vec![
                project("ControlApp", None, true),
                project("CoreEngine", Some(0), true),
                project("Tool", None, false),
                project("UnitTests", None, true),
                project("Util", Some(1), true),
            ],
            folders: vec![
                Folder { name: "Libs".into(), parent: None },
                Folder { name: "Native".into(), parent: Some(0) },
                Folder { name: "Docs".into(), parent: None },
            ],
        }
    }

    fn render(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| format!("{}{}", r.prefix, r.label)).collect()
    }

    #[test]
    fn folders_first_then_projects_without_empty_folders() {
        assert_eq!(
            render(&rows(&solution(), None)),
            [
                "[Solution] Basic.sln",
                "├─ Libs/",
                "│   ├─ Native/",
                "│   │   └─ Util",
                "│   └─ CoreEngine",
                "├─ ControlApp",
                "└─ UnitTests",
            ]
        );
    }

    #[test]
    fn a_filter_keeps_matches_and_their_folders() {
        assert_eq!(
            render(&rows(&solution(), Some("UTI"))),
            ["[Solution] Basic.sln", "└─ Libs/", "    └─ Native/", "        └─ Util"]
        );
        assert_eq!(
            render(&rows(&solution(), Some("Unit"))),
            ["[Solution] Basic.sln", "└─ UnitTests"],
            "the filter matches names, ignoring case"
        );
        assert_eq!(render(&rows(&solution(), Some("zzz"))), ["[Solution] Basic.sln"]);
    }
}
