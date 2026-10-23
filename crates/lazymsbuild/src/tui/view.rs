//! Drawing the TUI: the five panes of the spec (§5-5), the key hint line, and the popups. Below
//! 70 columns only the focused pane is shown. With `NO_COLOR`, emphasis uses bold and dim only.

use lazymsbuild_core::check::SyncState;
use lazymsbuild_core::diag::Severity;
use lazymsbuild_core::init;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};

use super::app::{ACTIONS, App, LineKind, LogTab, MenuItem, Pane, Popup};
use super::keys::Action;
use super::tree::RowKind;

struct Theme {
    color: bool,
}

impl Theme {
    fn new() -> Theme {
        Theme { color: std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()) }
    }

    fn fg(&self, color: Color) -> Style {
        if self.color { Style::new().fg(color) } else { Style::new() }
    }

    fn focused_border(&self) -> Style {
        if self.color {
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        }
    }

    fn error(&self) -> Style {
        if self.color { Style::new().fg(Color::Red) } else { Style::new().add_modifier(Modifier::BOLD) }
    }

    fn warning(&self) -> Style {
        self.fg(Color::Yellow)
    }

    fn dim(&self) -> Style {
        if self.color { Style::new().fg(Color::DarkGray) } else { Style::new().add_modifier(Modifier::DIM) }
    }

    fn selected(&self) -> Style {
        self.fg(Color::Green).add_modifier(Modifier::BOLD)
    }

    fn cursor(&self, focused: bool) -> Style {
        if focused {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().add_modifier(Modifier::UNDERLINED)
        }
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let theme = Theme::new();
    let [main, hint] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());
    if main.width < 70 {
        let pane = app.focus;
        draw_pane(frame, app, &theme, pane, main);
    } else {
        let [left, right] = Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(main);
        let configs_height = (app.profiles.len() as u16 + 2).clamp(3, 9);
        let [targets, configs, actions] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(configs_height),
            Constraint::Length(ACTIONS.len() as u16 + 2),
        ])
        .areas(left);
        let [log, db] = Layout::vertical([Constraint::Min(4), Constraint::Length(7)]).areas(right);
        for (pane, area) in [
            (Pane::Targets, targets),
            (Pane::Configs, configs),
            (Pane::Actions, actions),
            (Pane::Log, log),
            (Pane::Db, db),
        ] {
            draw_pane(frame, app, &theme, pane, area);
        }
    }
    draw_hint(frame, app, &theme, hint);
    if app.popup.is_some() {
        draw_popup(frame, app, &theme);
    }
}

fn pane_block<'a>(title: Line<'a>, focused: bool, theme: &Theme) -> Block<'a> {
    let block = Block::bordered().title(title).border_type(BorderType::Rounded);
    if focused { block.border_style(theme.focused_border()) } else { block }
}

fn draw_pane(frame: &mut Frame, app: &mut App, theme: &Theme, pane: Pane, area: Rect) {
    let focused = app.focus == pane;
    if focused {
        app.page = area.height.saturating_sub(2) as usize;
    }
    match pane {
        Pane::Targets => draw_targets(frame, app, theme, area, focused),
        Pane::Configs => draw_configs(frame, app, theme, area, focused),
        Pane::Actions => draw_actions(frame, app, theme, area, focused),
        Pane::Log => draw_log(frame, app, theme, area, focused),
        Pane::Db => draw_db(frame, app, theme, area, focused),
    }
}

fn draw_targets(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, focused: bool) {
    let mut title =
        vec![Span::raw("[1] Solution & Targets"), Span::styled(format!(" · {}", app.target_label()), theme.dim())];
    if app.filtering || !app.filter.is_empty() {
        title.push(Span::styled(format!(" /{}{}", app.filter, if app.filtering { "_" } else { "" }), theme.warning()));
    }
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| {
            let (selected, style, note) = match row.kind {
                RowKind::Solution => (app.target.is_none(), Style::new().add_modifier(Modifier::BOLD), None),
                RowKind::Folder(_) => (false, theme.dim(), None),
                RowKind::Project(index) => {
                    let project = &app.ws.solution.projects[index];
                    let selected = app.target == Some(index);
                    if !project.builds_in(&app.active) {
                        (selected, theme.dim(), Some(" (not built)"))
                    } else {
                        (selected, if selected { theme.selected() } else { Style::new() }, None)
                    }
                }
            };
            let style = if selected { style.patch(theme.selected()) } else { style };
            let mut spans = vec![Span::styled(row.prefix.clone(), theme.dim()), Span::styled(row.label.clone(), style)];
            if selected {
                spans.push(Span::styled(" ●", theme.selected()));
            }
            if let Some(note) = note {
                spans.push(Span::styled(note, theme.dim()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(pane_block(Line::from(title), focused, theme))
        .highlight_style(theme.cursor(focused))
        .highlight_symbol("> ");
    let mut state = ListState::default().with_selected(Some(app.tree_cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_configs(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, focused: bool) {
    let width = app.profiles.iter().map(|p| p.configuration.chars().count()).max().unwrap_or(0);
    let items: Vec<ListItem> = app
        .profiles
        .iter()
        .map(|p| {
            let active = *p == app.active;
            let text = format!("{} {:width$} | {}", if active { "*" } else { " " }, p.configuration, p.platform);
            ListItem::new(Line::styled(text, if active { theme.selected() } else { Style::new() }))
        })
        .collect();
    let list = List::new(items)
        .block(pane_block(Line::raw("[2] Configuration / Platform"), focused, theme))
        .highlight_style(theme.cursor(focused));
    let mut state = ListState::default().with_selected(Some(app.config_cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_actions(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, focused: bool) {
    let busy = app.running.is_some();
    let items: Vec<ListItem> = ACTIONS
        .iter()
        .map(|action| {
            let key = app.keymap.label(*action);
            let label = match action {
                Action::Build => "Build".to_string(),
                Action::Rebuild => "Rebuild".to_string(),
                Action::Clean => "Clean".to_string(),
                Action::Sync => "Sync (no build)".to_string(),
                Action::Parallel => format!("Parallel (-m) : {}", if app.parallel { "ON" } else { "OFF" }),
                _ => String::new(),
            };
            let disabled = busy && *action != Action::Parallel;
            let line = Line::from(vec![Span::styled(format!("{key:>2} "), theme.warning()), Span::raw(label)]);
            ListItem::new(if disabled { line.style(theme.dim()) } else { line })
        })
        .collect();
    let list = List::new(items)
        .block(pane_block(Line::raw("[3] Actions & Options"), focused, theme))
        .highlight_style(theme.cursor(focused));
    let mut state = ListState::default().with_selected(Some(app.action_cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_log(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, focused: bool) {
    let tab = |name: String, active: bool| {
        if active {
            Span::styled(name, Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED))
        } else {
            Span::raw(name)
        }
    };
    let (errors, warnings) = match &app.running {
        Some(r) if app.diagnostics.is_empty() => (r.errors as usize, r.warnings as usize),
        _ => (app.errors(), app.warnings()),
    };
    let mut title = vec![
        Span::raw("[4] "),
        tab("Log".into(), app.tab == LogTab::Log),
        Span::raw(" | "),
        tab(format!("Errors (E:{errors} W:{warnings})"), app.tab == LogTab::Errors),
    ];
    if app.tab == LogTab::Log && app.log_scroll > 0 {
        title.push(Span::styled(
            format!(" ↑{} ({} follows)", app.log_scroll, app.keymap.label(Action::Bottom)),
            theme.dim(),
        ));
    }
    let block = pane_block(Line::from(title), focused, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match app.tab {
        LogTab::Log => {
            let height = inner.height as usize;
            let end = app.log.len().saturating_sub(app.log_scroll);
            let start = end.saturating_sub(height);
            let lines: Vec<Line> = app
                .log
                .range(start..end)
                .map(|l| {
                    let style = match l.kind {
                        LineKind::Normal => Style::new(),
                        LineKind::Command => theme.dim(),
                        LineKind::Note => theme.fg(Color::Cyan),
                        LineKind::Error => theme.error(),
                        LineKind::Warning => theme.warning(),
                    };
                    Line::styled(l.text.clone(), style)
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
        LogTab::Errors => {
            if app.diagnostics.is_empty() {
                frame.render_widget(Paragraph::new(Line::styled("no errors or warnings", theme.dim())), inner);
                return;
            }
            let detail_height = if inner.height > 8 { 4 } else { 0 };
            let [list_area, detail_area] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(detail_height)]).areas(inner);
            let items: Vec<ListItem> = app
                .diagnostics
                .iter()
                .map(|d| {
                    let (word, style) = match d.severity {
                        Severity::Error => ("error  ", theme.error()),
                        Severity::Warning => ("warning", theme.warning()),
                    };
                    let place = match (&d.file, d.line) {
                        (Some(file), Some(line)) => format!("{}({line})", file_name(file)),
                        (Some(file), None) => file_name(file),
                        (None, _) => d.origin.clone().unwrap_or_default(),
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(word, style),
                        Span::raw(" "),
                        Span::styled(format!("{:<8}", d.code.clone().unwrap_or_default()), style),
                        Span::styled(format!("{place}: "), theme.dim()),
                        Span::raw(d.message.clone()),
                    ]))
                })
                .collect();
            let list = List::new(items).highlight_style(theme.cursor(focused));
            let mut state = ListState::default().with_selected(Some(app.diag_cursor));
            frame.render_stateful_widget(list, list_area, &mut state);
            if detail_height > 0 {
                if let Some(d) = app.diagnostics.get(app.diag_cursor) {
                    let mut text = Text::default();
                    text.push_line(Line::styled(d.to_msvc(), Style::new()));
                    if d.file.is_some() {
                        text.push_line(Line::styled(
                            format!("{} opens it", app.keymap.label(Action::Edit)),
                            theme.dim(),
                        ));
                    }
                    let detail = Paragraph::new(text)
                        .wrap(Wrap { trim: false })
                        .block(Block::new().borders(ratatui::widgets::Borders::TOP).border_style(theme.dim()));
                    frame.render_widget(detail, detail_area);
                }
            }
        }
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

fn draw_db(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, focused: bool) {
    let mut title = vec![Span::raw("[5] clangd DB Status")];
    if app.checking {
        title.push(Span::styled(" checking…", theme.dim()));
    }
    let block = pane_block(Line::from(title), focused, theme);
    let label = |text: &str| Span::styled(format!("{text:<9}: "), theme.dim());
    let mode = init::mode_name(app.ws.output_mode());
    let lines: Vec<Line> = match &app.check {
        Some(c) if c.database_exists => {
            let delta = app.delta.map(|d| format!(" ({d:+})")).unwrap_or_default();
            let coverage_style = if c.coverage.covered < c.coverage.total { theme.error() } else { Style::new() };
            let stale = if c.stale.is_empty() {
                "none".to_string()
            } else {
                c.stale.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", ")
            };
            let source = c.methods.iter().map(|(m, n)| format!("{m} {n}")).collect::<Vec<_>>().join(" + ");
            let (sync_text, sync_style) = match c.sync {
                SyncState::InSync => ("✔ published".to_string(), theme.fg(Color::Green)),
                SyncState::OutOfSync => ("✘ differs (another tool wrote it?)".to_string(), theme.error()),
                SyncState::NoOutput => ("✘ not published".to_string(), theme.error()),
                SyncState::NoDatabase => ("–".to_string(), theme.dim()),
            };
            let column = |left: Vec<Span<'static>>, right: Vec<Span<'static>>| {
                let mut spans = left;
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                spans.push(Span::raw(" ".repeat(32usize.saturating_sub(used))));
                spans.extend(right);
                Line::from(spans)
            };
            vec![
                column(vec![label("Profile"), Span::raw(app.active.to_string())], vec![label("Mode"), Span::raw(mode)]),
                column(
                    vec![label("Entries"), Span::raw(format!("{}{delta}", c.entries))],
                    vec![
                        label("Coverage"),
                        Span::styled(format!("{}/{}", c.coverage.covered, c.coverage.total), coverage_style),
                    ],
                ),
                column(
                    vec![
                        label("Stale"),
                        Span::styled(stale, if c.stale.is_empty() { Style::new() } else { theme.warning() }),
                    ],
                    vec![label("Updated"), Span::raw(app.db_updated().unwrap_or_else(|| "?".into()))],
                ),
                column(
                    vec![label("Source"), Span::raw(source)],
                    vec![label("Output"), Span::styled(sync_text, sync_style)],
                ),
                Line::from(vec![label("File"), Span::styled(c.output.clone(), theme.dim())]),
            ]
        }
        Some(_) => vec![
            Line::from(vec![label("Profile"), Span::raw(app.active.to_string())]),
            Line::styled(format!("No database for {} yet.", app.active), theme.warning()),
            Line::raw(format!(
                "{}: sync (no build needed)   {}: rebuild to fill it from a build",
                app.keymap.label(Action::Sync),
                app.keymap.label(Action::Rebuild)
            )),
        ],
        None => vec![Line::styled("checking the database…", theme.dim())],
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_hint(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let key = |action: Action| app.keymap.label(action);
    let line = if app.filtering {
        Line::from(vec![
            Span::styled("filter: ", theme.warning()),
            Span::raw("type to filter · Enter keeps it · Esc clears it"),
        ])
    } else if let Some(running) = &app.running {
        let elapsed = running.started.elapsed().as_secs();
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(running.started.elapsed().as_millis() / 100 % 10) as usize];
        let state = if running.cancelling { "cancelling" } else { "running" };
        Line::from(vec![
            Span::styled(
                format!(
                    "{spinner} {} ({}) {state} {}:{:02} ",
                    running.label,
                    running.profile,
                    elapsed / 60,
                    elapsed % 60
                ),
                theme.warning(),
            ),
            Span::styled(format!("· {} cancel · {} help", key(Action::Cancel), key(Action::Help)), theme.dim()),
        ])
    } else if let Some(notice) = &app.notice {
        Line::styled(notice.text.clone(), if notice.error { theme.error() } else { theme.fg(Color::Green) })
    } else {
        let narrow = if frame.area().width < 70 { " · 1-5 pane" } else { "" };
        Line::styled(
            format!(
                "{} build · {} rebuild · {} clean · {} sync · {} menu · {} help · {} quit{narrow}",
                key(Action::Build),
                key(Action::Rebuild),
                key(Action::Clean),
                key(Action::Sync),
                key(Action::Menu),
                key(Action::Help),
                key(Action::Quit)
            ),
            theme.dim(),
        )
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(10);
    let height = height.min(area.height.saturating_sub(2)).max(3);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn draw_popup(frame: &mut Frame, app: &App, theme: &Theme) {
    let area = frame.area();
    let Some(popup) = &app.popup else { return };
    let block = |title: String| {
        Block::bordered().border_type(BorderType::Rounded).border_style(theme.focused_border()).title(Line::from(title))
    };
    match popup {
        Popup::Help { scroll } => {
            let lines: Vec<Line> = Action::ALL
                .iter()
                .map(|a| {
                    Line::from(vec![
                        Span::styled(format!("{:<22}", app.keymap.labels(*a)), theme.warning()),
                        Span::raw(a.description()),
                    ])
                })
                .collect();
            let rect = centered(area, 76, lines.len() as u16 + 2);
            let max_scroll = (lines.len() as u16).saturating_sub(rect.height.saturating_sub(2));
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(lines)
                    .block(block(format!("Keys ({} closes)", app.keymap.label(Action::Close))))
                    .scroll(((*scroll as u16).min(max_scroll), 0)),
                rect,
            );
        }
        Popup::Menu { cursor } => {
            let items: Vec<ListItem> = MenuItem::ALL
                .iter()
                .map(|item| {
                    let key = item.action().map(|a| app.keymap.label(a)).unwrap_or_default();
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{key:>3}  "), theme.warning()),
                        Span::raw(item.label()),
                    ]))
                })
                .collect();
            let rect = centered(area, 60, MenuItem::ALL.len() as u16 + 2);
            frame.render_widget(Clear, rect);
            let list = List::new(items).block(block("Actions".into())).highlight_style(theme.cursor(true));
            let mut state = ListState::default().with_selected(Some(*cursor));
            frame.render_stateful_widget(list, rect, &mut state);
        }
        Popup::Confirm { message, .. } => {
            let mut text = Text::from(message.lines().map(|l| Line::raw(l.to_string())).collect::<Vec<_>>());
            text.push_line(Line::raw(""));
            text.push_line(Line::from(vec![
                Span::styled("y", theme.warning()),
                Span::raw(" yes   "),
                Span::styled("n", theme.warning()),
                Span::raw(" no"),
            ]));
            let height = wrapped_height(&text, 66) + 2;
            let rect = centered(area, 70, height);
            frame.render_widget(Clear, rect);
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).block(block("Confirm".into())), rect);
        }
        Popup::Quit { cursor } => {
            let items = vec![ListItem::new("Cancel the build and quit"), ListItem::new("Keep waiting")];
            let rect = centered(area, 44, 4);
            frame.render_widget(Clear, rect);
            let list = List::new(items).block(block("A build is running".into())).highlight_style(theme.cursor(true));
            let mut state = ListState::default().with_selected(Some(*cursor));
            frame.render_stateful_widget(list, rect, &mut state);
        }
        Popup::Message { title, lines } => {
            let mut text = Text::from(lines.iter().map(|l| Line::raw(l.clone())).collect::<Vec<_>>());
            text.push_line(Line::raw(""));
            text.push_line(Line::styled(format!("{} closes", app.keymap.label(Action::Close)), theme.dim()));
            let height = wrapped_height(&text, 76) + 2;
            let rect = centered(area, 80, height);
            frame.render_widget(Clear, rect);
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).block(block(title.clone())), rect);
        }
    }
}

/// Lines `text` takes when wrapped at `width` columns (an estimate for sizing popups).
fn wrapped_height(text: &Text<'_>, width: usize) -> u16 {
    text.lines
        .iter()
        .map(|l| {
            let w: usize = l.spans.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref())).sum();
            w.div_ceil(width.max(1)).max(1)
        })
        .sum::<usize>() as u16
}
