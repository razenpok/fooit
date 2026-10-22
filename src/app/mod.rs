//! Application state and behaviour, mirroring dooit's trees, bars and API

use crate::dates;
use crate::db::{Db, Item, Parent, Snapshot, SortField};
use crate::input::Input;
use crate::keys::{self, Action, KeyManager, KeyMatch};
use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

mod actions;
mod editing;
mod mouse;
mod moving;
mod nodes;

type Result<T> = std::result::Result<T, String>;

const NO_NODE: &str = "No item selected";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Search,
    Sort,
    Confirm,
    Move,
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
    Recurrence,
}

impl Field {
    pub fn name(&self) -> &'static str {
        match self {
            Field::Status => "status",
            Field::Description => "description",
            Field::Due => "due",
            Field::Urgency => "urgency",
            Field::Recurrence => "recurrence",
        }
    }
}

pub const WORKSPACE_LAYOUT: &[Field] = &[Field::Description];
pub const TODO_LAYOUT: &[Field] = &[Field::Status, Field::Description, Field::Due, Field::Recurrence, Field::Urgency];

pub fn layout(tree: TreeRef) -> &'static [Field] {
    if tree.is_todos() { TODO_LAYOUT } else { WORKSPACE_LAYOUT }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub item: Item,
    pub depth: usize,
    pub enabled: bool,
}

/// Columns a row is indented by at a depth
pub fn indent(depth: usize) -> usize {
    2 * depth
}

/// Columns before a row's cells, after its indent, for the expand marker of items with children
pub const MARKER_WIDTH: usize = 2;

/// Each column of a row at a depth: its field, where its content starts (from the tree's left
/// edge) and its width. Columns are one space apart, with a space after the last one
pub fn row_cells(columns: &[(Field, usize)], depth: usize, total: usize) -> Vec<(Field, usize, usize)> {
    let start = indent(depth) + MARKER_WIDTH;
    let fixed: usize = columns
        .iter()
        .filter(|(f, _)| *f != Field::Description)
        .map(|(_, w)| w + 1)
        .sum::<usize>()
        + start;
    let description = total.saturating_sub(fixed + 1).max(1);

    let mut x = start;
    columns
        .iter()
        .map(|(field, width)| {
            let width = if *field == Field::Description { description } else { *width };
            let cell = (*field, x, width);
            x += width + 1;
            cell
        })
        .collect()
}

/// Where a tree was last drawn, for finding what the mouse points at
#[derive(Default)]
pub struct View {
    pub area: Rect,
    /// Row index, first line and number of lines of each row on screen
    pub rows: Vec<(usize, u16, u16)>,
    /// The column widths the rows were drawn with
    pub columns: Vec<(Field, usize)>,
}

/// A row being dragged with the mouse
pub struct Drag {
    pub tree: TreeRef,
    pub item: Item,
    pub start: (u16, u16),
    pub target: Option<DropTarget>,
    /// Another workspace a dragged todo is over, which it moves to the bottom of
    pub onto: Option<Item>,
    /// Whether the mouse moved since it was pressed, otherwise it is a click
    pub moved: bool,
    /// Where the mouse is now, and when the list last scrolled for being at its edge
    pub at: (u16, u16),
    pub scrolled: Instant,
}

/// Where a dragged row lands: after the row `above` (or first when None), at a depth
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropTarget {
    pub above: Option<usize>,
    pub depth: usize,
}

/// The id new items have on screen until they are saved with a description
pub const DRAFT_ID: i64 = -1;

pub fn is_draft(item: Item) -> bool {
    matches!(item, Item::Ws(DRAFT_ID) | Item::Todo(DRAFT_ID))
}

/// The Today view: a row at the top of the workspaces, and a list of the todos due by today
pub const TODAY_ID: i64 = -2;
pub const TODAY: TreeRef = TreeRef::Todos(TODAY_ID);
pub const TODAY_ROW: Item = Item::Ws(TODAY_ID);

/// The todos of every workspace matching a search (ctrl+f)
pub const FOUND_ID: i64 = -3;
pub const FOUND: TreeRef = TreeRef::Todos(FOUND_ID);

/// Lists made up of todos from every workspace, which have no order or nesting of their own
pub fn is_gathered(tree: TreeRef) -> bool {
    tree == TODAY || tree == FOUND
}

/// An item being moved with the keyboard, and where it was, to put it back on escape
#[derive(Clone, Copy)]
pub struct Moving {
    pub item: Item,
    pub parent: Parent,
    pub after: Option<Item>,
    pub ws: Option<i64>,
}

/// A new item being added, which only goes into the database once it has a description
#[derive(Clone, Copy)]
pub struct Draft {
    pub item: Item,
    pub parent: Parent,
    /// The sibling it goes after, or None to go first
    pub after: Option<Item>,
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
    pub draft: Option<Draft>,
    /// Per-row column widths (by layout position, ignoring any edit) and the db generation they were measured at
    pub widths: Option<(u64, Vec<Vec<usize>>)>,
    pub view: View,
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
    RemoveCompleted { ws: i64 },
    Discard { tree: TreeRef },
}

pub struct Confirm {
    pub action: ConfirmAction,
    pub message: String,
    pub confirmed: Option<(String, Level)>,
    pub cancelled: Option<(String, Level)>,
    pub cancel_mode: Mode,
    /// Whether enter answers yes rather than no
    pub default_yes: bool,
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
    /// Items start expanded, so only the collapsed ones are tracked
    pub collapsed: HashSet<Item>,
    pub show_completed: bool,
    /// A count typed before an action, like the 3 in `3s`
    pub count: Option<u32>,
    /// Whether the search of every workspace is open, in place of the workspace's todos
    pub finding: bool,
    pub drag: Option<Drag>,
    pub moving: Option<Moving>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The data as it was after the last change, and its generation
    undo_base: (u64, Snapshot),
    /// The last change only put things back, so it is not one to undo
    undo_skip: bool,
    pub keys: KeyManager,
    pub bar: Bar,
    pub show_confirm: bool,
    pub help: Option<u16>,
    pub quit: bool,
    ws_clipboard: Option<Item>,
    todo_clipboard: Option<Item>,
    last_poll: Instant,
}

impl App {
    pub fn new(db: Db) -> App {
        let undo_base = (db.generation, db.snapshot());
        let mut app = App {
            db,
            ws_tree: TreeState::default(),
            todo_trees: HashMap::new(),
            focus: Focus::Workspaces,
            mode: Mode::Normal,
            collapsed: HashSet::new(),
            show_completed: false,
            count: None,
            finding: false,
            drag: None,
            moving: None,
            undo: vec![],
            redo: vec![],
            undo_base,
            undo_skip: false,
            keys: KeyManager::with_defaults(),
            bar: Bar::Status,
            show_confirm: true,
            help: None,
            quit: false,
            ws_clipboard: None,
            todo_clipboard: None,
            last_poll: Instant::now(),
        };
        app.refresh_all();
        app
    }

    /// Open on the first workspace's todos, adding a new one
    pub fn start(&mut self) {
        let first = self.ws_tree.rows.iter().position(|r| r.item != TODAY_ROW);
        self.set_highlight(TreeRef::Workspaces, first);
        if self.current_ws().is_some() {
            if let Err(e) = self.run(Action::SwitchFocus) {
                self.notify(e, Level::Error);
            }
        }
    }

    /// Keep a new item that is still being added if something was typed for it
    pub fn close(&mut self) {
        let mut trees = vec![TreeRef::Workspaces];
        trees.extend(self.todo_trees.keys().map(|ws| TreeRef::Todos(*ws)));
        for tree in trees {
            if self.tree(tree).is_some_and(|t| t.draft.is_some() && t.editing().is_some()) {
                self.stop_edit(tree, false);
            }
        }
    }

    // ------------------------------------------------------------------
    // Tree bookkeeping

    pub fn current_ws(&self) -> Option<i64> {
        match self.ws_tree.current() {
            Some(Item::Ws(id)) if id != DRAFT_ID => Some(id),
            _ => None,
        }
    }

    pub fn focused_tree(&self) -> TreeRef {
        match (self.focus, self.current_ws()) {
            (Focus::Todos, _) if self.finding => FOUND,
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
        let draft = state.draft;
        let highlighted_item = state.current();
        let highlighted_index = state.highlighted;

        // a completed todo, and everything beneath it, stays until the cursor leaves them
        let mut kept = HashSet::new();
        let mut item = highlighted_item;
        while let Some(i) = item {
            kept.insert(i);
            item = self.db.same_kind_parent(i);
        }

        if is_gathered(tree) {
            let filter = filter.to_lowercase();
            let rows = if tree == TODAY { self.today_rows(&kept, &filter) } else { self.found_rows(&kept, &filter) };
            self.set_rows(tree, rows, highlighted_item, highlighted_index);
            return;
        }

        struct Walk<'a> {
            app: &'a App,
            todos: bool,
            filter: &'a str,
            kept: &'a HashSet<Item>,
            draft: Option<Draft>,
            rows: Vec<Row>,
        }
        fn walk(w: &mut Walk, parent: Parent, depth: usize, in_kept: bool) {
            let draft = w.draft.filter(|d| d.parent == parent);
            let draft_row = |d: Draft| Row { item: d.item, depth, enabled: true };
            let mut placed = false;
            if let Some(d) = draft.filter(|d| d.after.is_none()) {
                w.rows.push(draft_row(d));
                placed = true;
            }
            for child in w.app.db.children(parent, w.todos) {
                let completed = w.app.is_completed(child);
                let kept = completed && w.kept.contains(&child);
                if !completed || w.app.show_completed || in_kept || kept {
                    let enabled = w.app.db.description(child).to_lowercase().contains(w.filter);
                    w.rows.push(Row { item: child, depth, enabled });
                    if !w.app.collapsed.contains(&child) || !w.filter.is_empty() {
                        walk(w, child.as_parent(), depth + 1, in_kept || kept);
                    }
                }
                if let Some(d) = draft.filter(|d| !placed && d.after == Some(child)) {
                    w.rows.push(draft_row(d));
                    placed = true;
                }
            }
            if let Some(d) = draft.filter(|_| !placed) {
                w.rows.push(draft_row(d));
            }
        }
        // search ignores case
        let filter = filter.to_lowercase();
        let mut w = Walk { app: self, todos, filter: &filter, kept: &kept, draft, rows: vec![] };
        if let Some(root) = root {
            walk(&mut w, root, 0, false);
        }
        let mut rows = w.rows;
        if tree == TreeRef::Workspaces && !rows.is_empty() {
            rows.insert(0, Row { item: TODAY_ROW, depth: 0, enabled: "today".contains(&filter) });
        }
        self.set_rows(tree, rows, highlighted_item, highlighted_index);
    }

    /// The todos due today or before, soonest first, plus a completed one under the cursor
    fn today_rows(&self, kept: &HashSet<Item>, filter: &str) -> Vec<Row> {
        let today = dates::now().date();
        let mut due: Vec<_> = self
            .db
            .todos
            .values()
            .filter(|t| t.due.is_some_and(|d| d.date() <= today))
            .filter(|t| t.pending || kept.contains(&Item::Todo(t.id)) || (self.show_completed && t.due.unwrap().date() == today))
            .map(|t| (t.due, t.description.to_lowercase(), Item::Todo(t.id)))
            .collect();
        due.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        due.into_iter()
            .map(|(_, description, item)| Row { item, depth: 0, enabled: description.contains(filter) })
            .collect()
    }

    /// The todos of every workspace matching a search, in the order of the workspaces
    fn found_rows(&self, kept: &HashSet<Item>, filter: &str) -> Vec<Row> {
        fn walk(app: &App, parent: Parent, kept: &HashSet<Item>, filter: &str, rows: &mut Vec<Row>) {
            for ws in app.db.children(parent, false) {
                walk(app, ws.as_parent(), kept, filter, rows);
            }
            for todo in app.db.children(parent, true) {
                let shown = app.is_shown(todo) || kept.contains(&todo);
                if shown && app.db.description(todo).to_lowercase().contains(filter) {
                    rows.push(Row { item: todo, depth: 0, enabled: true });
                }
                walk(app, todo.as_parent(), kept, filter, rows);
            }
        }
        let mut rows = vec![];
        if let (false, Some(root)) = (filter.is_empty(), self.db.root_id()) {
            walk(self, Parent::Ws(root), kept, filter, &mut rows);
        }
        rows
    }

    /// Close the search of every workspace
    pub(super) fn close_find(&mut self) {
        self.finding = false;
        self.tree_mut(FOUND).filter.clear();
        self.rebuild(FOUND);
    }

    /// Go to a todo in its workspace, opening the items it is in
    pub(super) fn jump_to(&mut self, item: Item) {
        self.close_find();
        let Some(ws) = self.workspace_of(item) else { return };
        let mut parent = self.db.same_kind_parent(item);
        while let Some(p) = parent {
            self.collapsed.remove(&p);
            parent = self.db.same_kind_parent(p);
        }
        self.highlight_item(TreeRef::Workspaces, ws);
        self.focus = Focus::Todos;
        self.refresh_all();
        if let Item::Ws(id) = ws {
            self.highlight_item(TreeRef::Todos(id), item);
        }
    }

    /// Open todos due today or before, in every workspace
    /// Open todos that are overdue, and that are due later today, in every workspace
    pub fn due_today(&self) -> (usize, usize) {
        let today = dates::now().date();
        let due: Vec<_> = self.db.todos.values().filter(|t| t.pending && t.due.is_some_and(|d| d.date() <= today)).collect();
        let overdue = due.iter().filter(|t| dates::is_overdue(t.due, t.pending)).count();
        (overdue, due.len() - overdue)
    }

    /// The workspace a todo is in, however deep
    pub fn workspace_of(&self, item: Item) -> Option<Item> {
        let mut parent = self.db.parent_of(item)?;
        loop {
            match parent {
                Parent::Ws(id) => return Some(Item::Ws(id)),
                Parent::Todo(id) => parent = self.db.parent_of(Item::Todo(id))?,
            }
        }
    }

    fn set_rows(&mut self, tree: TreeRef, rows: Vec<Row>, highlighted_item: Option<Item>, highlighted_index: Option<usize>) {
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
                state.draft = None;
            }
        }
    }

    pub fn refresh_all(&mut self) {
        self.rebuild(TreeRef::Workspaces);
        let existing: Vec<i64> = self.todo_trees.keys().copied().collect();
        for ws in existing {
            if ws == TODAY_ID || ws == FOUND_ID || self.db.exists(Item::Ws(ws)) {
                self.rebuild(TreeRef::Todos(ws));
            } else {
                self.todo_trees.remove(&ws);
            }
        }
        if let Some(ws) = self.current_ws() {
            self.tree_mut(TreeRef::Todos(ws));
        }

        if self.current_ws().is_none() && !self.finding {
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
        self.highlight_moved(tree);
    }

    fn set_highlight(&mut self, tree: TreeRef, index: Option<usize>) {
        self.tree_mut(tree).highlighted = index;
        self.highlight_moved(tree);
    }

    fn highlight_moved(&mut self, tree: TreeRef) {
        if tree == TreeRef::Workspaces {
            self.refresh_all();
            return;
        }
        // completed todos kept for the cursor may have to go now
        let shows_completed = self.tree(tree).is_some_and(|t| t.rows.iter().any(|r| self.is_completed(r.item)));
        if !self.show_completed && shows_completed {
            self.rebuild(tree);
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
        let mut changed = self.scroll_drag();
        // the drop target follows the rows that scrolled under the mouse
        if changed {
            if let Some(drag) = &self.drag {
                let target = self.drop_target(drag, drag.at.0, drag.at.1);
                if let Some(drag) = self.drag.as_mut() {
                    drag.target = target;
                }
            }
        }
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
                // changes made elsewhere are not ours to undo
                if self.mode != Mode::Move {
                    self.undo_base = (self.db.generation, self.db.snapshot());
                }
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
        self.handle_key(event);
        self.record_change();
    }

    pub fn on_mouse(&mut self, event: MouseEvent) {
        self.handle_mouse(event);
        self.record_change();
    }

    /// Keep what the data was before a change, for undo. A whole move counts as one change
    fn record_change(&mut self) {
        if self.mode == Mode::Move || self.db.generation == self.undo_base.0 {
            return;
        }
        let before = std::mem::replace(&mut self.undo_base, (self.db.generation, self.db.snapshot()));
        if !std::mem::take(&mut self.undo_skip) {
            self.undo.push(before.1);
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
    }

    fn undo(&mut self, redo: bool) -> Result<()> {
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some(snapshot) = from.pop() else {
            self.notify(if redo { "Nothing to redo" } else { "Nothing to undo" }, Level::Info);
            return Ok(());
        };
        to.push(self.db.snapshot());
        self.db.restore(&snapshot)?;
        self.undo_base = (self.db.generation, snapshot);
        self.refresh_all();
        self.notify(if redo { "Redone" } else { "Undone" }, Level::Info);
        Ok(())
    }

    fn handle_key(&mut self, event: KeyEvent) {
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

        if self.mode == Mode::Move {
            if let Err(e) = self.move_keypress(&key) {
                self.notify(e, Level::Error);
            }
            return;
        }

        // digits before an action count it, unless they are bound themselves
        let digit = key.len() == 1 && key.chars().all(|c| c.is_ascii_digit()) && (key != "0" || self.count.is_some());
        if self.mode == Mode::Normal && digit && !self.keys.pending() && !self.keys.is_bound(&key) {
            let count = self.count.unwrap_or(0).saturating_mul(10).saturating_add(key.parse::<u32>().unwrap());
            self.count = Some(count.min(9999));
            return;
        }

        match self.keys.register(&key, self.mode == Mode::Normal) {
            KeyMatch::NoMatch => {
                self.count = None;
                self.tree_keypress(&key);
            }
            KeyMatch::Multiple => {}
            KeyMatch::Found(action) => {
                if let Err(e) = self.run(action) {
                    self.notify(e, Level::Error);
                }
                self.count = None;
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
                    if tree == FOUND {
                        self.close_find();
                    }
                }
                _ => {
                    input.keypress(key);
                    let filter: String = input.value().chars().skip(1).collect();
                    self.set_filter(tree, filter);
                    // the results start at the first match
                    if tree == FOUND {
                        let state = self.tree_mut(FOUND);
                        state.highlighted = state.first_enabled();
                    }
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
                let cancel = !(key.to_lowercase() == "y" || (confirm.default_yes && key == "enter"));
                if !cancel {
                    match confirm.action {
                        ConfirmAction::Remove { tree, item } => self.remove(tree, item),
                        ConfirmAction::RemoveCompleted { ws } => self.remove_completed(ws),
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
                if tree == FOUND {
                    self.close_find();
                }
            }
            return;
        };

        let result = match (key, field) {
            ("enter", Field::Description) => {
                // an empty list is left when a #workspace sent the only new todo elsewhere
                if self.stop_edit(tree, false)
                    && self.current_item(tree).map(|i| !self.db.description(i).is_empty()).unwrap_or(true)
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
            // with nothing typed, left and right leave the edit and move on like outside it
            ("left" | "right", Field::Description) if self.editing_empty(tree) => self.leave_empty_edit(tree, key == "left"),
            ("escape" | "enter", _) => {
                self.stop_edit(tree, false);
                Ok(())
            }
            ("up" | "down", _) => {
                // pick the target first, the edited item may be dropped when it stays empty
                let new_at_bottom = key == "up" && self.at_top_of_todos(tree);
                let target = self.sibling_target(tree, key == "down");
                self.stop_edit(tree, false);
                if new_at_bottom {
                    self.add_last_item(tree)
                } else {
                    if let Some(item) = target {
                        self.highlight_item(tree, item);
                    }
                    Ok(())
                }
            }
            ("tab" | "shift+tab", _) => self.reindent_edit(tree, key == "tab"),
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

}
