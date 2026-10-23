//! Text output: colors only on a terminal and never with `NO_COLOR` (<https://no-color.org>),
//! and [`say!`], which is `println!` that ignores a closed pipe (`lazymsbuild check | head`).

use std::io::IsTerminal;
use std::sync::OnceLock;

/// `println!` that does not panic when standard output is gone.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout().lock(), $($arg)*);
    }};
}
pub(crate) use say;

static ENABLED: OnceLock<bool> = OnceLock::new();

/// Whether to color standard output.
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        // Turns on VT processing for an old console host; false when it cannot.
        !no_color && std::io::stdout().is_terminal() && ratatui::crossterm::ansi_support::supports_ansi()
    })
}

fn paint(code: &str, text: &str) -> String {
    if enabled() { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() }
}

pub fn red(text: &str) -> String {
    paint("31", text)
}

pub fn green(text: &str) -> String {
    paint("32", text)
}

pub fn yellow(text: &str) -> String {
    paint("33", text)
}

pub fn dim(text: &str) -> String {
    paint("2", text)
}

pub fn bold(text: &str) -> String {
    paint("1", text)
}
