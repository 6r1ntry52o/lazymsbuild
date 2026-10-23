//! From a compiler command line to compile-database entries.
//!
//! One cl.exe call may compile several sources (MSBuild batches every source with the same
//! options, and with `/MP` all of them); each source gets its own entry with the same options.
//!
//! Two things cl.exe gets outside its command line are added, so a clang tool sees what the
//! compiler saw (`docs/design.md`):
//! - the target architecture, as `--target=<triple>` right after the compiler;
//! - the `IncludePath` and `ExternalIncludePath` directories, which MSBuild passes in the
//!   `INCLUDE` and `EXTERNAL_INCLUDE` environment variables, as `/imsvc<dir>` (clang-cl: "add
//!   directory to system include search path, as if part of %INCLUDE%").
//!
//! And `/Y-` after precompiled-header options, see [`disable_pch`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::compdb::Entry;
use crate::paths;

/// Options whose value is the next argument (`/D NAME`). From MSBuild's `cl.xml` (switches written
/// with a separate value), cl.exe's documentation, and clang-cl. Case-sensitive, like cl.exe.
const SEPARATE_VALUE: &[&str] = &[
    // cl.exe
    "D",
    "I",
    "U",
    "FI",
    "FU",
    "AI",
    "F",
    "external:I",
    "analyze:log",
    "analyze:stacksize",
    "analyze:max_paths",
    "analyze:ruleset",
    "analyze:plugin",
    "headerUnit",
    "headerUnit:quote",
    "headerUnit:angle",
    "headerName:quote",
    "headerName:angle",
    "reference",
    "ifcOutput",
    "ifcSearchDir",
    "ifcMap",
    "stdIfcDir",
    "sourceDependencies",
    "sourceDependencies:directives",
    "scanDependencies",
    "dynamicdeopt:suffix",
    "winsysroot",
    "vctoolsdir",
    "winsdkdir",
    // clang-cl and clang
    "Xclang",
    "Xlinker",
    "Xpreprocessor",
    "Xassembler",
    "imsvc",
    "include",
    "isystem",
    "iquote",
    "idirafter",
    "isysroot",
    "ivfsoverlay",
    "o",
    "MF",
    "MT",
    "MQ",
    "target",
    "x",
    "resource-dir",
];

/// A compiler lazymsbuild records entries for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compiler {
    Cl,
    ClangCl,
}

/// Recognizes a compiler by its executable name.
pub fn compiler(program: &str) -> Option<Compiler> {
    let name = Path::new(program).file_name()?.to_string_lossy().to_ascii_lowercase();
    match name.as_str() {
        "cl.exe" | "cl" => Some(Compiler::Cl),
        "clang-cl.exe" | "clang-cl" => Some(Compiler::ClangCl),
        _ => None,
    }
}

/// What a project knows about a compile, beyond the command line.
#[derive(Clone, Debug)]
pub struct CompileContext<'a> {
    /// The compiler's working directory: the project directory.
    pub directory: &'a Path,
    /// The project's platform (`x64`, `Win32`), used when the compiler path does not tell.
    pub platform: Option<&'a str>,
    /// `$(IncludePath)` (`;`-separated).
    pub include_path: Option<&'a str>,
    /// `$(ExternalIncludePath)` (`;`-separated).
    pub external_include_path: Option<&'a str>,
}

impl<'a> CompileContext<'a> {
    /// A context with only the directory known.
    pub fn new(directory: &'a Path) -> CompileContext<'a> {
        CompileContext { directory, platform: None, include_path: None, external_include_path: None }
    }
}

/// Remembers which directories exist, so a build with many compiles checks each once.
#[derive(Debug, Default)]
pub struct DirCache(HashMap<String, bool>);

impl DirCache {
    fn exists(&mut self, dir: &Path) -> bool {
        *self.0.entry(paths::key(dir)).or_insert_with(|| dir.is_dir())
    }
}

struct Source {
    path: String,
    /// `/Tp` or `/Tc` when the source was given with one.
    marker: Option<String>,
}

/// Builds one entry per source of a compiler call.
pub fn entries(program: &str, args: &[String], context: &CompileContext<'_>, dirs: &mut DirCache) -> Vec<Entry> {
    let mut compile = Vec::new(); // options before /link
    let mut link = Vec::new(); // /link and what follows
    let mut sources = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !link.is_empty() {
            link.push(arg.clone());
            i += 1;
            continue;
        }
        let option = arg.strip_prefix('/').or_else(|| arg.strip_prefix('-').filter(|_| arg.len() > 1));
        match option {
            Some("link") => link.push(arg.clone()),
            Some(name @ ("Tp" | "Tc")) => {
                if let Some(file) = args.get(i + 1) {
                    sources.push(Source { path: file.clone(), marker: Some(format!("/{name}")) });
                    i += 1;
                }
            }
            Some(name) if (name.starts_with("Tp") || name.starts_with("Tc")) && name.len() > 2 => {
                sources.push(Source { path: name[2..].to_string(), marker: Some(format!("/{}", &name[..2])) });
            }
            Some(name) if SEPARATE_VALUE.contains(&name) && i + 1 < args.len() => {
                compile.push(arg.clone());
                compile.push(args[i + 1].clone());
                i += 1;
            }
            Some(_) => compile.push(arg.clone()),
            None if arg.starts_with('@') => compile.push(arg.clone()),
            None => sources.push(Source { path: arg.clone(), marker: None }),
        }
        i += 1;
    }
    if sources.is_empty() {
        return Vec::new();
    }

    let mut extra = Vec::new();
    if let Some(triple) = target_triple(program, &compile, context.platform) {
        extra.push(format!("--target={triple}"));
    }
    let include_dirs = include_dirs(context, dirs);

    let directory = context.directory.to_string_lossy().into_owned();
    sources
        .into_iter()
        .map(|source| {
            let file = paths::absolutize(context.directory, Path::new(&source.path));
            let file_text = file.to_string_lossy().into_owned();
            let mut arguments = Vec::with_capacity(compile.len() + include_dirs.len() + 4);
            arguments.push(program.to_string());
            arguments.extend(extra.iter().cloned());
            arguments.extend(compile.iter().cloned());
            arguments.extend(include_dirs.iter().map(|d| format!("/imsvc{d}")));
            if let Some(marker) = source.marker {
                arguments.push(marker);
            }
            arguments.push(file_text.clone());
            arguments.extend(link.iter().cloned());
            disable_pch(&mut arguments);
            Entry {
                directory: directory.clone(),
                output: object_file(&compile, &file, context.directory),
                file: file_text,
                arguments,
            }
        })
        .collect()
}

/// Adds `/Y-` after the last `/Yc` or `/Yu`. Clang tools cannot read MSVC's `.pch`
/// (clang-tidy: "not a valid precompiled PCH file"), and `/Yc` makes clang-cl plan two jobs, which
/// clang-tidy refuses ("expected exactly one compiler job"); `/Y-`, which cl.exe and clang-cl both
/// take as "ignore the other /Y options", has every tool parse the source without a precompiled
/// header, as clangd does. The original options stay, so the entry still shows the build's.
pub fn disable_pch(arguments: &mut Vec<String>) {
    let is_y = |arg: &str, name: &str| {
        arg.strip_prefix('/').or_else(|| arg.strip_prefix('-')).is_some_and(|option| option.starts_with(name))
    };
    let Some(last) = arguments.iter().rposition(|a| is_y(a, "Yc") || is_y(a, "Yu")) else { return };
    if arguments.iter().any(|a| is_y(a, "Y-")) {
        return;
    }
    arguments.insert(last + 1, "/Y-".to_string());
}

/// The `IncludePath` then `ExternalIncludePath` directories that exist, without repeats.
fn include_dirs(context: &CompileContext<'_>, dirs: &mut DirCache) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for list in [context.include_path, context.external_include_path].into_iter().flatten() {
        for part in list.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let dir = paths::absolutize(context.directory, Path::new(part));
            if seen.insert(paths::key(&dir)) && dirs.exists(&dir) {
                out.push(dir.to_string_lossy().trim_end_matches('\\').to_string());
            }
        }
    }
    out
}

/// The clang target triple for a compile, unless the command already chooses one.
pub fn target_triple(program: &str, options: &[String], platform: Option<&str>) -> Option<&'static str> {
    // Only options: a value such as the `target_os` of `/D target_os` must not count.
    let chooses_target = options.iter().filter(|o| o.starts_with(['/', '-'])).any(|o| {
        let o = o.trim_start_matches(['/', '-']);
        o.starts_with("target") || o.starts_with("clang:--target") || o == "m32" || o == "m64"
    });
    if chooses_target {
        return None;
    }
    if options.iter().any(|o| o.eq_ignore_ascii_case("/arm64EC") || o.eq_ignore_ascii_case("-arm64EC")) {
        return Some("arm64ec-pc-windows-msvc");
    }
    // MSVC layout: ...\bin\Host<host>\<target>\cl.exe
    let target_dir =
        Path::new(program).parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_ascii_lowercase());
    let from_dir = match target_dir.as_deref() {
        Some("x86") => Some("i686-pc-windows-msvc"),
        Some("x64" | "amd64") => Some("x86_64-pc-windows-msvc"),
        Some("arm64") => Some("aarch64-pc-windows-msvc"),
        Some("arm") => Some("thumbv7-pc-windows-msvc"),
        _ => None,
    };
    from_dir.or_else(|| match platform.map(str::to_ascii_lowercase).as_deref() {
        Some("win32" | "x86") => Some("i686-pc-windows-msvc"),
        Some("x64") => Some("x86_64-pc-windows-msvc"),
        Some("arm64") => Some("aarch64-pc-windows-msvc"),
        Some("arm") => Some("thumbv7-pc-windows-msvc"),
        Some("arm64ec") => Some("arm64ec-pc-windows-msvc"),
        _ => None,
    })
}

/// The object file from `/Fo` (a file, or a directory ending in a separator).
fn object_file(options: &[String], source: &Path, directory: &Path) -> Option<String> {
    let value = options.iter().rev().find_map(|o| o.strip_prefix("/Fo").or_else(|| o.strip_prefix("-Fo")))?;
    if value.is_empty() {
        return None;
    }
    let path = if value.ends_with(['\\', '/']) {
        let stem = source.file_stem()?.to_string_lossy();
        PathBuf::from(value).join(format!("{stem}.obj"))
    } else {
        PathBuf::from(value)
    };
    Some(paths::absolutize(directory, &path).to_string_lossy().into_owned())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::cmdline;

    const CL: &str = r"C:\Program Files\VS\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64\CL.exe";

    fn args(line: &str) -> Vec<String> {
        cmdline::split_args(line)
    }

    #[test]
    fn recognizes_compilers() {
        assert_eq!(compiler(CL), Some(Compiler::Cl));
        assert_eq!(compiler(r"C:\LLVM\bin\clang-cl.exe"), Some(Compiler::ClangCl));
        assert_eq!(compiler(r"C:\VS\bin\HostX64\x64\Lib.exe"), None);
        assert_eq!(compiler(r"C:\VS\bin\HostX64\x64\link.exe"), None);
    }

    #[test]
    fn splits_a_batch_and_keeps_option_values() {
        let dir = Path::new(r"C:\repo\src\Core");
        let context = CompileContext::new(dir);
        let a = args(r#"/c /Iinclude /D _DEBUG /D "NAME=a b" /Fo"x64\Debug\\" /TP core.cpp "sub dir\debug only.cpp""#);
        let e = entries(CL, &a, &context, &mut DirCache::default());
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].file, r"C:\repo\src\Core\core.cpp");
        assert_eq!(e[1].file, r"C:\repo\src\Core\sub dir\debug only.cpp");
        assert_eq!(e[0].directory, r"C:\repo\src\Core");
        assert_eq!(
            e[0].arguments,
            [
                CL,
                "--target=x86_64-pc-windows-msvc",
                "/c",
                "/Iinclude",
                "/D",
                "_DEBUG",
                "/D",
                "NAME=a b",
                r"/Fox64\Debug\",
                "/TP",
                r"C:\repo\src\Core\core.cpp"
            ]
        );
        assert_eq!(e[0].output.as_deref(), Some(r"C:\repo\src\Core\x64\Debug\core.obj"));
        assert_eq!(e[1].output.as_deref(), Some(r"C:\repo\src\Core\x64\Debug\debug only.obj"));
    }

    #[test]
    fn tp_markers_stay_with_their_source() {
        let dir = Path::new(r"C:\r");
        let context = CompileContext::new(dir);
        let e = entries(CL, &args("/c /Tp a.inl /Tcb.x c.cpp"), &context, &mut DirCache::default());
        let tails: Vec<Vec<&str>> =
            e.iter().map(|e| e.arguments[e.arguments.len() - 2..].iter().map(String::as_str).collect()).collect();
        assert_eq!(tails, [vec!["/Tp", r"C:\r\a.inl"], vec!["/Tc", r"C:\r\b.x"], vec!["/c", r"C:\r\c.cpp"]]);
    }

    #[test]
    fn precompiled_headers_are_turned_off_for_clang_tools() {
        let context = CompileContext::new(Path::new(r"C:\r"));
        let e =
            entries(CL, &args(r"/c /Yupch.h /Fpx64\Debug\App.pch /EHsc main.cpp"), &context, &mut DirCache::default());
        let a = &e[0].arguments;
        assert_eq!(a[3..6], ["/Yupch.h", "/Y-", r"/Fpx64\Debug\App.pch"], "{a:?}");

        let mut yc: Vec<String> = ["cl.exe", "-Ycpch.h", "pch.cpp"].map(String::from).into();
        disable_pch(&mut yc);
        assert_eq!(yc, ["cl.exe", "-Ycpch.h", "/Y-", "pch.cpp"]);
        // Once only, and nothing without a precompiled header.
        disable_pch(&mut yc);
        assert_eq!(yc.iter().filter(|a| *a == "/Y-").count(), 1);
        let mut plain: Vec<String> = ["cl.exe", "/c", "/Y-", "/Yd", "core.cpp"].map(String::from).into();
        let before = plain.clone();
        disable_pch(&mut plain);
        assert_eq!(plain, before);
    }

    #[test]
    fn nothing_to_compile_gives_no_entries() {
        let context = CompileContext::new(Path::new(r"C:\r"));
        assert!(entries(CL, &args("/c /nologo"), &context, &mut DirCache::default()).is_empty());
    }

    #[test]
    fn target_triples() {
        let none: [String; 0] = [];
        assert_eq!(target_triple(r"C:\VS\bin\HostX64\x86\cl.exe", &none, None), Some("i686-pc-windows-msvc"));
        assert_eq!(target_triple(r"C:\VS\bin\HostX86\x64\CL.exe", &none, None), Some("x86_64-pc-windows-msvc"));
        assert_eq!(target_triple(r"C:\VS\bin\Hostx64\arm64\cl.exe", &none, None), Some("aarch64-pc-windows-msvc"));
        assert_eq!(target_triple(r"C:\LLVM\bin\clang-cl.exe", &none, Some("Win32")), Some("i686-pc-windows-msvc"));
        assert_eq!(target_triple(r"C:\LLVM\bin\clang-cl.exe", &["-m32".to_string()], Some("Win32")), None);
        assert_eq!(target_triple(r"C:\LLVM\bin\clang-cl.exe", &["--target=x".to_string()], None), None);
        assert_eq!(
            target_triple(r"C:\VS\bin\Hostx64\arm64\cl.exe", &["/arm64EC".to_string()], None),
            Some("arm64ec-pc-windows-msvc")
        );
        assert_eq!(target_triple("cl.exe", &none, None), None);
    }

    #[test]
    fn include_path_becomes_system_includes() {
        let root = tempfile::tempdir().unwrap();
        let vendor = root.path().join("vendor sdk").join("include");
        std::fs::create_dir_all(&vendor).unwrap();
        let include_path = format!("{};;{}\\;C:\\does\\not\\exist", vendor.display(), vendor.display());
        let context = CompileContext {
            directory: root.path(),
            platform: Some("Win32"),
            include_path: Some(&include_path),
            external_include_path: Some(&include_path),
        };
        let e = entries("cl.exe", &args("/c /link /DEBUG a.c"), &context, &mut DirCache::default());
        // "a.c" after /link belongs to the linker, so nothing is compiled.
        assert!(e.is_empty());
        let e = entries("cl.exe", &args("/c a.c /link /DEBUG"), &context, &mut DirCache::default());
        let a = &e[0].arguments;
        let vendor_text = paths::normalize(&vendor).to_string_lossy().into_owned();
        assert_eq!(a[1], "--target=i686-pc-windows-msvc", "from the platform, since cl.exe has no target directory");
        assert_eq!(
            a.iter().filter(|x| x.starts_with("/imsvc")).count(),
            1,
            "deduplicated, missing dirs dropped: {a:?}"
        );
        assert_eq!(a[3], format!("/imsvc{vendor_text}"));
        assert_eq!(
            &a[a.len() - 3..],
            [paths::absolutize(root.path(), Path::new("a.c")).to_string_lossy().as_ref(), "/link", "/DEBUG"]
        );
    }
}
