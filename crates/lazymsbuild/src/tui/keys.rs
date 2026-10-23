//! Key bindings: the actions, their defaults (lazygit's where lazygit has one), and the `[keys]`
//! settings that replace them.

use std::collections::{BTreeMap, HashMap};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lazymsbuild_core::config::Keys;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    Pane1,
    Pane2,
    Pane3,
    Pane4,
    Pane5,
    NextPane,
    PrevPane,
    Up,
    Down,
    Top,
    Bottom,
    PageUp,
    PageDown,
    PrevTab,
    NextTab,
    Select,
    Filter,
    Help,
    Menu,
    Build,
    Rebuild,
    Clean,
    Sync,
    Parallel,
    Edit,
    Cancel,
    Close,
    Quit,
}

impl Action {
    pub const ALL: [Action; 28] = [
        Action::Pane1,
        Action::Pane2,
        Action::Pane3,
        Action::Pane4,
        Action::Pane5,
        Action::NextPane,
        Action::PrevPane,
        Action::Up,
        Action::Down,
        Action::Top,
        Action::Bottom,
        Action::PageUp,
        Action::PageDown,
        Action::PrevTab,
        Action::NextTab,
        Action::Select,
        Action::Filter,
        Action::Help,
        Action::Menu,
        Action::Build,
        Action::Rebuild,
        Action::Clean,
        Action::Sync,
        Action::Parallel,
        Action::Edit,
        Action::Cancel,
        Action::Close,
        Action::Quit,
    ];

    /// The name in `[keys]`.
    pub fn name(self) -> &'static str {
        match self {
            Action::Pane1 => "pane_1",
            Action::Pane2 => "pane_2",
            Action::Pane3 => "pane_3",
            Action::Pane4 => "pane_4",
            Action::Pane5 => "pane_5",
            Action::NextPane => "next_pane",
            Action::PrevPane => "prev_pane",
            Action::Up => "up",
            Action::Down => "down",
            Action::Top => "top",
            Action::Bottom => "bottom",
            Action::PageUp => "page_up",
            Action::PageDown => "page_down",
            Action::PrevTab => "prev_tab",
            Action::NextTab => "next_tab",
            Action::Select => "select",
            Action::Filter => "filter",
            Action::Help => "help",
            Action::Menu => "menu",
            Action::Build => "build",
            Action::Rebuild => "rebuild",
            Action::Clean => "clean",
            Action::Sync => "sync",
            Action::Parallel => "parallel",
            Action::Edit => "edit",
            Action::Cancel => "cancel",
            Action::Close => "close",
            Action::Quit => "quit",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Action::Pane1 => "Solution & Targets",
            Action::Pane2 => "Configuration / Platform",
            Action::Pane3 => "Actions & Options",
            Action::Pane4 => "Log | Errors",
            Action::Pane5 => "clangd DB Status",
            Action::NextPane => "Next pane",
            Action::PrevPane => "Previous pane",
            Action::Up => "Up",
            Action::Down => "Down",
            Action::Top => "Top",
            Action::Bottom => "Bottom",
            Action::PageUp => "Half page up",
            Action::PageDown => "Half page down",
            Action::PrevTab => "Previous tab (Log / Errors)",
            Action::NextTab => "Next tab (Log / Errors)",
            Action::Select => "Select the target or configuration; run the action",
            Action::Filter => "Filter the targets",
            Action::Help => "Keys",
            Action::Menu => "All actions",
            Action::Build => "Build",
            Action::Rebuild => "Rebuild",
            Action::Clean => "Clean (asks first)",
            Action::Sync => "Sync: fill the database without building",
            Action::Parallel => "Parallel builds (-m) on/off",
            Action::Edit => "Open the selected error in the editor",
            Action::Cancel => "Cancel the build (asks first); quit when idle",
            Action::Close => "Close the filter or the menu",
            Action::Quit => "Quit (asks first during a build)",
        }
    }

    fn defaults(self) -> &'static [&'static str] {
        match self {
            Action::Pane1 => &["1"],
            Action::Pane2 => &["2"],
            Action::Pane3 => &["3"],
            Action::Pane4 => &["4"],
            Action::Pane5 => &["5"],
            Action::NextPane => &["tab", "l"],
            Action::PrevPane => &["shift+tab", "h"],
            Action::Up => &["k", "up"],
            Action::Down => &["j", "down"],
            Action::Top => &["g", "home"],
            Action::Bottom => &["G", "end"],
            Action::PageUp => &["ctrl+u", "pageup"],
            Action::PageDown => &["ctrl+d", "pagedown"],
            Action::PrevTab => &["["],
            Action::NextTab => &["]"],
            Action::Select => &["enter", "space"],
            Action::Filter => &["/"],
            Action::Help => &["?"],
            Action::Menu => &["x"],
            Action::Build => &["b"],
            Action::Rebuild => &["r"],
            Action::Clean => &["c"],
            Action::Sync => &["s"],
            Action::Parallel => &["m"],
            Action::Edit => &["e"],
            Action::Cancel => &["ctrl+c"],
            Action::Close => &["esc"],
            Action::Quit => &["q"],
        }
    }
}

/// A key as bindings compare it. For characters the case carries Shift (`G`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    code: KeyCode,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

impl Key {
    pub fn from_event(event: &KeyEvent) -> Key {
        let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
        let alt = event.modifiers.contains(KeyModifiers::ALT);
        match event.code {
            KeyCode::Char(c) => Key { code: KeyCode::Char(c), ctrl, alt, shift: false },
            KeyCode::BackTab => Key { code: KeyCode::Tab, ctrl, alt, shift: true },
            code => Key { code, ctrl, alt, shift: event.modifiers.contains(KeyModifiers::SHIFT) },
        }
    }

    /// Parses `b`, `G`, `?`, `ctrl+u`, `shift+tab`, `enter`, `space`, `esc`, `up`, `f5`, ...
    pub fn parse(spec: &str) -> Result<Key, String> {
        let mut key = Key { code: KeyCode::Null, ctrl: false, alt: false, shift: false };
        let parts: Vec<&str> = if spec == "+" { vec!["+"] } else { spec.split('+').collect() };
        let (name, modifiers) = parts.split_last().ok_or_else(|| "empty key".to_string())?;
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => key.ctrl = true,
                "alt" => key.alt = true,
                "shift" => key.shift = true,
                other => return Err(format!("unknown modifier `{other}`")),
            }
        }
        let lower = name.to_ascii_lowercase();
        key.code = match lower.as_str() {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "space" => KeyCode::Char(' '),
            "backspace" => KeyCode::Backspace,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
                KeyCode::F(f[1..].parse().unwrap())
            }
            _ => {
                let mut chars = name.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => {
                        // `shift+g` is `G`; for characters the case is the shift.
                        let c = if key.shift && c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c };
                        key.shift = false;
                        if key.ctrl { KeyCode::Char(c.to_ascii_lowercase()) } else { KeyCode::Char(c) }
                    }
                    _ => return Err(format!("unknown key `{name}`")),
                }
            }
        };
        Ok(key)
    }

    /// How the key is shown: `b`, `G`, `Ctrl+u`, `Shift+Tab`, `Enter`, `↑`.
    pub fn label(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        match self.code {
            KeyCode::Char(' ') => out.push_str("Space"),
            KeyCode::Char(c) => out.push(c),
            KeyCode::Enter => out.push_str("Enter"),
            KeyCode::Esc => out.push_str("Esc"),
            KeyCode::Tab => out.push_str("Tab"),
            KeyCode::Backspace => out.push_str("Backspace"),
            KeyCode::Up => out.push('↑'),
            KeyCode::Down => out.push('↓'),
            KeyCode::Left => out.push('←'),
            KeyCode::Right => out.push('→'),
            KeyCode::Home => out.push_str("Home"),
            KeyCode::End => out.push_str("End"),
            KeyCode::PageUp => out.push_str("PgUp"),
            KeyCode::PageDown => out.push_str("PgDn"),
            KeyCode::Delete => out.push_str("Del"),
            KeyCode::Insert => out.push_str("Ins"),
            KeyCode::F(n) => out.push_str(&format!("F{n}")),
            other => out.push_str(&format!("{other:?}")),
        }
        out
    }
}

/// The bindings in effect.
#[derive(Clone, Debug)]
pub struct Keymap {
    by_key: HashMap<Key, Action>,
    by_action: BTreeMap<Action, Vec<Key>>,
}

impl Keymap {
    /// The defaults with `[keys]` applied. Problems are returned as warnings; a bad entry keeps the
    /// defaults of its action.
    pub fn new(overrides: &BTreeMap<String, Keys>) -> (Keymap, Vec<String>) {
        let mut warnings = Vec::new();
        let mut by_action: BTreeMap<Action, Vec<Key>> = BTreeMap::new();
        for action in Action::ALL {
            let defaults = action.defaults().iter().map(|s| Key::parse(s).expect("built-in key")).collect();
            by_action.insert(action, defaults);
        }
        for (name, keys) in overrides {
            let Some(action) = Action::ALL.iter().copied().find(|a| a.name() == name) else {
                warnings.push(format!("[keys]: unknown action `{name}`"));
                continue;
            };
            let parsed: Result<Vec<Key>, String> = keys.to_vec().iter().map(|k| Key::parse(k)).collect();
            match parsed {
                Ok(list) if !list.is_empty() => {
                    by_action.insert(action, list);
                }
                Ok(_) => warnings.push(format!("[keys]: `{name}` has no key")),
                Err(err) => warnings.push(format!("[keys]: `{name}`: {err}")),
            }
        }
        // Defaults first; a key that `[keys]` gives to another action is taken from its default
        // action (and from that action's help). Two `[keys]` entries with one key: the later wins.
        let overridden = |a: &Action| overrides.contains_key(a.name());
        let mut by_key = HashMap::new();
        for action in Action::ALL.iter().filter(|a| !overridden(a)) {
            for key in &by_action[action] {
                by_key.insert(*key, *action);
            }
        }
        let mut claimed: HashMap<Key, Action> = HashMap::new();
        for action in Action::ALL.iter().filter(|a| overridden(a)) {
            for key in by_action[action].clone() {
                if let Some(previous) = claimed.insert(key, *action) {
                    warnings.push(format!(
                        "[keys]: {} is bound to both {} and {}",
                        key.label(),
                        previous.name(),
                        action.name()
                    ));
                }
                if let Some(default_owner) = by_key.insert(key, *action).filter(|o| o != action && !overridden(o)) {
                    by_action.entry(default_owner).or_default().retain(|k| *k != key);
                }
            }
        }
        (Keymap { by_key, by_action }, warnings)
    }

    pub fn action(&self, event: &KeyEvent) -> Option<Action> {
        self.by_key.get(&Key::from_event(event)).copied()
    }

    pub fn keys(&self, action: Action) -> &[Key] {
        self.by_action.get(&action).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The first key of an action, for hints.
    pub fn label(&self, action: Action) -> String {
        self.keys(action).first().map(Key::label).unwrap_or_default()
    }

    /// Every key of an action, for the help.
    pub fn labels(&self, action: Action) -> String {
        self.keys(action).iter().map(Key::label).collect::<Vec<_>>().join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent { code, modifiers, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    #[test]
    fn defaults_follow_the_spec() {
        let (keymap, warnings) = Keymap::new(&BTreeMap::new());
        assert!(warnings.is_empty(), "{warnings:?}");
        let check = |code, modifiers, action| assert_eq!(keymap.action(&event(code, modifiers)), Some(action));
        check(KeyCode::Char('b'), KeyModifiers::NONE, Action::Build);
        check(KeyCode::Char('G'), KeyModifiers::SHIFT, Action::Bottom);
        check(KeyCode::Char('g'), KeyModifiers::NONE, Action::Top);
        check(KeyCode::Char('u'), KeyModifiers::CONTROL, Action::PageUp);
        check(KeyCode::Char('c'), KeyModifiers::CONTROL, Action::Cancel);
        check(KeyCode::Char('c'), KeyModifiers::NONE, Action::Clean);
        check(KeyCode::BackTab, KeyModifiers::SHIFT, Action::PrevPane);
        check(KeyCode::Tab, KeyModifiers::NONE, Action::NextPane);
        check(KeyCode::Char(' '), KeyModifiers::NONE, Action::Select);
        check(KeyCode::Char('?'), KeyModifiers::SHIFT, Action::Help);
        check(KeyCode::Char('3'), KeyModifiers::NONE, Action::Pane3);
        check(KeyCode::Esc, KeyModifiers::NONE, Action::Close);
    }

    #[test]
    fn overrides_replace_an_actions_keys() {
        let mut overrides = BTreeMap::new();
        overrides.insert("build".to_string(), Keys::Many(vec!["B".into(), "f7".into()]));
        overrides.insert("nope".to_string(), Keys::One("z".into()));
        overrides.insert("quit".to_string(), Keys::One("ctrl+".into()));
        let (keymap, warnings) = Keymap::new(&overrides);
        assert_eq!(keymap.action(&event(KeyCode::Char('B'), KeyModifiers::SHIFT)), Some(Action::Build));
        assert_eq!(keymap.action(&event(KeyCode::F(7), KeyModifiers::NONE)), Some(Action::Build));
        assert_eq!(keymap.action(&event(KeyCode::Char('b'), KeyModifiers::NONE)), None);
        assert_eq!(keymap.labels(Action::Build), "B F7");
        assert_eq!(
            keymap.action(&event(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(Action::Quit),
            "a bad entry keeps the defaults"
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    #[test]
    fn an_override_wins_a_default_key() {
        let mut overrides = BTreeMap::new();
        overrides.insert("sync".to_string(), Keys::One("b".into()));
        let (keymap, _) = Keymap::new(&overrides);
        assert_eq!(keymap.action(&event(KeyCode::Char('b'), KeyModifiers::NONE)), Some(Action::Sync));
        assert_eq!(keymap.labels(Action::Build), "", "the help no longer shows b for build");
    }

    #[test]
    fn labels() {
        assert_eq!(Key::parse("shift+tab").unwrap().label(), "Shift+Tab");
        assert_eq!(Key::parse("ctrl+U").unwrap().label(), "Ctrl+u");
        assert_eq!(Key::parse("shift+g").unwrap().label(), "G");
        assert_eq!(Key::parse("space").unwrap().label(), "Space");
        assert!(Key::parse("hyper+x").is_err());
        assert!(Key::parse("xyz").is_err());
    }
}
