//! Key resolution and multi-key binding matching, ported from dooit's KeyManager

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    SwitchFocus,
    MoveDown,
    MoveUp,
    NextSibling,
    PreviousSibling,
    IntoChildren,
    ToParent,
    EditDescription,
    EditBackspace,
    EditDue,
    EditRecurrence,
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
    Snooze,
    StartMove,
    Undo,
    Redo,
    ToggleShowCompleted,
    RemoveCompleted,
    IncreaseUrgency,
    DecreaseUrgency,
    StartSearch,
    FindEverywhere,
    StartSort,
    Quit,
    ShowHelp,
}

impl Action {
    pub const ALL: &[Action] = &[
        Action::SwitchFocus,
        Action::MoveDown,
        Action::MoveUp,
        Action::NextSibling,
        Action::PreviousSibling,
        Action::IntoChildren,
        Action::ToParent,
        Action::EditDescription,
        Action::EditBackspace,
        Action::EditDue,
        Action::EditRecurrence,
        Action::AddSibling,
        Action::ToggleExpand,
        Action::ToggleExpandParent,
        Action::GoToTop,
        Action::GoToBottom,
        Action::AddChild,
        Action::ShiftDown,
        Action::ShiftUp,
        Action::Indent,
        Action::Dedent,
        Action::RemoveNode,
        Action::CopyDescription,
        Action::CopyModel,
        Action::PasteBelow,
        Action::PasteAbove,
        Action::ToggleComplete,
        Action::Snooze,
        Action::StartMove,
        Action::Undo,
        Action::Redo,
        Action::ToggleShowCompleted,
        Action::RemoveCompleted,
        Action::IncreaseUrgency,
        Action::DecreaseUrgency,
        Action::StartSearch,
        Action::FindEverywhere,
        Action::StartSort,
        Action::Quit,
        Action::ShowHelp,
    ];

    /// The name used for the action in the keys file: `remove_node` for `RemoveNode`
    pub fn name(&self) -> String {
        let mut name = String::new();
        for (i, c) in format!("{:?}", self).chars().enumerate() {
            if c.is_uppercase() && i > 0 {
                name.push('_');
            }
            name.push(c.to_ascii_lowercase());
        }
        name
    }

    /// The help screen's sections, in order
    pub const SECTIONS: &[&str] = &["Moving around", "Adding and editing", "Arranging", "Removing and undoing", "Views and more"];

    /// The help screen section the action is listed under
    pub fn section(&self) -> &'static str {
        use Action::*;
        Self::SECTIONS[match self {
            SwitchFocus | MoveDown | MoveUp | NextSibling | PreviousSibling | IntoChildren | ToParent | GoToTop | GoToBottom
            | ToggleExpand | ToggleExpandParent => 0,
            AddSibling | AddChild | EditDescription | EditBackspace | EditDue | EditRecurrence | ToggleComplete | Snooze | IncreaseUrgency
            | DecreaseUrgency => 1,
            Indent | Dedent | ShiftDown | ShiftUp | StartMove | StartSort | CopyDescription | CopyModel | PasteBelow
            | PasteAbove => 2,
            RemoveNode | RemoveCompleted | Undo | Redo => 3,
            ToggleShowCompleted | StartSearch | FindEverywhere | ShowHelp | Quit => 4,
        }]
    }

    /// The descriptions shown on the help screen
    pub fn description(&self) -> &'static str {
        match self {
            Action::SwitchFocus => "Switch focus between the workspace and the todo list",
            Action::MoveDown => "Move the cursor down in the focused list",
            Action::MoveUp => "Move the cursor up in the focused list",
            Action::NextSibling => "Move the cursor to the next item on the same level",
            Action::PreviousSibling => "Move the cursor to the previous item on the same level",
            Action::IntoChildren => "Move the cursor into the subtasks of the highlighted item",
            Action::ToParent => "Move the cursor to the parent of the highlighted item",
            Action::EditDescription => "Start editing the description of the focused item",
            Action::EditBackspace => "Start editing the description, deleting its last character",
            Action::EditDue => "Start editing the due date of the todo",
            Action::EditRecurrence => "Start editing the recurrence of the todo",
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
            Action::Snooze => "Push the due date a day later (3s: three days), from today when overdue",
            Action::Undo => "Undo the last change",
            Action::Redo => "Redo the last undone change",
            Action::StartMove => "Move the highlighted item with the arrows and tab, enter to finish",
            Action::ToggleShowCompleted => "Show or hide the completed todos",
            Action::RemoveCompleted => "Remove the completed todos of the workspace",
            Action::IncreaseUrgency => "Increase the urgency of the todo",
            Action::DecreaseUrgency => "Decrease the urgency of the todo",
            Action::StartSearch => "Start a search within the list",
            Action::FindEverywhere => "Search the todos of every workspace",
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
            (&["j"], Action::MoveDown),
            (&["k"], Action::MoveUp),
            (&["<down>"], Action::NextSibling),
            (&["<up>"], Action::PreviousSibling),
            (&["<right>", "l"], Action::IntoChildren),
            (&["<left>", "h"], Action::ToParent),
            (&["i"], Action::EditDescription),
            (&["<backspace>"], Action::EditBackspace),
            (&["d"], Action::EditDue),
            (&["r"], Action::EditRecurrence),
            (&["a", "<enter>"], Action::AddSibling),
            (&["z"], Action::ToggleExpand),
            (&["Z"], Action::ToggleExpandParent),
            (&["gg"], Action::GoToTop),
            (&["G"], Action::GoToBottom),
            (&["A"], Action::AddChild),
            (&["J"], Action::ShiftDown),
            (&["K"], Action::ShiftUp),
            (&[">>", "."], Action::Indent),
            (&["<<", ","], Action::Dedent),
            (&["xx"], Action::RemoveNode),
            (&["y"], Action::CopyDescription),
            (&["Y"], Action::CopyModel),
            (&["p"], Action::PasteBelow),
            (&["P"], Action::PasteAbove),
            (&["c"], Action::ToggleComplete),
            (&["s"], Action::Snooze),
            (&["m"], Action::StartMove),
            (&["u"], Action::Undo),
            (&["<ctrl+r>"], Action::Redo),
            (&["H"], Action::ToggleShowCompleted),
            (&["X"], Action::RemoveCompleted),
            (&["=", "+"], Action::IncreaseUrgency),
            (&["-", "_"], Action::DecreaseUrgency),
            (&["/"], Action::StartSearch),
            (&["<ctrl+f>"], Action::FindEverywhere),
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

    /// The default bindings with the ones in a keys file in their place. Each line binds an
    /// action to one key or a list of them, replacing its defaults, like
    /// `remove_node = "d"` or `next_sibling = ["<down>", "J"]`; `#` starts a comment
    pub fn from_file(text: &str) -> Result<KeyManager, String> {
        let mut km = KeyManager::with_defaults();
        for (number, line) in text.lines().enumerate() {
            let line = strip_comment(line).trim();
            if line.is_empty() {
                continue;
            }
            let at = |message: String| format!("line {}: {}", number + 1, message);
            let (name, value) = line.split_once('=').ok_or_else(|| at("expected `action = \"key\"`".into()))?;
            let name = name.trim();
            let action = Action::ALL
                .iter()
                .find(|a| a.name() == name)
                .ok_or_else(|| at(format!("unknown action `{}`", name)))?;
            let keys = parse_keys(value.trim()).ok_or_else(|| at("expected a \"key\" or a list of them".into()))?;
            km.bindings.retain(|(_, a)| a != action);
            km.bindings.extend(keys.into_iter().map(|k| (k, *action)));
        }

        // a key that is also another action's, or the start of another key, would never be
        // told apart from it
        for (key, action) in &km.bindings {
            if let Some((_, other)) = km.bindings.iter().find(|(k, a)| k == key && a != action) {
                return Err(format!("`{}` is bound to both {} and {}", key, action.name(), other.name()));
            }
            if let Some((other, other_action)) = km.bindings.iter().find(|(k, _)| k != key && k.starts_with(key.as_str())) {
                return Err(format!(
                    "`{}` ({}) is the start of `{}` ({}), so it could never be pressed on its own",
                    key,
                    action.name(),
                    other,
                    other_action.name()
                ));
            }
        }
        Ok(km)
    }

    /// The bindings written out as a keys file
    pub fn to_file(&self) -> String {
        let mut out = String::from("# fooit keys: action = \"key\" or [\"key\", ...]; keys like \"a\", \"gg\", \"<ctrl+s>\", \"<down>\"\n");
        for action in Action::ALL {
            let keys: Vec<String> =
                self.bindings.iter().filter(|(_, a)| a == action).map(|(k, _)| format!("{:?}", k)).collect();
            if keys.is_empty() {
                continue;
            }
            if !action.description().is_empty() {
                out += &format!("\n# {}\n", action.description());
            }
            out += &format!("{} = [{}]\n", action.name(), keys.join(", "));
        }
        out
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

    /// Whether part of a binding of several keys has been typed
    pub fn pending(&self) -> bool {
        !self.inputs.is_empty()
    }

    pub fn is_bound(&self, key: &str) -> bool {
        self.bindings.iter().any(|(k, _)| k == key)
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

fn strip_comment(line: &str) -> &str {
    // a # inside quotes is a key, not a comment
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

/// `"a"` or `["a", "<ctrl+s>"]`, with `\"` and `\\` inside the quotes
fn parse_keys(value: &str) -> Option<Vec<String>> {
    let (list, mut rest) = match value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        Some(inner) => (true, inner.trim()),
        None => (false, value.trim()),
    };
    let mut keys = vec![];
    while !rest.is_empty() {
        let quoted = rest.strip_prefix('"')?;
        let mut key = String::new();
        let mut chars = quoted.char_indices();
        let end = loop {
            match chars.next()? {
                (_, '\\') => key.push(chars.next()?.1),
                (i, '"') => break i,
                (_, c) => key.push(c),
            }
        };
        if key.is_empty() {
            return None;
        }
        keys.push(key);
        rest = quoted[end + 1..].trim_start();
        if !list && !rest.is_empty() {
            return None;
        }
        if let Some(after) = rest.strip_prefix(',') {
            rest = after.trim_start();
        } else if !rest.is_empty() {
            return None;
        }
    }
    (!keys.is_empty()).then_some(keys)
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
        assert!(matches!(km.register(".", true), KeyMatch::Found(Action::Indent)));
        assert!(matches!(km.register(",", true), KeyMatch::Found(Action::Dedent)));
        assert!(matches!(km.register("left", true), KeyMatch::Found(Action::ToParent)));
        assert!(matches!(km.register("right", true), KeyMatch::Found(Action::IntoChildren)));
        assert!(matches!(km.register("down", true), KeyMatch::Found(Action::NextSibling)));
        assert!(matches!(km.register("j", true), KeyMatch::Found(Action::MoveDown)));
        assert!(matches!(km.register("tab", true), KeyMatch::Found(Action::SwitchFocus)));
        assert!(matches!(km.register("x", true), KeyMatch::Multiple));
        assert!(matches!(km.register("q", true), KeyMatch::NoMatch));
        assert!(matches!(km.register("j", false), KeyMatch::NoMatch));
        assert!(matches!(km.register("ctrl+s", true), KeyMatch::Found(Action::StartSort)));
        assert!(matches!(km.register("q", true), KeyMatch::Found(Action::Quit)));
    }

    #[test]
    fn keys_file() {
        let mut km = KeyManager::from_file(
            "# mine\nremove_node = \"d\"  # one key\nnext_sibling = [\"<down>\", \"J\"]\nedit_due = \"D\"\nshift_down = \"<ctrl+j>\"\n",
        )
        .unwrap();
        assert!(matches!(km.register("d", true), KeyMatch::Found(Action::RemoveNode)));
        assert!(matches!(km.register("J", true), KeyMatch::Found(Action::NextSibling)));
        assert!(matches!(km.register("x", true), KeyMatch::NoMatch));

        assert!(KeyManager::from_file("nope = \"z\"").err().unwrap().contains("unknown action `nope`"));
        assert!(KeyManager::from_file("quit = \"g\"").err().unwrap().contains("is the start of `gg`"));
        assert_eq!(parse_keys(r#"["<<", ",", "\""]"#).unwrap(), ["<<", ",", "\""]);
        assert!(KeyManager::from_file("quit = z").is_err());
        assert!(KeyManager::from_file("remove_node = \"d\"").err().unwrap().contains("bound to both"));

        // what to_file writes reads back the same
        let defaults = KeyManager::with_defaults();
        assert_eq!(KeyManager::from_file(&defaults.to_file()).unwrap().bindings.len(), defaults.bindings.len());
        assert!(matches!(km.register("enter", true), KeyMatch::Found(Action::AddSibling)));
        assert!(matches!(km.register("enter", false), KeyMatch::NoMatch));
    }
}
