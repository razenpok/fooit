//! Application state and behaviour, mirroring dooit's trees, bars and API

use crate::dates;
use crate::db::{Db, Item, Parent, SortField};
use crate::input::Input;
use crate::keys::{self, Action, KeyManager, KeyMatch};
use ratatui::crossterm::event::KeyEvent;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, String>;

const NO_NODE: &str = "No item selected";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Search,
    Sort,
    Confirm,
}

impl Mode {
    pub fn name(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Search => "SEARCH",
            Mode::Sort => "SORT",
            Mode::Confirm => "CONFIRM",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Level {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TreeRef {
    Workspaces,
    Todos(i64),
}

impl TreeRef {
    pub fn is_todos(&self) -> bool {
        matches!(self, TreeRef::Todos(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Status,
    Description,
    Due,
    Urgency,
    Effort,
    Recurrence,
}

impl Field {
    pub fn name(&self) -> &'static str {
        match self {
            Field::Status => "status",
            Field::Description => "description",
            Field::Due => "due",
            Field::Urgency => "urgency",
            Field::Effort => "effort",
            Field::Recurrence => "recurrence",
        }
    }
}

pub const WORKSPACE_LAYOUT: &[Field] = &[Field::Description];
pub const TODO_LAYOUT: &[Field] = &[Field::Status, Field::Description, Field::Due, Field::Urgency];

pub fn layout(tree: TreeRef) -> &'static [Field] {
    if tree.is_todos() { TODO_LAYOUT } else { WORKSPACE_LAYOUT }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub item: Item,
    pub depth: usize,
    pub enabled: bool,
}

pub struct Edit {
    pub item: Item,
    pub field: Field,
    pub input: Input,
}

#[derive(Default)]
pub struct TreeState {
    pub rows: Vec<Row>,
    pub highlighted: Option<usize>,
    pub filter: String,
    pub offset: usize,
    pub edit: Option<Edit>,
    pub unsaved: Option<Item>,
    /// Per-row column widths (by layout position, ignoring any edit) and the db generation they were measured at
    pub widths: Option<(u64, Vec<Vec<usize>>)>,
}

impl TreeState {
    pub fn current(&self) -> Option<Item> {
        self.highlighted.and_then(|i| self.rows.get(i)).map(|r| r.item)
    }

    pub fn editing(&self) -> Option<&Edit> {
        let edit = self.edit.as_ref()?;
        (Some(edit.item) == self.current()).then_some(edit)
    }

    fn index_of(&self, item: Item) -> Option<usize> {
        self.rows.iter().position(|r| r.item == item)
    }

    fn clamp(&mut self, index: Option<usize>) {
        self.highlighted = match index {
            _ if self.rows.is_empty() => None,
            Some(i) => Some(i.min(self.rows.len() - 1)),
            None => None,
        };
    }

    fn first_enabled(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.enabled)
    }

    fn last_enabled(&self) -> Option<usize> {
        self.rows.iter().rposition(|r| r.enabled)
    }

    /// textual's `find_next_enabled`, which wraps around the ends
    fn next_enabled(&self, down: bool) -> Option<usize> {
        let Some(anchor) = self.highlighted else {
            return if down { self.first_enabled() } else { self.last_enabled() };
        };
        let len = self.rows.len();
        for step in 1..=len {
            let index = if down {
                (anchor + step) % len
            } else {
                (anchor + len * 2 - step) % len
            };
            if self.rows[index].enabled {
                return Some(index);
            }
        }
        Some(anchor)
    }
}

pub enum ConfirmAction {
    Remove { tree: TreeRef, item: Item },
    Discard { tree: TreeRef },
}

pub struct Confirm {
    pub action: ConfirmAction,
    pub message: String,
    pub confirmed: Option<(String, Level)>,
    pub cancelled: Option<(String, Level)>,
    pub cancel_mode: Mode,
}

pub enum Bar {
    Status,
    Notification { message: String, level: Level, until: Instant },
    Search { input: Input, tree: TreeRef },
    Sort { options: Vec<SortField>, selected: usize, tree: TreeRef, item: Item },
    Confirm(Confirm),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Workspaces,
    Todos,
}

pub struct App {
    pub db: Db,
    pub ws_tree: TreeState,
    pub todo_trees: HashMap<i64, TreeState>,
    pub focus: Focus,
    pub mode: Mode,
    pub expanded: HashSet<Item>,
    pub keys: KeyManager,
    pub bar: Bar,
    pub show_confirm: bool,
    pub help: Option<u16>,
    pub quit: bool,
    pub username: String,
    ws_clipboard: Option<Item>,
    todo_clipboard: Option<Item>,
    last_poll: Instant,
}

impl App {
    pub fn new(db: Db) -> App {
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_else(|_| "user".into());

        let mut app = App {
            db,
            ws_tree: TreeState::default(),
            todo_trees: HashMap::new(),
            focus: Focus::Workspaces,
            mode: Mode::Normal,
            expanded: HashSet::new(),
            keys: KeyManager::with_defaults(),
            bar: Bar::Status,
            show_confirm: true,
            help: None,
            quit: false,
            username,
            ws_clipboard: None,
            todo_clipboard: None,
            last_poll: Instant::now(),
        };
        app.refresh_all();
        app
    }

    // ------------------------------------------------------------------
    // Tree bookkeeping

    pub fn current_ws(&self) -> Option<i64> {
        match self.ws_tree.current() {
            Some(Item::Ws(id)) => Some(id),
            _ => None,
        }
    }

    pub fn focused_tree(&self) -> TreeRef {
        match (self.focus, self.current_ws()) {
            (Focus::Todos, Some(ws)) => TreeRef::Todos(ws),
            _ => TreeRef::Workspaces,
        }
    }

    pub fn tree(&self, tree: TreeRef) -> Option<&TreeState> {
        match tree {
            TreeRef::Workspaces => Some(&self.ws_tree),
            TreeRef::Todos(ws) => self.todo_trees.get(&ws),
        }
    }

    pub fn tree_mut(&mut self, tree: TreeRef) -> &mut TreeState {
        match tree {
            TreeRef::Workspaces => &mut self.ws_tree,
            TreeRef::Todos(ws) => {
                if !self.todo_trees.contains_key(&ws) {
                    self.todo_trees.insert(ws, TreeState::default());
                    self.rebuild(tree);
                }
                self.todo_trees.get_mut(&ws).unwrap()
            }
        }
    }

    fn tree_root(&self, tree: TreeRef) -> Option<Parent> {
        match tree {
            TreeRef::Workspaces => self.db.root_id().map(Parent::Ws),
            TreeRef::Todos(ws) => Some(Parent::Ws(ws)),
        }
    }

    /// Rebuild the visible rows, keeping the highlight on the same item when possible
    fn rebuild(&mut self, tree: TreeRef) {
        let todos = tree.is_todos();
        let root = self.tree_root(tree);
        let Some(state) = (match tree {
            TreeRef::Workspaces => Some(&self.ws_tree),
            TreeRef::Todos(ws) => self.todo_trees.get(&ws),
        }) else {
            return;
        };

        let filter = state.filter.clone();
        let highlighted_item = state.current();
        let highlighted_index = state.highlighted;

        let mut rows = vec![];
        fn walk(app: &App, parent: Parent, todos: bool, depth: usize, filter: &str, rows: &mut Vec<Row>) {
            for child in app.db.children(parent, todos) {
                let enabled = app.db.description(child).contains(filter);
                rows.push(Row { item: child, depth, enabled });
                if app.expanded.contains(&child) || !filter.is_empty() {
                    walk(app, child.as_parent(), todos, depth + 1, filter, rows);
                }
            }
        }
        if let Some(root) = root {
            walk(self, root, todos, 0, &filter, &mut rows);
        }

        let state = match tree {
            TreeRef::Workspaces => &mut self.ws_tree,
            TreeRef::Todos(ws) => self.todo_trees.get_mut(&ws).unwrap(),
        };
        state.rows = rows;
        state.widths = None;
        match highlighted_item.and_then(|item| state.index_of(item)) {
            Some(index) => state.highlighted = Some(index),
            None => state.clamp(highlighted_index),
        }

        // drop edits whose item disappeared
        if let Some(edit) = &state.edit {
            if state.index_of(edit.item).is_none() {
                state.edit = None;
                state.unsaved = None;
            }
        }
    }

    pub fn refresh_all(&mut self) {
        self.rebuild(TreeRef::Workspaces);
        let existing: Vec<i64> = self.todo_trees.keys().copied().collect();
        for ws in existing {
            if self.db.exists(Item::Ws(ws)) {
                self.rebuild(TreeRef::Todos(ws));
            } else {
                self.todo_trees.remove(&ws);
            }
        }
        if let Some(ws) = self.current_ws() {
            self.tree_mut(TreeRef::Todos(ws));
        }

        if self.current_ws().is_none() {
            self.focus = Focus::Workspaces;
        }
        if self.mode == Mode::Insert && self.tree(self.focused_tree()).and_then(|t| t.editing()).is_none() {
            self.mode = Mode::Normal;
        }
    }

    fn current_item(&self, tree: TreeRef) -> Result<Item> {
        self.tree(tree).and_then(|t| t.current()).ok_or_else(|| NO_NODE.to_string())
    }

    fn highlight_item(&mut self, tree: TreeRef, item: Item) {
        let state = self.tree_mut(tree);
        if let Some(index) = state.index_of(item) {
            state.highlighted = Some(index);
        }
        if tree == TreeRef::Workspaces {
            self.refresh_all();
        }
    }

    fn set_highlight(&mut self, tree: TreeRef, index: Option<usize>) {
        self.tree_mut(tree).highlighted = index;
        if tree == TreeRef::Workspaces {
            self.refresh_all();
        }
    }

    // ------------------------------------------------------------------
    // Bar

    pub fn notify(&mut self, message: impl Into<String>, level: Level) {
        self.bar = Bar::Notification {
            message: message.into(),
            level,
            until: Instant::now() + Duration::from_secs(1),
        };
    }

    fn bar_focused(&self) -> bool {
        !matches!(self.bar, Bar::Status | Bar::Notification { .. })
    }

    fn confirm(&mut self, confirm: Confirm) {
        self.bar = Bar::Confirm(confirm);
        self.mode = Mode::Confirm;
    }

    // ------------------------------------------------------------------
    // Event handling

    /// Expires notifications and picks up outside changes; returns whether anything visible changed
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        if let Bar::Notification { until, .. } = &self.bar {
            if Instant::now() >= *until {
                self.bar = Bar::Status;
                changed = true;
            }
        }

        if self.last_poll.elapsed() >= Duration::from_secs(1) {
            self.last_poll = Instant::now();
            if self.db.has_changed() {
                self.refresh_all();
                changed = true;
            }
        }
        changed
    }

    pub fn on_paste(&mut self, text: &str) {
        let text = text.replace(['\r', '\n'], " ");
        if let Bar::Search { input, tree } = &mut self.bar {
            input.insert_text(&text);
            let filter: String = input.value().chars().skip(1).collect();
            let tree = *tree;
            self.set_filter(tree, filter);
            return;
        }
        let tree = self.focused_tree();
        if let Some(edit) = self.tree_mut(tree).edit.as_mut() {
            edit.input.insert_text(&text);
        }
    }

    pub fn on_key(&mut self, event: KeyEvent) {
        let Some(key) = keys::resolve(event) else { return };

        if key == "ctrl+c" {
            self.quit = true;
            return;
        }

        if let Some(scroll) = self.help {
            match key.as_str() {
                "escape" => self.help = None,
                "j" | "down" => self.help = Some(scroll.saturating_add(1)),
                "k" | "up" => self.help = Some(scroll.saturating_sub(1)),
                "pagedown" => self.help = Some(scroll.saturating_add(10)),
                "pageup" => self.help = Some(scroll.saturating_sub(10)),
                _ => {}
            }
            return;
        }

        if self.bar_focused() {
            self.bar_keypress(&key);
            return;
        }

        if self.mode == Mode::Insert && self.tree(self.focused_tree()).and_then(|t| t.editing()).is_none() {
            self.mode = Mode::Normal;
        }

        match self.keys.register(&key, self.mode == Mode::Normal) {
            KeyMatch::NoMatch => self.tree_keypress(&key),
            KeyMatch::Multiple => {}
            KeyMatch::Found(action) => {
                if let Err(e) = self.run(action) {
                    self.notify(e, Level::Error);
                }
            }
        }
    }

    fn bar_keypress(&mut self, key: &str) {
        let bar = std::mem::replace(&mut self.bar, Bar::Status);
        match bar {
            Bar::Search { mut input, tree } => match key {
                "enter" => self.mode = Mode::Normal,
                "escape" => {
                    self.set_filter(tree, String::new());
                    self.mode = Mode::Normal;
                }
                _ => {
                    input.keypress(key);
                    let filter: String = input.value().chars().skip(1).collect();
                    self.set_filter(tree, filter);
                    self.bar = Bar::Search { input, tree };
                }
            },
            Bar::Sort { options, mut selected, tree, item } => match key {
                "escape" => self.mode = Mode::Normal,
                "enter" => {
                    self.mode = Mode::Normal;
                    if let Err(e) = self.db.sort_siblings(item, options[selected]) {
                        self.notify(e, Level::Error);
                    }
                    self.rebuild(tree);
                    self.refresh_all();
                }
                _ => {
                    match key {
                        "left" | "h" | "shift+tab" => selected = selected.saturating_sub(1),
                        "right" | "l" | "tab" => selected = (selected + 1).min(options.len() - 1),
                        _ => {}
                    }
                    self.bar = Bar::Sort { options, selected, tree, item };
                }
            },
            Bar::Confirm(confirm) => {
                let cancel = key.to_lowercase() != "y";
                if !cancel {
                    match confirm.action {
                        ConfirmAction::Remove { tree, item } => self.remove(tree, item),
                        ConfirmAction::Discard { tree } => {
                            self.stop_edit(tree, true);
                        }
                    }
                }
                self.mode = if cancel { confirm.cancel_mode } else { Mode::Normal };

                let notice = if cancel { confirm.cancelled } else { confirm.confirmed };
                if let Some((message, level)) = notice {
                    self.notify(message, level);
                }
            }
            other => self.bar = other,
        }
    }

    fn tree_keypress(&mut self, key: &str) {
        let tree = self.focused_tree();
        let editing = self.tree(tree).and_then(|t| t.editing()).map(|e| e.field);

        let Some(field) = editing else {
            if key == "escape" {
                self.set_filter(tree, String::new());
            }
            return;
        };

        let result = match (key, field) {
            ("enter", Field::Description) => {
                if self.stop_edit(tree, false)
                    && self.current_item(tree).map(|i| !self.db.description(i).is_empty()).unwrap_or(false)
                {
                    self.add_sibling(tree)
                } else {
                    Ok(())
                }
            }
            ("escape", Field::Description) => {
                self.discard_edit(tree);
                Ok(())
            }
            ("escape" | "enter", _) => {
                self.stop_edit(tree, false);
                Ok(())
            }
            _ => {
                if let Some(edit) = self.tree_mut(tree).edit.as_mut() {
                    edit.input.keypress(key);
                }
                Ok(())
            }
        };

        if let Err(e) = result {
            self.notify(e, Level::Error);
        }
    }

    // ------------------------------------------------------------------
    // Editing

    pub fn edit_default(&self, item: Item, field: Field) -> String {
        match (item, field) {
            (_, Field::Description) => self.db.description(item).to_string(),
            (Item::Todo(id), _) => {
                let Some(todo) = self.db.todo(id) else { return String::new() };
                match field {
                    Field::Due => match todo.due {
                        None => String::new(),
                        Some(due) => {
                            use chrono::Timelike;
                            if due.hour() != 0 || due.minute() != 0 {
                                due.format("%Y-%m-%d %H:%M").to_string()
                            } else {
                                due.format("%Y-%m-%d").to_string()
                            }
                        }
                    },
                    Field::Urgency => match todo.urgency {
                        0 => String::new(),
                        u => u.to_string(),
                    },
                    Field::Effort => todo.effort.to_string(),
                    Field::Recurrence => todo.recurrence.map(dates::recurrence_to_string).unwrap_or_default(),
                    Field::Status => match todo.status() {
                        "completed" => "x".into(),
                        "overdue" => "!".into(),
                        _ => "o".into(),
                    },
                    Field::Description => unreachable!(),
                }
            }
            (Item::Ws(_), _) => String::new(),
        }
    }

    fn start_edit(&mut self, tree: TreeRef, field: Field) -> Result<()> {
        if !layout(tree).contains(&field) {
            self.notify(format!("No such column: {}", field.name()), Level::Error);
            return Ok(());
        }

        let item = self.current_item(tree)?;
        let input = Input::new(&self.edit_default(item, field));
        self.tree_mut(tree).edit = Some(Edit { item, field, input });
        self.mode = Mode::Insert;
        Ok(())
    }

    fn save_edit(&mut self, edit: &Edit) -> Result<()> {
        let value = edit.input.value().trim().to_string();

        if edit.field == Field::Description {
            return self.db.set_description(edit.item, &value);
        }

        let Item::Todo(id) = edit.item else { return Ok(()) };
        let Some(mut todo) = self.db.todo(id).cloned() else { return Ok(()) };

        let parse_int = |value: &str| -> Result<i64> {
            if value.is_empty() || value == "0" {
                return Ok(0);
            }
            value
                .parse()
                .map_err(|_| format!("invalid literal for int() with base 10: '{}'", value))
        };

        match edit.field {
            Field::Due => {
                todo.due = if value.is_empty() {
                    None
                } else {
                    dates::parse_due(&value).or(todo.due)
                };
            }
            Field::Recurrence => {
                todo.recurrence = if value.is_empty() {
                    None
                } else {
                    Some(dates::parse_recurrence(&value)?)
                };
                if todo.recurrence.is_some() {
                    todo.pending = true;
                }
            }
            Field::Effort => todo.effort = parse_int(&value)?,
            Field::Urgency => todo.urgency = parse_int(&value)?,
            Field::Status => todo.pending = value != "COMPLETED",
            Field::Description => unreachable!(),
        }

        self.db.update_todo(todo)
    }

    /// Stop editing the highlighted item, returns false if the item was dropped
    fn stop_edit(&mut self, tree: TreeRef, discard: bool) -> bool {
        let state = self.tree_mut(tree);
        let Some(edit) = state.edit.take() else {
            self.mode = Mode::Normal;
            return true;
        };
        let is_unsaved = state.unsaved == Some(edit.item);
        state.unsaved = None;

        if is_unsaved && (discard || edit.input.value().trim().is_empty()) {
            self.drop_current(tree);
            self.mode = Mode::Normal;
            return false;
        }

        if !discard {
            if let Err(e) = self.save_edit(&edit) {
                self.notify(e, Level::Error);
            }
        }

        self.mode = Mode::Normal;
        self.refresh_all();
        true
    }

    fn discard_edit(&mut self, tree: TreeRef) {
        let has_changes = self
            .tree(tree)
            .and_then(|t| t.editing())
            .map(|e| e.input.value().trim() != self.edit_default(e.item, e.field).trim())
            .unwrap_or(false);

        if !has_changes || !self.show_confirm {
            self.stop_edit(tree, true);
            return;
        }

        self.confirm(Confirm {
            action: ConfirmAction::Discard { tree },
            message: "Discard changes? [y/N]".into(),
            confirmed: Some(("The changes were discarded".into(), Level::Info)),
            cancelled: None,
            cancel_mode: Mode::Insert,
        });
    }

    /// Remove the highlighted item without confirmation, highlighting the one above
    fn drop_current(&mut self, tree: TreeRef) {
        let state = self.tree_mut(tree);
        let index = state.highlighted.unwrap_or(0);
        let Some(item) = state.current() else { return };

        self.expanded.remove(&item);
        if let Err(e) = self.db.delete(item) {
            self.notify(e, Level::Error);
        }
        self.refresh_all();

        let state = self.tree_mut(tree);
        let index = if state.rows.is_empty() { None } else { Some(index.saturating_sub(1)) };
        state.clamp(index);
        let highlighted = state.highlighted;
        self.set_highlight(tree, highlighted);
    }

    fn set_filter(&mut self, tree: TreeRef, filter: String) {
        self.tree_mut(tree).filter = filter;
        self.rebuild(tree);
        if tree == TreeRef::Workspaces {
            self.refresh_all();
        }
    }

    // ------------------------------------------------------------------
    // Node operations

    fn start_editing_new_node(&mut self, tree: TreeRef, item: Item) -> Result<()> {
        self.tree_mut(tree).unsaved = Some(item);
        self.highlight_item(tree, item);
        self.start_edit(tree, Field::Description)
    }

    fn add_first_item(&mut self, tree: TreeRef) -> Result<Item> {
        let item = match tree {
            TreeRef::Workspaces => {
                let root = self.db.ensure_root()?;
                self.db.insert(Parent::Ws(root), false, None)?
            }
            TreeRef::Todos(ws) => self.db.insert(Parent::Ws(ws), true, None)?,
        };
        self.refresh_all();
        Ok(item)
    }

    fn add_last_item(&mut self, tree: TreeRef) -> Result<()> {
        if self.tree(tree).and_then(|t| t.editing()).is_some() {
            return Ok(());
        }
        let item = self.add_first_item(tree)?;
        self.start_editing_new_node(tree, item)
    }

    fn add_sibling(&mut self, tree: TreeRef) -> Result<()> {
        if self.tree(tree).and_then(|t| t.editing()).is_some() {
            return Ok(());
        }

        let item = if self.tree_mut(tree).rows.is_empty() {
            self.add_first_item(tree)?
        } else {
            let current = self.current_item(tree)?;
            let item = self.db.add_sibling(current)?;
            self.refresh_all();
            item
        };
        self.start_editing_new_node(tree, item)
    }

    fn add_child(&mut self, tree: TreeRef) -> Result<()> {
        let current = self.current_item(tree)?;
        let item = self.db.add_child(current)?;
        self.expanded.insert(current);
        self.refresh_all();
        self.start_editing_new_node(tree, item)
    }

    fn remove(&mut self, tree: TreeRef, item: Item) {
        self.expanded.remove(&item);
        if let Item::Ws(id) = item {
            self.todo_trees.remove(&id);
        }
        if let Err(e) = self.db.delete(item) {
            self.notify(e, Level::Error);
        }
        self.rebuild(tree);
        self.refresh_all();
    }

    fn toggle_expand_item(&mut self, item: Item) {
        if !self.expanded.remove(&item) {
            self.expanded.insert(item);
        }
        self.refresh_all();
    }

    fn paste(&mut self, tree: TreeRef, below: bool) -> Result<()> {
        let clipboard = if tree.is_todos() { self.todo_clipboard } else { self.ws_clipboard };
        let Some(source) = clipboard.filter(|s| self.db.exists(*s)) else {
            self.notify("No model in clipboard", Level::Error);
            return Ok(());
        };

        let rows_empty = self.tree(tree).map(|t| t.rows.is_empty()).unwrap_or(true);
        let (parent, position) = if rows_empty {
            let parent = match tree {
                TreeRef::Workspaces => Parent::Ws(self.db.ensure_root()?),
                TreeRef::Todos(ws) => Parent::Ws(ws),
            };
            (parent, None)
        } else {
            let current = self.current_item(tree)?;
            let parent = self.db.parent_of(current).ok_or(NO_NODE)?;
            let siblings = self.db.siblings(current);
            let index = siblings.iter().position(|s| *s == current).unwrap_or(0);
            (parent, Some(if below { index + 1 } else { index }))
        };

        let item = self.db.clone_item(source, parent, position)?;
        self.refresh_all();
        self.highlight_item(tree, item);
        Ok(())
    }

    fn copy_to_clipboard(text: &str) {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let candidates: &[(&str, &[&str])] = &[
            ("pbcopy", &[]),
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];
        for (cmd, args) in candidates {
            if let Ok(mut child) = Command::new(cmd)
                .args(*args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                drop(child.stdin.take());
                if child.wait().map(|s| s.success()).unwrap_or(false) {
                    return;
                }
            }
        }

        // fall back to OSC 52, like textual does
        let encoded = base64(text.as_bytes());
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{}\x07", encoded);
        let _ = out.flush();
    }

    // ------------------------------------------------------------------
    // Actions

    fn run(&mut self, action: Action) -> Result<()> {
        let tree = self.focused_tree();

        match action {
            Action::SwitchFocus => match self.focus {
                Focus::Workspaces => {
                    if let Some(ws) = self.current_ws() {
                        self.focus = Focus::Todos;
                        self.add_last_item(TreeRef::Todos(ws))?;
                    }
                }
                Focus::Todos => self.focus = Focus::Workspaces,
            },
            Action::MoveDown | Action::MoveUp => {
                let down = action == Action::MoveDown;
                let state = self.tree_mut(tree);
                let len = state.rows.len();
                if len == 0
                    || (down && state.highlighted == Some(len - 1))
                    || (!down && state.highlighted == Some(0))
                {
                    return Ok(());
                }
                let next = state.next_enabled(down);
                self.set_highlight(tree, next);
            }
            Action::GoToTop => {
                let index = self.tree_mut(tree).first_enabled();
                self.set_highlight(tree, index);
            }
            Action::GoToBottom => {
                let index = self.tree_mut(tree).last_enabled();
                self.set_highlight(tree, index);
            }
            Action::EditDescription => self.start_edit(tree, Field::Description)?,
            Action::EditDue => self.start_edit(tree, Field::Due)?,
            Action::EditRecurrence => self.start_edit(tree, Field::Recurrence)?,
            Action::EditEffort => self.start_edit(tree, Field::Effort)?,
            Action::AddSibling => self.add_sibling(tree)?,
            Action::AddChild => self.add_child(tree)?,
            Action::ToggleExpand => {
                let item = self.current_item(tree)?;
                self.toggle_expand_item(item);
            }
            Action::ToggleExpandParent => {
                let item = self.current_item(tree)?;
                if let Some(parent) = self.db.same_kind_parent(item) {
                    self.highlight_item(tree, parent);
                    self.toggle_expand_item(parent);
                }
            }
            Action::ShiftUp => {
                let item = self.current_item(tree)?;
                self.db.shift_up(item)?;
                self.refresh_all();
            }
            Action::ShiftDown => {
                let item = self.current_item(tree)?;
                self.db.shift_down(item)?;
                self.refresh_all();
            }
            Action::Indent => {
                let item = self.current_item(tree)?;
                if let Some(parent) = self.db.indent(item)? {
                    self.expanded.insert(parent);
                }
                self.refresh_all();
            }
            Action::Dedent => {
                let item = self.current_item(tree)?;
                self.db.dedent(item)?;
                self.refresh_all();
            }
            Action::RemoveNode => {
                let item = self.current_item(tree)?;
                if self.show_confirm {
                    self.confirm(Confirm {
                        action: ConfirmAction::Remove { tree, item },
                        message: "Are you sure? [y/N]".into(),
                        confirmed: Some(("The items were deleted".into(), Level::Error)),
                        cancelled: Some(("The items were retained".into(), Level::Info)),
                        cancel_mode: Mode::Normal,
                    });
                } else {
                    self.remove(tree, item);
                }
            }
            Action::CopyDescription => {
                let item = self.current_item(tree)?;
                Self::copy_to_clipboard(self.db.description(item));
            }
            Action::CopyModel => {
                let item = self.current_item(tree)?;
                self.notify(format!("{} was copied to clipboard", item.kind_name()), Level::Info);
                if item.is_todo() {
                    self.todo_clipboard = Some(item);
                } else {
                    self.ws_clipboard = Some(item);
                }
            }
            Action::PasteBelow => self.paste(tree, true)?,
            Action::PasteAbove => self.paste(tree, false)?,
            Action::ToggleComplete | Action::IncreaseUrgency | Action::DecreaseUrgency => {
                if !tree.is_todos() {
                    return Ok(());
                }
                let Item::Todo(id) = self.current_item(tree)? else { return Ok(()) };
                let Some(mut todo) = self.db.todo(id).cloned() else { return Ok(()) };
                match action {
                    Action::ToggleComplete => todo.pending = !todo.pending,
                    Action::IncreaseUrgency => todo.urgency += 1,
                    _ => todo.urgency -= 1,
                }
                self.db.update_todo(todo)?;
                self.refresh_all();
            }
            Action::StartSearch => {
                self.current_item(tree)?;
                self.bar = Bar::Search { input: Input::new("/"), tree };
                self.mode = Mode::Search;
            }
            Action::StartSort => {
                let item = self.current_item(tree)?;
                self.bar = Bar::Sort {
                    options: SortField::options_for(item),
                    selected: 0,
                    tree,
                    item,
                };
                self.mode = Mode::Sort;
            }
            Action::Quit => self.quit = true,
            Action::ShowHelp => self.help = Some(0),
        }
        Ok(())
    }
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
