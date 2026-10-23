//! Finding MSBuild.exe.
//!
//! vswhere lists the Visual Studio and Build Tools instances. Only instances with the C++ build
//! tools count (their `MSBuild\Microsoft\VC\v*\Microsoft.Cpp.Default.props` exists): an instance
//! without them cannot build a `.vcxproj`, and `vswhere -latest` alone may return one. Among those,
//! the one whose major version matches the solution's `VisualStudioVersion` is preferred, then the
//! newest. For Visual Studio 2022 and later on 64-bit Windows the 64-bit MSBuild is used, as the
//! IDE does (it also runs the 64-bit-hosted compiler, see `docs/design.md`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::paths;

/// A Visual Studio or Build Tools installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub path: PathBuf,
    /// `17.14.37516.0`.
    pub version: String,
    pub display_name: String,
    pub prerelease: bool,
    /// The VC targets folder (`v170`), when the C++ build tools are installed.
    pub vc_targets: Option<String>,
}

impl Instance {
    pub fn major(&self) -> Option<u32> {
        self.version.split('.').next()?.parse().ok()
    }

    fn version_key(&self) -> Vec<u64> {
        self.version.split('.').map(|p| p.parse().unwrap_or(0)).collect()
    }
}

/// The MSBuild lazymsbuild runs.
#[derive(Clone, Debug)]
pub struct MsBuild {
    pub exe: PathBuf,
    /// The installation it belongs to (none when configured by path).
    pub instance: Option<Instance>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VswhereInstance {
    installation_path: String,
    installation_version: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    is_prerelease: Option<bool>,
}

/// vswhere.exe: at the fixed place the Visual Studio Installer puts it, or on `PATH`.
pub fn vswhere() -> Option<PathBuf> {
    let program_files = std::env::var_os("ProgramFiles(x86)").or_else(|| std::env::var_os("ProgramFiles"))?;
    let fixed = PathBuf::from(program_files).join(r"Microsoft Visual Studio\Installer\vswhere.exe");
    if fixed.is_file() {
        return Some(fixed);
    }
    crate::fsutil::which("vswhere.exe")
}

/// Every installation with MSBuild, prereleases included.
pub fn instances() -> Result<Vec<Instance>> {
    let Some(vswhere) = vswhere() else { bail!("vswhere.exe not found: is Visual Studio or Build Tools installed?") };
    let mut cmd = Command::new(&vswhere);
    cmd.args([
        "-all",
        "-prerelease",
        "-products",
        "*",
        "-requires",
        "Microsoft.Component.MSBuild",
        "-format",
        "json",
        "-utf8",
    ])
    .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = cmd.output().with_context(|| format!("cannot run {}", vswhere.display()))?;
    if !output.status.success() {
        bail!("vswhere failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let listed: Vec<VswhereInstance> = serde_json::from_slice(&output.stdout).context("unexpected vswhere output")?;
    Ok(listed
        .into_iter()
        .map(|i| {
            let path = PathBuf::from(&i.installation_path);
            Instance {
                vc_targets: vc_targets(&path),
                display_name: i.display_name.unwrap_or_else(|| i.installation_path.clone()),
                version: i.installation_version,
                prerelease: i.is_prerelease.unwrap_or(false),
                path,
            }
        })
        .collect())
}

/// The newest `MSBuild\Microsoft\VC\v*` folder that has the C++ props.
fn vc_targets(install: &Path) -> Option<String> {
    let dir = install.join(r"MSBuild\Microsoft\VC");
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("Microsoft.Cpp.Default.props").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found.pop()
}

/// The processor architecture of Windows itself (not of this process).
fn native_arch() -> String {
    std::env::var("PROCESSOR_ARCHITEW6432")
        .or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE"))
        .unwrap_or_default()
        .to_ascii_uppercase()
}

/// MSBuild.exe of an installation.
pub fn exe_for(instance: &Instance) -> Option<PathBuf> {
    let bin = instance.path.join(r"MSBuild\Current\Bin");
    let mut candidates = Vec::new();
    if instance.major().is_some_and(|m| m >= 17) {
        match native_arch().as_str() {
            "ARM64" => {
                candidates.push(bin.join(r"arm64\MSBuild.exe"));
                candidates.push(bin.join(r"amd64\MSBuild.exe"));
            }
            "AMD64" => candidates.push(bin.join(r"amd64\MSBuild.exe")),
            _ => {}
        }
    }
    candidates.push(bin.join("MSBuild.exe"));
    candidates.push(instance.path.join(r"MSBuild\15.0\Bin\MSBuild.exe"));
    candidates.into_iter().find(|c| c.is_file())
}

/// Picks the installation to build with.
pub fn choose(instances: &[Instance], preferred_major: Option<u32>) -> Option<&Instance> {
    instances
        .iter()
        .filter(|i| i.vc_targets.is_some())
        .max_by_key(|i| (preferred_major.is_some_and(|m| i.major() == Some(m)), !i.prerelease, i.version_key()))
}

/// Finds MSBuild: `configured` when given, otherwise through vswhere.
pub fn locate(configured: Option<&Path>, preferred_major: Option<u32>) -> Result<MsBuild> {
    if let Some(path) = configured {
        if !path.is_file() {
            bail!("build.msbuild is set to {}, which does not exist", path.display());
        }
        return Ok(MsBuild { exe: paths::normalize(path), instance: None });
    }
    let all = instances()?;
    let Some(chosen) = choose(&all, preferred_major) else {
        if all.is_empty() {
            bail!("no Visual Studio or Build Tools installation with MSBuild was found");
        }
        let names: Vec<&str> = all.iter().map(|i| i.display_name.as_str()).collect();
        bail!(
            "none of the installed Visual Studio instances ({}) has the C++ build tools; add the \"Desktop development with C++\" workload",
            names.join(", ")
        );
    };
    let Some(exe) = exe_for(chosen) else { bail!("MSBuild.exe not found in {}", chosen.path.display()) };
    Ok(MsBuild { exe, instance: Some(chosen.clone()) })
}

/// MSBuild's own version (`17.14.51.32402`), from `-version`.
pub fn version(exe: &Path) -> Result<String> {
    let mut cmd = Command::new(exe);
    cmd.args(["-nologo", "-version"]).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = cmd.output().with_context(|| format!("cannot run {}", exe.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_string).context("MSBuild printed no version")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(version: &str, vc: bool, prerelease: bool) -> Instance {
        Instance {
            path: PathBuf::from(format!(r"C:\VS\{version}")),
            version: version.into(),
            display_name: version.into(),
            prerelease,
            vc_targets: vc.then(|| "v170".to_string()),
        }
    }

    #[test]
    fn chooses_an_instance_with_cpp_tools() {
        let all = vec![
            instance("17.14.37516.0", false, false),
            instance("17.10.1.0", true, false),
            instance("16.11.5.0", true, false),
        ];
        assert_eq!(choose(&all, None).unwrap().version, "17.10.1.0", "the newest one without C++ is skipped");
        assert_eq!(choose(&all, Some(16)).unwrap().version, "16.11.5.0", "the solution's major version wins");
        let pre = vec![instance("18.0.1.0", true, true), instance("17.14.1.0", true, false)];
        assert_eq!(choose(&pre, None).unwrap().version, "17.14.1.0", "releases before previews");
        assert!(choose(&[instance("17.0.0.0", false, false)], None).is_none());
    }
}
