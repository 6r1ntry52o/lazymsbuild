//! Windows command lines: splitting them into arguments and quoting arguments back.
//!
//! cl.exe parses its command line with the Microsoft C runtime rules, so those are the rules
//! used here ("Parsing C++ command-line arguments" in the Microsoft C++ documentation):
//! - arguments are separated by spaces and tabs;
//! - `"` starts and ends a quoted part, in which spaces are kept;
//! - inside a quoted part, `""` is one literal `"`;
//! - `2n` backslashes before a `"` are `n` backslashes, and the `"` quotes;
//! - `2n+1` backslashes before a `"` are `n` backslashes and a literal `"`;
//! - other backslashes are literal.

use std::borrow::Cow;

/// Splits the argument part of a command line (everything after the program).
pub fn split_args(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut args = Vec::new();
    let mut i = 0;
    loop {
        while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let mut arg = String::new();
        let mut quoted = false;
        while i < chars.len() {
            let c = chars[i];
            if !quoted && (c == ' ' || c == '\t') {
                break;
            }
            if c == '\\' {
                let start = i;
                while i < chars.len() && chars[i] == '\\' {
                    i += 1;
                }
                let n = i - start;
                if i < chars.len() && chars[i] == '"' {
                    arg.extend(std::iter::repeat_n('\\', n / 2));
                    if n % 2 == 1 {
                        arg.push('"');
                        i += 1;
                    }
                    // An even count leaves the quote for the branch below.
                } else {
                    arg.extend(std::iter::repeat_n('\\', n));
                }
                continue;
            }
            if c == '"' {
                if quoted && chars.get(i + 1) == Some(&'"') {
                    arg.push('"');
                    i += 2;
                } else {
                    quoted = !quoted;
                    i += 1;
                }
                continue;
            }
            arg.push(c);
            i += 1;
        }
        args.push(arg);
    }
    args
}

/// Splits a whole command line into the program and its arguments.
///
/// MSBuild logs tool command lines with the program path unquoted even when it contains spaces
/// (`C:\Program Files (x86)\...\CL.exe /c ...`), so an unquoted program runs to the first `.exe`
/// that is followed by whitespace or the end of the line.
pub fn split_command(s: &str) -> Option<(String, Vec<String>)> {
    let line = s.trim_start();
    if line.is_empty() {
        return None;
    }
    if let Some(rest) = line.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some((rest[..end].to_string(), split_args(&rest[end + 1..])));
    }
    let lower = line.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(".exe") {
        let end = from + pos + 4;
        let rest = &line[end..];
        if rest.is_empty() || rest.starts_with([' ', '\t']) {
            return Some((line[..end].to_string(), split_args(rest)));
        }
        from = end;
    }
    let end = line.find([' ', '\t']).unwrap_or(line.len());
    Some((line[..end].to_string(), split_args(&line[end..])))
}

/// Quotes one argument so that [`split_args`] gives it back unchanged.
pub fn quote_arg(arg: &str) -> Cow<'_, str> {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{0b}', '"']) {
        return Cow::Borrowed(arg);
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    Cow::Owned(out)
}

/// Joins arguments into a command line that [`split_args`] splits back into the same arguments.
pub fn join_args<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = String::new();
    for (i, arg) in args.into_iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&quote_arg(arg.as_ref()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_the_c_runtime() {
        assert_eq!(split_args(r#"/c /Iinclude main.cpp"#), ["/c", "/Iinclude", "main.cpp"]);
        assert_eq!(split_args(r#"/Fo"x64\Debug\\" a.cpp"#), [r"/Fox64\Debug\", "a.cpp"]);
        assert_eq!(split_args(r#""sub dir\extra file.c" util.c"#), [r"sub dir\extra file.c", "util.c"]);
        assert_eq!(split_args(r#"/D "A=\"x y\"" b"#), ["/D", r#"A="x y""#, "b"]);
        assert_eq!(split_args(r#"a\\\"b c\\d"#), [r#"a\"b"#, r"c\\d"]);
        assert_eq!(split_args(r#""a""b" """#), [r#"a"b"#, ""]);
        assert_eq!(split_args("  \t "), Vec::<String>::new());
        assert_eq!(split_args(r#""unterminated quote"#), ["unterminated quote"]);
    }

    #[test]
    fn splits_msbuild_tool_command_lines() {
        let line = r#"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64\CL.exe /c /ZI /D _DEBUG /Fo"x64\Debug\\" /TP core.cpp debug_only.cpp"#;
        let (program, args) = split_command(line).unwrap();
        assert_eq!(
            program,
            r"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64\CL.exe"
        );
        assert_eq!(args, ["/c", "/ZI", "/D", "_DEBUG", r"/Fox64\Debug\", "/TP", "core.cpp", "debug_only.cpp"]);

        let (program, args) = split_command(r#""C:\tools x\clang-cl.exe" -c a.cpp"#).unwrap();
        assert_eq!(program, r"C:\tools x\clang-cl.exe");
        assert_eq!(args, ["-c", "a.cpp"]);

        let (program, _) = split_command(r"C:\a.exe.dir\cl.exe /c x.cpp").unwrap();
        assert_eq!(program, r"C:\a.exe.dir\cl.exe");

        let (program, args) = split_command("cl /c x.cpp").unwrap();
        assert_eq!((program.as_str(), args.len()), ("cl", 2));
        assert_eq!(split_command("   "), None);
    }

    #[test]
    fn quoting_round_trips() {
        let cases = [
            "plain",
            "with space",
            r"trailing\",
            r"C:\dir with space\",
            r#"quote"inside"#,
            r#"back\"slash"#,
            "",
            r"\\server\share\x.cpp",
            "テスト ファイル.cpp",
        ];
        for case in cases {
            let line = join_args([case, "next"]);
            assert_eq!(split_args(&line), [case, "next"], "line: {line}");
        }
        assert_eq!(quote_arg("/Iinclude"), "/Iinclude");
        assert_eq!(quote_arg(r"C:\a b\"), r#""C:\a b\\""#);
    }
}
