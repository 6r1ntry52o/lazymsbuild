//! The compile databases: one per solution configuration in the state directory (the master
//! copies), and the published `compile_commands.json`, a copy of the active configuration's.
//!
//! How an extraction changes a database (spec §5-2):
//! - **sync (method A)** rebuilds the configuration's database from the extractor's output;
//! - **a build (method B)** updates the entries of the sources it compiled (failed compiles
//!   included: their commands are valid); after a *successful* build, every project that was
//!   *rebuilt* has its entries replaced by this build's, since a rebuild compiles all of them;
//! - then, for the projects involved, entries of sources the project no longer lists are dropped
//!   (only when lazymsbuild could read the project's whole source list), and entries of sources
//!   that no longer exist on disk are dropped everywhere.
//!
//! A Clean changes nothing. Every write is atomic.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::compdb::{self, Entry, Format};
use crate::extract::{Extraction, Method};
use crate::solution::Profile;
use crate::vcxproj::ProjectFile;
use crate::workspace::{OutputRecord, Workspace};
use crate::{fsutil, git, paths, time};

pub const META_VERSION: u32 = 1;

/// Per-entry facts that are not part of a compile database.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntryMeta {
    /// The project file the entry came from.
    pub project: Option<String>,
    pub method: Method,
    pub updated_at: String,
}

/// Per-project facts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub path: String,
    /// When the project's entries were last known to match its project files (a sync, or a
    /// successful build). Freshness compares the project files' times with this.
    pub captured_at: Option<String>,
    pub method: Option<Method>,
}

/// The last build that touched the database.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastBuild {
    pub verb: String,
    pub target: Option<String>,
    /// `succeeded`, `failed` or `cancelled`.
    pub result: String,
    pub started_at: String,
    pub finished_at: String,
    pub errors: u32,
    pub warnings: u32,
}

/// `<Config>_<Platform>.meta.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub format_version: u32,
    pub solution: String,
    pub profile: String,
    pub updated_at: Option<String>,
    /// By entry key.
    pub entries: BTreeMap<String, EntryMeta>,
    /// By project key.
    pub projects: BTreeMap<String, ProjectMeta>,
    pub last_build: Option<LastBuild>,
}

/// What applying an extraction changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub before: usize,
    pub after: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

impl ApplyReport {
    pub fn delta(&self) -> i64 {
        self.after as i64 - self.before as i64
    }
}

/// One configuration's database and its meta file.
#[derive(Clone, Debug)]
pub struct ProfileDb {
    pub profile: Profile,
    pub db_path: PathBuf,
    pub meta_path: PathBuf,
    /// By entry key.
    pub entries: BTreeMap<String, Entry>,
    pub meta: Meta,
    /// Whether the database existed on disk when it was loaded.
    pub exists: bool,
}

impl ProfileDb {
    pub fn load(workspace: &Workspace, profile: &Profile) -> Result<ProfileDb> {
        Self::load_from(
            workspace.profile_db_path(profile),
            workspace.profile_meta_path(profile),
            profile,
            &workspace.solution.path,
        )
    }

    pub fn load_from(db_path: PathBuf, meta_path: PathBuf, profile: &Profile, solution: &Path) -> Result<ProfileDb> {
        let exists = db_path.is_file();
        let entries = if exists { compdb::read(&db_path)? } else { Vec::new() };
        let mut meta: Meta = match std::fs::read_to_string(&meta_path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Meta::default(),
        };
        if meta.format_version > META_VERSION {
            // Written by a newer lazymsbuild: start over rather than misread it. The entries are
            // still valid; only the bookkeeping is lost until the next sync or build.
            meta = Meta::default();
        }
        meta.format_version = META_VERSION;
        meta.solution = solution.display().to_string();
        meta.profile = profile.to_string();
        let entries = entries.into_iter().map(|e| (e.key(), e)).collect();
        Ok(ProfileDb { profile: profile.clone(), db_path, meta_path, entries, meta, exists })
    }

    pub fn entries_vec(&self) -> Vec<Entry> {
        self.entries.values().cloned().collect()
    }

    /// The project an entry belongs to, by entry key.
    pub fn project_of(&self, key: &str) -> Option<&str> {
        self.meta.entries.get(key).and_then(|m| m.project.as_deref())
    }

    /// Applies an extraction. `projects` are the parsed project files of the solution's C++
    /// projects, by project key, for dropping sources a project no longer lists.
    /// `replace_complete` allows replacing the entries of rebuilt projects (only after a build
    /// that succeeded); a sync always replaces everything.
    pub fn apply(
        &mut self,
        extraction: &Extraction,
        projects: &HashMap<String, ProjectFile>,
        replace_complete: bool,
        captured_at: &str,
    ) -> ApplyReport {
        let now = time::now();
        let mut report = ApplyReport { before: self.entries.len(), ..Default::default() };

        match extraction.method {
            Method::DesignTime => {
                self.entries.clear();
                self.meta.entries.clear();
            }
            Method::Build if replace_complete => {
                for project in &extraction.complete_projects {
                    let project_key = paths::key(project);
                    let keys: Vec<String> = self
                        .meta
                        .entries
                        .iter()
                        .filter(|(_, m)| m.project.as_deref().is_some_and(|p| paths::key(Path::new(p)) == project_key))
                        .map(|(k, _)| k.clone())
                        .collect();
                    for key in keys {
                        self.entries.remove(&key);
                        self.meta.entries.remove(&key);
                    }
                }
            }
            Method::Build => {}
        }

        for captured in &extraction.entries {
            let key = captured.entry.key();
            match self.entries.insert(key.clone(), captured.entry.clone()) {
                Some(old) if old != captured.entry => report.updated += 1,
                Some(_) => {}
                None => report.added += 1,
            }
            self.meta.entries.insert(
                key,
                EntryMeta {
                    project: captured.project.as_ref().map(|p| p.display().to_string()),
                    method: extraction.method,
                    updated_at: now.clone(),
                },
            );
        }

        let involved = if extraction.method == Method::DesignTime {
            extraction.complete_projects.clone()
        } else {
            let mut involved: Vec<PathBuf> = extraction.entries.iter().filter_map(|c| c.project.clone()).collect();
            involved.extend(extraction.current_projects.iter().cloned());
            involved
        };
        for project in &extraction.current_projects {
            let meta = self.meta.projects.entry(paths::key(project)).or_default();
            meta.path = project.display().to_string();
            meta.captured_at = Some(captured_at.to_string());
            meta.method = Some(extraction.method);
        }

        // Sources a project no longer lists. Only for projects whose whole list could be read, and
        // never what this extraction compiled: a target can add ClCompile items while it runs
        // (Notepad++ adds uchardet that way), which no reading of the project file sees.
        let compiled_now: HashSet<String> = extraction.entries.iter().map(|c| c.entry.key()).collect();
        let mut drop_keys = Vec::new();
        for project in &involved {
            let project_key = paths::key(project);
            let Some(file) = projects.get(&project_key).filter(|f| f.unresolved.is_empty()) else { continue };
            for (key, meta) in &self.meta.entries {
                let belongs = meta.project.as_deref().is_some_and(|p| paths::key(Path::new(p)) == project_key);
                if belongs && !file.lists(key) && !compiled_now.contains(key) {
                    drop_keys.push(key.clone());
                }
            }
        }
        // Sources that no longer exist.
        drop_keys.extend(self.entries.iter().filter(|(_, e)| !Path::new(&e.file).exists()).map(|(k, _)| k.clone()));
        drop_keys.sort();
        drop_keys.dedup();
        for key in drop_keys {
            if self.entries.remove(&key).is_some() {
                report.removed += 1;
            }
            self.meta.entries.remove(&key);
        }

        self.meta.updated_at = Some(now);
        report.after = self.entries.len();
        report
    }

    /// Writes the database and its meta file, each atomically.
    pub fn save(&mut self) -> Result<()> {
        let json = compdb::to_json(&self.entries_vec(), Format::Arguments);
        fsutil::write_atomic(&self.db_path, json.as_bytes())
            .with_context(|| format!("cannot write {}", self.db_path.display()))?;
        let meta = serde_json::to_string_pretty(&self.meta)?;
        fsutil::write_atomic(&self.meta_path, meta.as_bytes())
            .with_context(|| format!("cannot write {}", self.meta_path.display()))?;
        self.exists = true;
        Ok(())
    }

    /// The bytes the published copy of this database should have.
    pub fn render(&self, format: Format) -> String {
        compdb::to_json(&self.entries_vec(), format)
    }
}

/// Why the database could not be published.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error(
        "{0} is tracked by Git, so lazymsbuild does not write it. Use external mode (`lazymsbuild init --mode external`) or stop tracking it"
    )]
    Tracked(PathBuf),
    #[error("{0} was not written by lazymsbuild (or was changed since); pass --force to overwrite it")]
    Foreign(PathBuf),
}

/// Checks that the output may be written: not tracked by Git, and either absent or last written
/// by lazymsbuild.
pub fn check_publish(workspace: &Workspace, output: &Path) -> Result<()> {
    if workspace.in_git
        && output.exists()
        && paths::relative_to(output, &workspace.root).is_some()
        && git::available()
        && git::is_tracked(&workspace.root, output).unwrap_or(false)
    {
        return Err(PublishError::Tracked(output.to_path_buf()).into());
    }
    if let Some(actual) = fsutil::sha256_file(output)? {
        let ours = workspace.output_record(output).is_some_and(|r| r.sha256 == actual);
        if !ours {
            return Err(PublishError::Foreign(output.to_path_buf()).into());
        }
    }
    Ok(())
}

/// Publishes a configuration's database to the output path. Unless `force`, refuses to overwrite
/// a file lazymsbuild did not write ([`check_publish`]); a file tracked by Git is never written.
pub fn publish(workspace: &mut Workspace, db: &ProfileDb, force: bool) -> Result<PathBuf> {
    let output = workspace.output_path();
    match check_publish(workspace, &output) {
        Ok(()) => {}
        Err(err) if force && matches!(err.downcast_ref::<PublishError>(), Some(PublishError::Foreign(_))) => {}
        Err(err) => return Err(err),
    }
    let bytes = db.render(workspace.config.output.format);
    fsutil::write_atomic(&output, bytes.as_bytes()).with_context(|| format!("cannot write {}", output.display()))?;
    workspace.record_output(OutputRecord {
        path: output.clone(),
        sha256: fsutil::sha256(bytes.as_bytes()),
        solution: workspace.solution.path.clone(),
        profile: db.profile.to_string(),
        written_at: time::now(),
    })?;
    Ok(output)
}

/// Whether the published file matches the configuration's database.
pub fn in_sync(workspace: &Workspace, db: &ProfileDb) -> Result<Option<bool>> {
    let output = workspace.output_path();
    let Some(actual) = fsutil::sha256_file(&output)? else { return Ok(None) };
    Ok(Some(actual == fsutil::sha256(db.render(workspace.config.output.format).as_bytes())))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::extract::Captured;

    fn entry(file: &Path, flag: &str) -> Entry {
        Entry {
            directory: file.parent().unwrap().display().to_string(),
            file: file.display().to_string(),
            arguments: vec!["cl.exe".into(), flag.into(), file.display().to_string()],
            output: None,
        }
    }

    fn captured(file: &Path, flag: &str, project: &Path) -> Captured {
        Captured { entry: entry(file, flag), project: Some(project.to_path_buf()) }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        db: ProfileDb,
        projects: HashMap<String, ProjectFile>,
        project: PathBuf,
    }

    fn fixture(sources: &[&str]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = paths::normalize(dir.path());
        for s in ["a.cpp", "b.cpp", "c.cpp", "d.cpp"] {
            std::fs::write(root.join(s), "").unwrap();
        }
        let project = root.join("P.vcxproj");
        let items: String = sources.iter().map(|s| format!(r#"<ClCompile Include="{s}" />"#)).collect();
        std::fs::write(&project, format!("<Project><ItemGroup>{items}</ItemGroup></Project>")).unwrap();
        let mut projects = HashMap::new();
        projects.insert(paths::key(&project), ProjectFile::load(&project, None, None).unwrap());
        let db = ProfileDb::load_from(
            root.join("db.json"),
            root.join("db.meta.json"),
            &Profile::new("Debug", "x64"),
            &root.join("S.sln"),
        )
        .unwrap();
        Fixture { _dir: dir, root, db, projects, project }
    }

    #[test]
    fn sync_replaces_everything() {
        let mut f = fixture(&["a.cpp", "b.cpp"]);
        let mut x = Extraction::new(Method::DesignTime);
        x.entries = vec![
            captured(&f.root.join("a.cpp"), "/DA", &f.project),
            captured(&f.root.join("b.cpp"), "/DA", &f.project),
        ];
        x.complete_projects = vec![f.project.clone()];
        x.current_projects = vec![f.project.clone()];
        let r = f.db.apply(&x, &f.projects, true, "2026-10-23T00:00:00.000+09:00");
        assert_eq!((r.before, r.after, r.added), (0, 2, 2));
        let project_meta = &f.db.meta.projects[&paths::key(&f.project)];
        assert_eq!(project_meta.captured_at.as_deref(), Some("2026-10-23T00:00:00.000+09:00"));

        x.entries.pop();
        let r = f.db.apply(&x, &f.projects, true, "t");
        assert_eq!((r.before, r.after), (2, 1), "a sync replaces the whole database");
    }

    #[test]
    fn build_upserts_and_replaces_only_rebuilt_projects_on_success() {
        let mut f = fixture(&["a.cpp", "b.cpp", "c.cpp"]);
        let other = f.root.join("Other.vcxproj");
        let mut sync = Extraction::new(Method::DesignTime);
        sync.entries = vec![
            captured(&f.root.join("a.cpp"), "/DOLD", &f.project),
            captured(&f.root.join("b.cpp"), "/DOLD", &f.project),
            captured(&f.root.join("d.cpp"), "/DOLD", &other),
        ];
        sync.complete_projects = vec![f.project.clone(), other.clone()];
        f.db.apply(&sync, &f.projects, true, "t0");

        // An incremental build compiled a.cpp and the new c.cpp.
        let mut build = Extraction::new(Method::Build);
        build.entries = vec![
            captured(&f.root.join("a.cpp"), "/DNEW", &f.project),
            captured(&f.root.join("c.cpp"), "/DNEW", &f.project),
        ];
        build.current_projects = vec![f.project.clone()];
        let r = f.db.apply(&build, &f.projects, false, "t1");
        assert_eq!((r.added, r.updated, r.after), (1, 1, 4));
        assert_eq!(
            f.db.entries[&paths::key(&f.root.join("b.cpp"))].arguments[1],
            "/DOLD",
            "untouched by an incremental build"
        );

        // A failed or cancelled rebuild only upserts.
        let mut rebuild = Extraction::new(Method::Build);
        rebuild.entries = vec![captured(&f.root.join("a.cpp"), "/DRE", &f.project)];
        rebuild.complete_projects = vec![f.project.clone()];
        let r = f.db.apply(&rebuild, &f.projects, false, "t2");
        assert_eq!(r.after, 4);

        // A successful rebuild replaces the project's entries, and only that project's.
        let r = f.db.apply(&rebuild, &f.projects, true, "t3");
        assert_eq!(r.after, 2, "a.cpp from the rebuild, d.cpp of the other project");
        assert!(f.db.entries.contains_key(&paths::key(&f.root.join("d.cpp"))));
    }

    #[test]
    fn gc_drops_unlisted_and_missing_sources() {
        let mut f = fixture(&["a.cpp", "b.cpp"]);
        let mut sync = Extraction::new(Method::DesignTime);
        sync.entries = vec![
            captured(&f.root.join("a.cpp"), "/D1", &f.project),
            captured(&f.root.join("b.cpp"), "/D1", &f.project),
        ];
        sync.complete_projects = vec![f.project.clone()];
        f.db.apply(&sync, &f.projects, true, "t0");

        // b.cpp leaves the project (still on disk), a.cpp is deleted from disk.
        std::fs::write(
            &f.project,
            r#"<Project><ItemGroup><ClCompile Include="a.cpp" /><ClCompile Include="c.cpp" /></ItemGroup></Project>"#,
        )
        .unwrap();
        f.projects.insert(paths::key(&f.project), ProjectFile::load(&f.project, None, None).unwrap());
        std::fs::remove_file(f.root.join("a.cpp")).unwrap();
        let mut build = Extraction::new(Method::Build);
        build.entries = vec![captured(&f.root.join("c.cpp"), "/D2", &f.project)];
        build.current_projects = vec![f.project.clone()];
        let r = f.db.apply(&build, &f.projects, false, "t1");
        let keys: Vec<&String> = f.db.entries.keys().collect();
        assert_eq!(keys, [&paths::key(&f.root.join("c.cpp"))]);
        assert_eq!(r.removed, 2);
    }

    #[test]
    fn gc_keeps_what_the_build_compiled() {
        // A target adds b.cpp at build time; the project file does not list it.
        let mut f = fixture(&["a.cpp", "b.cpp"]);
        std::fs::write(&f.project, r#"<Project><ItemGroup><ClCompile Include="a.cpp" /></ItemGroup></Project>"#)
            .unwrap();
        f.projects.insert(paths::key(&f.project), ProjectFile::load(&f.project, None, None).unwrap());
        let mut build = Extraction::new(Method::Build);
        build.entries = vec![
            captured(&f.root.join("a.cpp"), "/D1", &f.project),
            captured(&f.root.join("b.cpp"), "/D1", &f.project),
        ];
        build.current_projects = vec![f.project.clone()];
        build.complete_projects = vec![f.project.clone()];
        let r = f.db.apply(&build, &f.projects, true, "t0");
        assert_eq!((r.after, r.removed), (2, 0));
    }

    #[test]
    fn save_and_load_round_trip() {
        let mut f = fixture(&["a.cpp"]);
        let mut x = Extraction::new(Method::Build);
        x.entries = vec![captured(&f.root.join("a.cpp"), "/D1", &f.project)];
        f.db.apply(&x, &f.projects, false, "t");
        f.db.save().unwrap();
        let back =
            ProfileDb::load_from(f.db.db_path.clone(), f.db.meta_path.clone(), &f.db.profile, &f.root.join("S.sln"))
                .unwrap();
        assert!(back.exists);
        assert_eq!(back.entries, f.db.entries);
        assert_eq!(back.meta, f.db.meta);
        assert_eq!(back.project_of(&paths::key(&f.root.join("a.cpp"))), Some(f.project.display().to_string().as_str()));
    }
}
