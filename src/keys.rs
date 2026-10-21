//! Key resolution and multi-key binding matching, ported from dooit's KeyManager

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    SwitchFocus,
    MoveDown,
    MoveUp,
    MoveDownTopLevel,
    MoveUpTopLevel,
    EditDescription,
    EditDue,
    EditRecurrence,
    EditEffort,
    AddSibling,
    ToggleExpand,
    ToggleExpandParent,
    GoToTop,
    GoToBottom,
    AddChild,
    ShiftDown,
    ShiftUp,
    Indent,
    Dedent,
    RemoveNode,
    CopyDescription,
    CopyModel,
    PasteBelow,
    PasteAbove,
    ToggleComplete,
    ToggleShowCompleted,
    RemoveCompleted,
    IncreaseUrgency,
    DecreaseUrgency,
    StartSearch,
    StartSort,
    Quit,
    ShowHelp,
}

impl Action {
    /// The descriptions shown on the help screen
    pub fn description(&self) -> &'static str {
        match self {
            Action::SwitchFocus => "Switch focus between the workspace and the todo list",
            Action::MoveDown => "Move the cursor down in the focused list",
            Action::MoveUp => "Move the cursor up in the focused list",
            Action::MoveDownTopLevel => "Move the cursor to the next top-level item",
            Action::MoveUpTopLevel => "Move the cursor to the previous top-level item",
            Action::EditDescription => "Start editing the description of the focused item",
            Action::EditDue => "Start editing the due date of the todo",
            Action::EditRecurrence => "Start editing the recurrence of the todo",
            Action::EditEffort => "",
            Action::AddSibling => "Add a sibling to highlighted item",
            Action::ToggleExpand => "Toggle the expansion of the highlighted item",
            Action::ToggleExpandParent => "Toggle the expansion of the parent of the highlighted item",
            Action::GoToTop => "Move the cursor to the top of the list",
            Action::GoToBottom => "Move the cursor to the bottom of the list",
            Action::AddChild => "Add a child to the highlighted item",
            Action::ShiftDown => "Shift the highlighted item down",
            Action::ShiftUp => "Shift the highlighted item up",
            Action::Indent => "Make the highlighted item a child of the item above it",
            Action::Dedent => "Move the highlighted item out of its parent",
            Action::RemoveNode => "Remove the highlighted item",
            Action::CopyDescription => "Copy the description of the focused item to the clipboard",
            Action::CopyModel => "Copy the current highlighted node to clipboard",
            Action::PasteBelow => "Paste the copied node in the list (puts below the highlighted)",
            Action::PasteAbove => "Paste the copied node in the list (puts above the highlighted)",
            Action::ToggleComplete => "Toggle the completion of the todo",
            Action::ToggleShowCompleted => "Show or hide the completed todos",
            Action::RemoveCompleted => "Remove the completed todos of the workspace",
            Action::IncreaseUrgency => "Increase the urgency of the todo",
            Action::DecreaseUrgency => "Decrease the urgency of the todo",
            Action::StartSearch => "Start a search within the list",
            Action::StartSort => "Start sorting the siblings of the highlighted item",
            Action::Quit => "Quit fooit",
            Action::ShowHelp => "Show the help screen",
        }
    }
}

pub enum KeyMatch {
    NoMatch,
    Multiple,
    Found(Action),
}

pub struct KeyManager {
    pub bindings: Vec<(String, Action)>,
    inputs: Vec<String>,
}

impl KeyManager {
    /// The bindings: dooit's defaults plus fooit's own
    pub fn with_defaults() -> KeyManager {
        let mut km = KeyManager {
            bindings: vec![],
            inputs: vec![],
        };
        let defaults: &[(&[&str], Action)] = &[
            (&["<tab>"], Action::SwitchFocus),
            (&["j", "<down>"], Action::MoveDown),
            (&["k", "<up>"], Action::MoveUp),
            (&["<shift+down>"], Action::MoveDownTopLevel),
            (&["<shift+up>"], Action::MoveUpTopLevel),
            (&["i"], Action::EditDescription),
            (&["d"], Action::EditDue),
            (&["r"], Action::EditRecurrence),
            (&["e"], Action::EditEffort),
            (&["a", "<enter>"], Action::AddSibling),
            (&["z"], Action::ToggleExpand),
            (&["Z"], Action::ToggleExpandParent),
            (&["gg"], Action::GoToTop),
            (&["G"], Action::GoToBottom),
            (&["A"], Action::AddChild),
            (&["J"], Action::ShiftDown),
            (&["K"], Action::ShiftUp),
            (&[">>", "<right>"], Action::Indent),
            (&["<<", "<left>"], Action::Dedent),
            (&["xx"], Action::RemoveNode),
            (&["y"], Action::CopyDescription),
            (&["Y"], Action::CopyModel),
            (&["p"], Action::PasteBelow),
            (&["P"], Action::PasteAbove),
            (&["c"], Action::ToggleComplete),
            (&["H"], Action::ToggleShowCompleted),
            (&["X"], Action::RemoveCompleted),
            (&["=", "+"], Action::IncreaseUrgency),
            (&["-", "_"], Action::DecreaseUrgency),
            (&["/"], Action::StartSearch),
            (&["<ctrl+s>"], Action::StartSort),
            (&["q", "<ctrl+q>"], Action::Quit),
            (&["?"], Action::ShowHelp),
        ];
        for (keys, action) in defaults {
            for key in *keys {
                km.bindings.push((key.to_string(), *action));
            }
        }
        km
    }

    fn input(&self) -> String {
        let mut formatted = String::new();
        for i in &self.inputs {
            if i.chars().count() > 1 {
                formatted.push('<');
                formatted.push_str(i);
                formatted.push('>');
            } else {
                formatted.push_str(i);
            }
        }
        formatted
    }

    pub fn clear(&mut self) {
        self.inputs.clear();
    }

    /// Bindings only exist for NORMAL mode, so any other mode never matches
    pub fn register(&mut self, key: &str, normal_mode: bool) -> KeyMatch {
        if key == "escape" {
            self.clear();
            return KeyMatch::NoMatch;
        }

        self.inputs.push(key.to_string());
        let input = self.input();

        let matched: Vec<&(String, Action)> = if normal_mode {
            self.bindings.iter().filter(|(k, _)| k.starts_with(&input)).collect()
        } else {
            vec![]
        };

        if matched.is_empty() {
            self.clear();
            return KeyMatch::NoMatch;
        }

        let exact = matched.iter().find(|(k, _)| *k == input);
        if matched.len() > 1 || exact.is_none() {
            return KeyMatch::Multiple;
        }

        let action = matched[0].1;
        self.clear();
        KeyMatch::Found(action)
    }
}

/// Resolve a terminal key event to the key names dooit (textual) uses
pub fn resolve(event: KeyEvent) -> Option<String> {
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);

    let key = match event.code {
        KeyCode::Char(c) if ctrl => format!("ctrl+{}", c.to_ascii_lowercase()),
        KeyCode::Char(c) if alt => match c {
            'b' => "ctrl+left".into(),
            'f' => "ctrl+right".into(),
            'd' => "ctrl+delete".into(),
            _ => c.to_string(),
        },
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "escape".into(),
        KeyCode::Backspace if ctrl || alt => "ctrl+w".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "shift+tab".into(),
        KeyCode::Left if ctrl || alt => "ctrl+left".into(),
        KeyCode::Right if ctrl || alt => "ctrl+right".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up if shift => "shift+up".into(),
        KeyCode::Down if shift => "shift+down".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::Delete if ctrl || alt => "ctrl+delete".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::F(n) => format!("f{}", n),
        _ => return None,
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences() {
        let mut km = KeyManager::with_defaults();
        assert!(matches!(km.register("g", true), KeyMatch::Multiple));
        assert!(matches!(km.register("g", true), KeyMatch::Found(Action::GoToTop)));
        assert!(matches!(km.register("<", true), KeyMatch::Multiple));
        assert!(matches!(km.register("<", true), KeyMatch::Found(Action::Dedent)));
        assert!(matches!(km.register("left", true), KeyMatch::Found(Action::Dedent)));
        assert!(matches!(km.register("right", true), KeyMatch::Found(Action::Indent)));
        assert!(matches!(km.register("shift+up", true), KeyMatch::Found(Action::MoveUpTopLevel)));
        assert!(matches!(km.register("tab", true), KeyMatch::Found(Action::SwitchFocus)));
        assert!(matches!(km.register("x", true), KeyMatch::Multiple));
        assert!(matches!(km.register("q", true), KeyMatch::NoMatch));
        assert!(matches!(km.register("j", false), KeyMatch::NoMatch));
        assert!(matches!(km.register("ctrl+s", true), KeyMatch::Found(Action::StartSort)));
        assert!(matches!(km.register("q", true), KeyMatch::Found(Action::Quit)));
        assert!(matches!(km.register("enter", true), KeyMatch::Found(Action::AddSibling)));
        assert!(matches!(km.register("enter", false), KeyMatch::NoMatch));
    }
}
