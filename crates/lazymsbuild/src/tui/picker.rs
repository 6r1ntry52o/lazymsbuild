//! The list shown before the TUI proper when a repository has several solutions. It runs before a
//! workspace exists, because nearly everything in the TUI depends on the solution.

use std::path::PathBuf;

use anyhow::Result;
use crossterm::event::{Event, KeyEvent, KeyEventKind};
use lazymsbuild_core::paths;
use lazymsbuild_core::workspace::SolutionChoice;
use ratatui::DefaultTerminal;

use super::keys::{Action, Keymap};
use super::view;

/// What a key did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picked {
    Solution(usize),
    Quit,
}

pub struct Picker {
    /// The paths, relative to the repository root where they can be.
    pub names: Vec<String>,
    pub cursor: usize,
    pub last_used: Option<usize>,
    pub keymap: Keymap,
    /// Lines a page moves (set from the list height while drawing).
    pub page: usize,
}

impl Picker {
    pub fn new(choice: &SolutionChoice) -> Picker {
        // A bad `[keys]` entry is reported by the TUI proper.
        let (keymap, _warnings) = Keymap::new(&choice.keys);
        Picker {
            names: choice.solutions.iter().map(|path| paths::display_relative(path, &choice.root)).collect(),
            cursor: choice.preselected,
            last_used: choice.last_used,
            keymap,
            page: 1,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Picked> {
        let last = self.names.len().saturating_sub(1);
        match self.keymap.action(&key)? {
            Action::Up => self.cursor = self.cursor.saturating_sub(1),
            Action::Down => self.cursor = (self.cursor + 1).min(last),
            Action::Top => self.cursor = 0,
            Action::Bottom => self.cursor = last,
            Action::PageUp => self.cursor = self.cursor.saturating_sub(self.page.max(1)),
            Action::PageDown => self.cursor = (self.cursor + self.page.max(1)).min(last),
            Action::Select => return Some(Picked::Solution(self.cursor)),
            Action::Quit | Action::Close | Action::Cancel => return Some(Picked::Quit),
            _ => {}
        }
        None
    }
}

/// Asks which solution to use; `None` when the user quit.
pub fn run(choice: &SolutionChoice) -> Result<Option<PathBuf>> {
    let mut picker = Picker::new(choice);
    let mut terminal = ratatui::try_init()?;
    let picked = pick(&mut terminal, &mut picker);
    ratatui::restore();
    Ok(match picked? {
        Picked::Solution(index) => choice.solutions.get(index).cloned(),
        Picked::Quit => None,
    })
}

fn pick(terminal: &mut DefaultTerminal, picker: &mut Picker) -> Result<Picked> {
    loop {
        terminal.draw(|frame| view::draw_picker(frame, picker))?;
        // Nothing else runs yet, so waiting here holds nothing up.
        if let Event::Key(key) = crossterm::event::read()?
            // Windows reports key releases too.
            && key.kind != KeyEventKind::Release
            && let Some(picked) = picker.on_key(key)
        {
            return Ok(picked);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crossterm::event::{KeyCode, KeyEventState, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn picker(count: usize, preselected: usize) -> Picker {
        let solutions: Vec<PathBuf> = (0..count).map(|i| PathBuf::from(format!(r"C:\repo\sub\S{i}.sln"))).collect();
        Picker::new(&SolutionChoice {
            solutions,
            preselected,
            last_used: Some(preselected),
            root: PathBuf::from(r"C:\repo"),
            keys: BTreeMap::new(),
        })
    }

    fn press(picker: &mut Picker, code: KeyCode) -> Option<Picked> {
        picker.on_key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    fn screen(picker: &mut Picker, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| view::draw_picker(frame, picker)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn keys_move_within_the_list_and_choose() {
        let mut p = picker(3, 1);
        assert_eq!(p.cursor, 1);
        assert_eq!(press(&mut p, KeyCode::Char('j')), None);
        assert_eq!(press(&mut p, KeyCode::Down), None);
        assert_eq!(p.cursor, 2, "stops at the end");
        press(&mut p, KeyCode::Char('g'));
        assert_eq!(p.cursor, 0);
        press(&mut p, KeyCode::Up);
        assert_eq!(p.cursor, 0, "stops at the top");
        press(&mut p, KeyCode::Char('G'));
        assert_eq!(press(&mut p, KeyCode::Enter), Some(Picked::Solution(2)));
    }

    #[test]
    fn quitting_chooses_nothing() {
        for code in [KeyCode::Char('q'), KeyCode::Esc] {
            assert_eq!(press(&mut picker(2, 0), code), Some(Picked::Quit));
        }
    }

    #[test]
    fn keys_that_mean_nothing_here_do_nothing() {
        let mut p = picker(2, 0);
        for code in [KeyCode::Char('b'), KeyCode::Char('x'), KeyCode::Char('?')] {
            assert_eq!(press(&mut p, code), None);
        }
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn shows_the_paths_and_the_one_used_last() {
        let text = screen(&mut picker(3, 1), 80, 20);
        assert!(text.contains("Choose a solution"), "{text}");
        assert!(text.contains(r"sub\S0.sln"), "{text}");
        assert!(!text.contains(r"C:\repo"), "paths are relative to the repository: {text}");
        let marked: Vec<&str> = text.lines().filter(|line| line.contains("last used")).collect();
        assert_eq!(marked.len(), 1, "{text}");
        assert!(marked[0].contains(r"sub\S1.sln"), "{text}");
    }

    #[test]
    fn a_long_list_follows_the_cursor_and_pages_by_its_height() {
        let mut p = picker(50, 0);
        press(&mut p, KeyCode::Char('G'));
        let text = screen(&mut p, 80, 12);
        assert!(text.contains(r"S49.sln"), "{text}");
        assert!(!text.contains(r"S0.sln"), "{text}");
        assert!(p.page > 1 && p.page < 12, "page {}", p.page);

        let page = p.page;
        press(&mut p, KeyCode::Char(','));
        assert_eq!(p.cursor, 49 - page);
    }
}
