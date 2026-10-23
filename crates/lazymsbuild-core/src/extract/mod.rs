//! Getting compile commands out of MSBuild.
//!
//! Two methods (see `docs/design.md`):
//! - **A, design-time** ([`design_time`]): Microsoft's msbuild-extractor-sample evaluates the
//!   projects without building. Every source, no build needed: what `sync` uses.
//! - **B, build-time** ([`build_log`]): lazymsbuild's MSBuild logger records the compiler command
//!   lines of a real build. Exactly what was compiled, at no extra cost, but only for the sources
//!   the build compiled: what keeps the database current after each build.
//!
//! Both produce an [`Extraction`], which [`crate::store`] applies the same way whatever made it.
//! A third method, a `.binlog` converter, was planned but no such converter exists; `-bl` is kept
//! as a diagnostic option (`build.keep_binlog`).

pub mod build_log;
pub mod cl;
pub mod design_time;

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::compdb::Entry;
use crate::diag::Diagnostic;

/// How entries were obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Method {
    /// Method A: msbuild-extractor-sample.
    DesignTime,
    /// Method B: the build logger.
    Build,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Method::DesignTime => "design-time",
            Method::Build => "build",
        })
    }
}

/// An entry and the project it belongs to.
#[derive(Clone, Debug)]
pub struct Captured {
    pub entry: Entry,
    /// The project file, when known.
    pub project: Option<PathBuf>,
}

/// What one extraction produced.
#[derive(Clone, Debug)]
pub struct Extraction {
    pub method: Method,
    pub entries: Vec<Captured>,
    /// Projects for which this extraction holds every entry: on sync all evaluated projects,
    /// after a build the projects that were rebuilt successfully. Their old entries are replaced.
    pub complete_projects: Vec<PathBuf>,
    /// Projects whose entries are now current with their project files (a successful build
    /// recompiles whatever the project file changes affect). Freshness is measured from here.
    pub current_projects: Vec<PathBuf>,
    pub diagnostics: Vec<Diagnostic>,
    pub warnings: Vec<String>,
}

impl Extraction {
    pub fn new(method: Method) -> Extraction {
        Extraction {
            method,
            entries: Vec::new(),
            complete_projects: Vec::new(),
            current_projects: Vec::new(),
            diagnostics: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// A way to get compile commands that rides along with a build (method B, or a future binlog
/// reader): it adds arguments to the MSBuild command line, then reads what the build left behind.
pub trait CommandExtractor {
    fn method(&self) -> Method;

    /// Arguments to add to the MSBuild command line. `output` is a fresh file for this build.
    fn msbuild_args(&self, output: &std::path::Path) -> Vec<String>;

    /// Reads what the build left in `output`.
    fn collect(&self, output: &std::path::Path) -> anyhow::Result<Extraction>;
}
