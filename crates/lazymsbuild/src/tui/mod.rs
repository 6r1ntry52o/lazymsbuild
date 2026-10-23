//! The terminal UI (`lazymsbuild` without a command).

mod app;
mod keys;
mod tree;
#[cfg(test)]
mod ui_tests;
mod view;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event as TermEvent, EventStream, KeyEventKind};
use futures::StreamExt;
use lazymsbuild_core::{cmdline, fsutil};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use self::app::{App, EditorRequest};
use crate::cli::Global;

pub async fn run(global: &Global) -> Result<i32> {
    let workspace = crate::commands::open_workspace(global)?;
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel();
    let mut app = App::new(workspace, events_tx, done_tx)?;
    // Restores the terminal on panic, too.
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(&mut terminal, &mut app, &mut events_rx, &mut done_rx).await;
    ratatui::restore();
    result.map(|()| 0)
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    events: &mut mpsc::UnboundedReceiver<lazymsbuild_core::engine::Event>,
    done: &mut mpsc::UnboundedReceiver<app::Done>,
) -> Result<()> {
    let mut input = EventStream::new();
    loop {
        terminal.draw(|frame| view::draw(frame, app))?;
        // Redraw often only while something moves (the spinner, the elapsed time).
        let tick = if app.running.is_some() { Duration::from_millis(100) } else { Duration::from_secs(1) };
        tokio::select! {
            event = input.next() => match event {
                // Windows reports key releases too.
                Some(Ok(TermEvent::Key(key))) if key.kind != KeyEventKind::Release => app.on_key(key),
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(err.into()),
                None => break,
            },
            Some(event) = events.recv() => {
                app.on_event(event);
                // A build can print thousands of lines a second: take what is queued before drawing.
                while let Ok(event) = events.try_recv() {
                    app.on_event(event);
                }
            }
            Some(finished) = done.recv() => app.on_done(finished),
            _ = tokio::time::sleep(tick) => app.on_tick(),
        }
        if let Some(request) = app.editor_request.take() {
            open_editor(terminal, app, &request)?;
        }
        if app.quit {
            break;
        }
    }
    Ok(())
}

/// Opens a location: `ui.edit_at_line`, else `$EDITOR`, else `$VISUAL` (the terminal is handed
/// over while it runs), else the default application.
fn open_editor(terminal: &mut DefaultTerminal, app: &mut App, request: &EditorRequest) -> Result<()> {
    match editor_command(app.ws.config.ui.edit_at_line.as_deref(), request) {
        Some(mut command) => {
            ratatui::restore();
            let status = command.status();
            *terminal = ratatui::try_init()?;
            terminal.clear()?;
            match status {
                Ok(status) if status.success() => {}
                Ok(status) => app.notify(format!("the editor exited with {status}"), true),
                Err(err) => app.notify(format!("cannot start the editor: {err}"), true),
            }
        }
        None => match fsutil::open_with_default_app(&request.file) {
            Ok(()) => app.notify(format!("opened {}", request.file.display()), false),
            Err(err) => app.notify(format!("cannot open {}: {err}", request.file.display()), true),
        },
    }
    Ok(())
}

fn editor_command(template: Option<&str>, request: &EditorRequest) -> Option<Command> {
    let file = request.file.display().to_string();
    let fill = |arg: &str| {
        arg.replace("{file}", &file)
            .replace("{line}", &request.line.to_string())
            .replace("{column}", &request.column.to_string())
    };
    let (program, args) = match template.map(str::trim).filter(|t| !t.is_empty()) {
        Some(template) => {
            let parts: Vec<String> = cmdline::split_args(template).iter().map(|a| fill(a)).collect();
            let (program, rest) = parts.split_first()?;
            (program.clone(), rest.to_vec())
        }
        None => {
            let editor =
                ["EDITOR", "VISUAL"].iter().find_map(|v| std::env::var(v).ok().filter(|e| !e.trim().is_empty()))?;
            let parts = cmdline::split_args(&editor);
            let (program, base) = parts.split_first()?;
            let mut args = base.to_vec();
            args.extend(at_line_args(program, &file, request.line, request.column));
            (program.clone(), args)
        }
    };
    // `code` is code.cmd: find it with PATHEXT, which Command alone does not try.
    let resolved = if Path::new(&program).components().count() > 1 {
        program.into()
    } else {
        fsutil::which(&program).unwrap_or(program.into())
    };
    let mut command = Command::new(resolved);
    command.args(args);
    Some(command)
}

/// How common editors take "open at a line".
fn at_line_args(program: &str, file: &str, line: u32, column: u32) -> Vec<String> {
    let name = Path::new(program).file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match name.as_str() {
        "nvim" | "vim" | "vi" | "gvim" | "nvim-qt" | "nano" | "micro" | "emacs" | "emacsclient" | "kak" => {
            vec![format!("+{line}"), file.to_string()]
        }
        "code" | "code-insiders" | "codium" | "cursor" => vec!["-g".into(), format!("{file}:{line}:{column}")],
        "subl" | "sublime_text" | "hx" | "helix" | "zed" => vec![format!("{file}:{line}:{column}")],
        "notepad++" => vec![format!("-n{line}"), format!("-c{column}"), file.to_string()],
        _ => vec![file.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editors_open_at_the_line() {
        assert_eq!(at_line_args(r"C:\Program Files\Neovim\bin\nvim.exe", "a.cpp", 3, 5), ["+3", "a.cpp"]);
        assert_eq!(at_line_args("code", "a.cpp", 3, 5), ["-g", "a.cpp:3:5"]);
        assert_eq!(at_line_args("notepad", "a.cpp", 3, 5), ["a.cpp"]);
    }

    #[test]
    fn templates_fill_placeholders() {
        let request = EditorRequest { file: r"C:\src\dir with space\a.cpp".into(), line: 7, column: 13 };
        let command = editor_command(Some(r#"myedit --goto "{file}:{line}:{column}""#), &request).unwrap();
        let args: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["--goto", r"C:\src\dir with space\a.cpp:7:13"]);
    }
}
