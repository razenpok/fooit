//! Application state and behaviour, mirroring dooit's trees, bars and API

use crate::dates;
use crate::db::{Db, Item, Parent, Snapshot, SortField};
use crate::input::Input;
use crate::keys::{self, Action, KeyManager, KeyMatch};
use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
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
        let first = self.ws_tree.first_enabled();
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
        let rows = w.rows;

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

    /// Whether the mouse is ours; while editing it is left to the terminal for selecting text
    pub fn wants_mouse(&self) -> bool {
        self.mode != Mode::Insert
    }

    fn handle_mouse(&mut self, event: MouseEvent) {
        if self.help.is_some() || self.bar_focused() || !self.wants_mouse() || self.mode == Mode::Move {
            return;
        }
        let (x, y) = (event.column, event.row);
        let result = match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(x, y),
            MouseEventKind::Drag(MouseButton::Left) => {
                self.mouse_drag(x, y);
                Ok(())
            }
            MouseEventKind::Up(MouseButton::Left) => self.mouse_up(),
            MouseEventKind::ScrollDown if self.mode == Mode::Normal => self.run(Action::MoveDown),
            MouseEventKind::ScrollUp if self.mode == Mode::Normal => self.run(Action::MoveUp),
            _ => Ok(()),
        };
        if let Err(e) = result {
            self.notify(e, Level::Error);
        }
    }

    /// The shown tree and row under a screen position
    fn row_at(&self, x: u16, y: u16) -> Option<(TreeRef, usize)> {
        let mut trees = vec![TreeRef::Workspaces];
        trees.extend(self.current_ws().map(TreeRef::Todos));
        trees.into_iter().find_map(|tree| {
            let view = &self.tree(tree)?.view;
            if !view.area.contains(Position::new(x, y)) {
                return None;
            }
            let (index, _, _) = view.rows.iter().find(|(_, top, height)| (*top..top + height).contains(&y))?;
            Some((tree, *index))
        })
    }

    fn mouse_down(&mut self, x: u16, y: u16) -> Result<()> {
        let Some((tree, index)) = self.row_at(x, y) else { return Ok(()) };
        let item = self.tree(tree).unwrap().rows[index].item;
        let depth = self.tree(tree).unwrap().rows[index].depth;

        self.focus = if tree.is_todos() { Focus::Todos } else { Focus::Workspaces };
        self.highlight_item(tree, item);

        if self.cell_at(tree, depth, x).is_some_and(|(field, _, _)| field == Field::Status) {
            return self.run(Action::ToggleComplete);
        }
        let marker = self.tree(tree).unwrap().view.area.x + indent(depth) as u16;
        if (marker..marker + MARKER_WIDTH as u16).contains(&x) && self.has_children(item) {
            self.toggle_expand_item(item);
            return Ok(());
        }

        self.drag = Some(Drag { tree, item, start: (x, y), target: None, onto: None, moved: false });
        Ok(())
    }

    /// The column under a screen column, with its content's screen column and width
    fn cell_at(&self, tree: TreeRef, depth: usize, x: u16) -> Option<(Field, u16, usize)> {
        let view = &self.tree(tree)?.view;
        row_cells(&view.columns, depth, view.area.width as usize)
            .into_iter()
            .map(|(field, start, width)| (field, view.area.x + start as u16, width))
            .find(|(_, start, width)| (*start..start + *width as u16 + 1).contains(&x))
    }

    /// Start editing the clicked column of a todo, with the cursor where the click was
    fn click_edit(&mut self, tree: TreeRef, item: Item, (x, y): (u16, u16)) -> Result<()> {
        let Some(state) = self.tree(tree).filter(|_| tree.is_todos()) else { return Ok(()) };
        let Some(index) = state.index_of(item) else { return Ok(()) };
        let Some(&(_, top, _)) = state.view.rows.iter().find(|(i, _, _)| *i == index) else { return Ok(()) };
        let Some((field, start, width)) = self.cell_at(tree, state.rows[index].depth, x) else { return Ok(()) };
        if field == Field::Status {
            return Ok(());
        }

        self.start_edit(tree, field)?;
        let Some(edit) = self.tree_mut(tree).edit.as_mut().filter(|e| e.item == item) else { return Ok(()) };
        let column = x.saturating_sub(start) as usize;
        let line = if field == Field::Description { (y - top) as usize } else { 0 };
        let wrap_width = if field == Field::Description { width } else { usize::MAX / 2 };
        let cursor = crate::ui::char_at(&edit.input.value(), wrap_width, line, column);
        edit.input.set_cursor(cursor);
        Ok(())
    }

    fn mouse_drag(&mut self, x: u16, y: u16) {
        let Some(drag) = &self.drag else { return };
        if !drag.moved && drag.start == (x, y) {
            return;
        }
        let (target, onto) = if drag.tree.is_todos() && self.ws_tree.view.area.contains(Position::new(x, y)) {
            let current = self.current_ws().map(Item::Ws);
            let onto = self.row_at(x, y).map(|(_, i)| self.ws_tree.rows[i].item).filter(|ws| Some(*ws) != current);
            (None, onto)
        } else {
            (self.drop_target(drag, x, y), None)
        };
        if let Some(drag) = self.drag.as_mut() {
            drag.target = target;
            drag.onto = onto;
            drag.moved = true;
        }
    }

    fn mouse_up(&mut self) -> Result<()> {
        let Some(drag) = self.drag.take() else { return Ok(()) };
        if let Some(ws) = drag.onto {
            return self.move_to_workspace(drag.item, ws);
        }
        match drag.target {
            Some(target) => self.drop_item(drag.tree, drag.item, target),
            None if !drag.moved => self.click_edit(drag.tree, drag.item, drag.start),
            None => Ok(()),
        }
    }

    fn move_to_workspace(&mut self, item: Item, ws: Item) -> Result<()> {
        let last = self.db.children(ws.as_parent(), true).last().copied();
        self.db.move_to(item, ws.as_parent(), last)?;
        self.refresh_all();
        self.notify(format!("Moved to {}", self.db.description(ws)), Level::Info);
        Ok(())
    }

    /// The rows a dragged item takes along: itself and everything shown beneath it
    fn dragged_rows(state: &TreeState, item: Item) -> Option<std::ops::Range<usize>> {
        let from = state.index_of(item)?;
        let depth = state.rows[from].depth;
        let len = state.rows[from + 1..].iter().take_while(|r| r.depth > depth).count();
        Some(from..from + 1 + len)
    }

    /// The gap the pointer is over, and the depth the horizontal movement asks for, limited to
    /// what fits between the rows around the gap
    fn drop_target(&self, drag: &Drag, x: u16, y: u16) -> Option<DropTarget> {
        let state = self.tree(drag.tree)?;
        let rows = &state.rows;
        let dragged = Self::dragged_rows(state, drag.item)?;

        let pointed = state.view.rows.iter().find(|(_, top, height)| y < top + height).or(state.view.rows.last())?.0;
        let (above, below) = if pointed < dragged.start {
            (pointed.checked_sub(1), Some(pointed))
        } else if pointed >= dragged.end {
            (Some(pointed), Some(pointed + 1).filter(|b| *b < rows.len()))
        } else {
            (dragged.start.checked_sub(1), Some(dragged.end).filter(|b| *b < rows.len()))
        };

        let max = above.map_or(0, |a| rows[a].depth + 1);
        let min = below.map_or(0, |b| rows[b].depth).min(max);
        let wanted = indent(rows[dragged.start].depth) as i32 + x as i32 - drag.start.0 as i32;
        let depth = (min..=max).min_by_key(|d| (indent(*d) as i32 - wanted).abs()).unwrap_or(min);
        Some(DropTarget { above, depth })
    }

    fn drop_item(&mut self, tree: TreeRef, item: Item, target: DropTarget) -> Result<()> {
        let state = self.tree(tree).ok_or(NO_NODE)?;
        let rows = &state.rows;
        let dragged = Self::dragged_rows(state, item).ok_or(NO_NODE)?;

        let opened = target.above.filter(|a| target.depth > rows[*a].depth).map(|a| rows[a].item);
        let (parent, after) = match target.above {
            None => (self.tree_root(tree).ok_or(NO_NODE)?, None),
            Some(a) if target.depth > rows[a].depth => (rows[a].item.as_parent(), None),
            Some(a) => {
                // the row at the target depth that `above` sits in
                let anchor = (0..=a)
                    .rev()
                    .filter(|i| !dragged.contains(i))
                    .find(|i| rows[*i].depth <= target.depth)
                    .map(|i| rows[i].item)
                    .ok_or(NO_NODE)?;
                (self.db.parent_of(anchor).ok_or(NO_NODE)?, Some(anchor))
            }
        };

        // a new parent opens up to show the dropped item
        if let Some(opened) = opened {
            self.collapsed.remove(&opened);
        }
        self.db.move_to(item, parent, after)?;
        self.refresh_all();
        self.highlight_item(tree, item);
        Ok(())
    }

    fn start_move(&mut self, tree: TreeRef) -> Result<()> {
        let item = self.current_item(tree)?;
        let parent = self.db.parent_of(item).ok_or(NO_NODE)?;
        let siblings = self.db.siblings(item);
        let index = siblings.iter().position(|s| *s == item).unwrap_or(0);
        let after = index.checked_sub(1).map(|i| siblings[i]);
        self.moving = Some(Moving { item, parent, after, ws: self.current_ws() });
        self.mode = Mode::Move;
        Ok(())
    }

    /// The tree the item being moved is in, after a move to another workspace too
    fn moving_tree(&self, item: Item) -> TreeRef {
        match (item, self.current_ws()) {
            (Item::Todo(_), Some(ws)) => TreeRef::Todos(ws),
            _ => TreeRef::Workspaces,
        }
    }

    fn move_keypress(&mut self, key: &str) -> Result<()> {
        let Some(moving) = self.moving else {
            self.mode = Mode::Normal;
            return Ok(());
        };
        let item = moving.item;

        match key {
            "enter" => {
                self.moving = None;
                self.mode = Mode::Normal;
                self.focus = if item.is_todo() { Focus::Todos } else { Focus::Workspaces };
            }
            "escape" => {
                if self.db.exists(item) {
                    self.db.move_to(item, moving.parent, moving.after)?;
                }
                self.undo_skip = true;
                self.moving = None;
                self.mode = Mode::Normal;
                if let Some(ws) = moving.ws {
                    self.highlight_item(TreeRef::Workspaces, Item::Ws(ws));
                }
                self.focus = if item.is_todo() { Focus::Todos } else { Focus::Workspaces };
            }
            // a todo picks its workspace in the workspace list
            "tab" if item.is_todo() => {
                self.focus = if self.focus == Focus::Todos { Focus::Workspaces } else { Focus::Todos };
            }
            "up" | "k" | "down" | "j" if item.is_todo() && self.focus == Focus::Workspaces => {
                let down = matches!(key, "down" | "j");
                if let Some(Item::Ws(ws)) = self.move_target(TreeRef::Workspaces, down) {
                    let last = self.db.children(Parent::Ws(ws), true).last().copied();
                    self.db.move_to(item, Parent::Ws(ws), last.filter(|l| *l != item))?;
                    self.highlight_item(TreeRef::Workspaces, Item::Ws(ws));
                }
            }
            "up" | "k" => {
                self.db.shift_up(item)?;
            }
            "down" | "j" => {
                self.db.shift_down(item)?;
            }
            "right" | "l" => {
                if let Some(parent) = self.db.indent(item)? {
                    self.collapsed.remove(&parent);
                }
            }
            "left" | "h" => {
                self.db.dedent(item)?;
            }
            _ => return Ok(()),
        }
        self.refresh_all();
        let tree = self.moving_tree(item);
        self.highlight_item(tree, item);
        Ok(())
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
            ("up" | "down", _) => {
                // pick the target first, the edited item may be dropped when it stays empty
                let target = self.sibling_target(tree, key == "down");
                self.stop_edit(tree, false);
                if let Some(item) = target {
                    self.highlight_item(tree, item);
                }
                Ok(())
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
        if let Some(draft) = state.draft.take() {
            self.mode = Mode::Normal;
            return self.finish_draft(tree, draft, &edit, discard);
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

    /// Add a new item to the database once it has a description, otherwise drop it and
    /// highlight the row above; returns whether it was added
    fn finish_draft(&mut self, tree: TreeRef, draft: Draft, edit: &Edit, discard: bool) -> bool {
        let description = edit.input.value().trim().to_string();
        if !discard && !description.is_empty() {
            let siblings = self.db.children(draft.parent, draft.item.is_todo());
            let position = draft.after.and_then(|a| siblings.iter().position(|s| *s == a)).map_or(0, |i| i + 1);
            let added = if draft.item.is_todo() {
                self.db.quick_add_todo(draft.parent, Some(position), &description)
            } else {
                self.db.insert_with_description(draft.parent, false, Some(position), &description)
            };
            match added {
                Ok(item) => {
                    self.refresh_all();
                    self.highlight_item(tree, item);
                    return true;
                }
                Err(e) => self.notify(e, Level::Error),
            }
        }

        let index = self.tree_mut(tree).highlighted.unwrap_or(0);
        self.refresh_all();
        let state = self.tree_mut(tree);
        let index = if state.rows.is_empty() { None } else { Some(index.saturating_sub(1)) };
        state.clamp(index);
        let highlighted = state.highlighted;
        self.set_highlight(tree, highlighted);
        false
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

    /// Start adding a new item under `parent`, after the sibling `after` (or first)
    fn start_draft(&mut self, tree: TreeRef, parent: Parent, after: Option<Item>) -> Result<()> {
        let item = if tree.is_todos() { Item::Todo(DRAFT_ID) } else { Item::Ws(DRAFT_ID) };
        self.tree_mut(tree).draft = Some(Draft { item, parent, after });
        self.refresh_all();
        self.highlight_item(tree, item);
        self.start_edit(tree, Field::Description)
    }

    fn add_last_item(&mut self, tree: TreeRef) -> Result<()> {
        if self.tree(tree).and_then(|t| t.editing()).is_some() {
            return Ok(());
        }
        let parent = match tree {
            TreeRef::Workspaces => Parent::Ws(self.db.ensure_root()?),
            TreeRef::Todos(ws) => Parent::Ws(ws),
        };
        let last = self.db.children(parent, tree.is_todos()).last().copied();
        self.start_draft(tree, parent, last)
    }

    fn add_sibling(&mut self, tree: TreeRef) -> Result<()> {
        if self.tree(tree).and_then(|t| t.editing()).is_some() {
            return Ok(());
        }
        if self.tree_mut(tree).rows.is_empty() {
            return self.add_last_item(tree);
        }
        let current = self.current_item(tree)?;
        let parent = self.db.parent_of(current).ok_or("Cannot add a sibling to the root")?;
        self.start_draft(tree, parent, Some(current))
    }

    fn add_child(&mut self, tree: TreeRef) -> Result<()> {
        let current = self.current_item(tree)?;
        let last = self.db.children(current.as_parent(), current.is_todo()).last().copied();
        self.collapsed.remove(&current);
        self.start_draft(tree, current.as_parent(), last)
    }

    /// Nest the item being edited under the one above it, or move it out of its parent, and
    /// keep editing it; a new item that is not saved yet only moves on screen
    fn reindent_edit(&mut self, tree: TreeRef, indent: bool) -> Result<()> {
        let Some(item) = self.tree(tree).and_then(|t| t.editing()).map(|e| e.item) else { return Ok(()) };
        let Some(draft) = self.tree_mut(tree).draft.filter(|d| d.item == item) else {
            if indent {
                if let Some(parent) = self.db.indent(item)? {
                    self.collapsed.remove(&parent);
                }
            } else {
                self.db.dedent(item)?;
            }
            self.refresh_all();
            return Ok(());
        };

        let moved = if indent {
            // the last child of the sibling above
            let Some(above) = draft.after else { return Ok(()) };
            self.collapsed.remove(&above);
            let last = self.db.children(above.as_parent(), item.is_todo()).last().copied();
            Draft { parent: above.as_parent(), after: last, ..draft }
        } else {
            // right after its parent, when that is an item of the same kind
            let parent = match draft.parent {
                Parent::Todo(id) if item.is_todo() => Item::Todo(id),
                Parent::Ws(id) if !item.is_todo() => Item::Ws(id),
                _ => return Ok(()),
            };
            let Some(grandparent) = self.db.parent_of(parent) else { return Ok(()) };
            Draft { parent: grandparent, after: Some(parent), ..draft }
        };
        self.tree_mut(tree).draft = Some(moved);
        self.refresh_all();
        Ok(())
    }

    fn remove(&mut self, tree: TreeRef, item: Item) {
        self.collapsed.remove(&item);
        if let Item::Ws(id) = item {
            self.todo_trees.remove(&id);
        }
        if let Err(e) = self.db.delete(item) {
            self.notify(e, Level::Error);
        }
        self.rebuild(tree);
        self.refresh_all();
    }

    pub fn has_children(&self, item: Item) -> bool {
        !self.db.children(item.as_parent(), item.is_todo()).is_empty()
    }

    /// Completed and all children of a todo
    pub fn progress(&self, item: Item) -> (usize, usize) {
        let children = self.db.children(item.as_parent(), true);
        let done = children.iter().filter(|c| self.is_completed(**c)).count();
        (done, children.len())
    }

    /// Open todos in a workspace, nested ones included
    pub fn open_todos(&self, ws: Item) -> usize {
        fn count(app: &App, parent: Parent) -> usize {
            app.db
                .children(parent, true)
                .into_iter()
                .map(|t| usize::from(!app.is_completed(t)) + count(app, t.as_parent()))
                .sum()
        }
        count(self, ws.as_parent())
    }

    pub fn is_completed(&self, item: Item) -> bool {
        match item {
            Item::Todo(id) => self.db.todo(id).is_some_and(|t| !t.pending),
            Item::Ws(_) => false,
        }
    }

    /// The completed todos beneath a parent, leaving out those inside another completed todo
    fn completed_todos(&self, parent: Parent) -> Vec<Item> {
        let mut found = vec![];
        for child in self.db.children(parent, true) {
            if self.is_completed(child) {
                found.push(child);
            } else {
                found.extend(self.completed_todos(child.as_parent()));
            }
        }
        found
    }

    fn remove_completed(&mut self, ws: i64) {
        for item in self.completed_todos(Parent::Ws(ws)) {
            self.remove(TreeRef::Todos(ws), item);
        }
    }

    fn toggle_expand_item(&mut self, item: Item) {
        if !self.collapsed.remove(&item) {
            self.collapsed.insert(item);
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

    /// The row moving the cursor up or down lands on, wrapping around the ends of the list
    fn move_target(&mut self, tree: TreeRef, down: bool) -> Option<Item> {
        let state = self.tree_mut(tree);
        if state.rows.is_empty() {
            return None;
        }
        state.next_enabled(down).map(|i| state.rows[i].item)
    }

    /// The next or previous item on the highlighted item's level, wrapping around its siblings
    fn sibling_target(&mut self, tree: TreeRef, down: bool) -> Option<Item> {
        let state = self.tree_mut(tree);
        let rows = &state.rows;
        let Some(h) = state.highlighted.filter(|h| *h < rows.len()) else {
            return self.move_target(tree, down);
        };
        let depth = rows[h].depth;
        let start = (0..h).rev().find(|i| rows[*i].depth < depth).map_or(0, |parent| parent + 1);
        let end = (h + 1..rows.len()).find(|i| rows[*i].depth < depth).unwrap_or(rows.len());
        let siblings: Vec<usize> = (start..end).filter(|i| rows[*i].depth == depth && rows[*i].enabled).collect();
        let n = siblings.len();
        let next = match siblings.iter().position(|i| *i == h) {
            _ if n == 0 => return None,
            Some(at) if down => siblings[(at + 1) % n],
            Some(at) => siblings[(at + n - 1) % n],
            None if down => siblings[0],
            None => siblings[n - 1],
        };
        Some(rows[next].item)
    }

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
                if let Some(item) = self.move_target(tree, action == Action::MoveDown) {
                    self.highlight_item(tree, item);
                }
            }
            Action::NextSibling | Action::PreviousSibling => {
                if let Some(item) = self.sibling_target(tree, action == Action::NextSibling) {
                    self.highlight_item(tree, item);
                }
            }
            Action::IntoChildren => {
                let item = self.current_item(tree)?;
                if self.collapsed.remove(&item) {
                    self.refresh_all();
                }
                let state = self.tree_mut(tree);
                let child = state.index_of(item).and_then(|i| state.rows.get(i + 1).filter(|r| r.depth > state.rows[i].depth));
                if let Some(child) = child.map(|r| r.item) {
                    self.highlight_item(tree, child);
                }
            }
            Action::ToParent => {
                let item = self.current_item(tree)?;
                if let Some(parent) = self.db.same_kind_parent(item) {
                    self.highlight_item(tree, parent);
                }
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
                    self.collapsed.remove(&parent);
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
            Action::ToggleShowCompleted => {
                self.show_completed = !self.show_completed;
                self.refresh_all();
                let verb = if self.show_completed { "Showing" } else { "Hiding" };
                self.notify(format!("{} completed todos", verb), Level::Info);
            }
            Action::RemoveCompleted => {
                let ws = self.current_ws().ok_or_else(|| NO_NODE.to_string())?;
                let count = self.completed_todos(Parent::Ws(ws)).len();
                if count == 0 {
                    self.notify("No completed todos", Level::Info);
                    return Ok(());
                }
                let message = format!("Remove {} completed todo{}? [y/N]", count, if count == 1 { "" } else { "s" });
                let action = ConfirmAction::RemoveCompleted { ws };
                if self.show_confirm {
                    self.confirm(Confirm {
                        action,
                        message,
                        confirmed: Some(("The completed todos were deleted".into(), Level::Error)),
                        cancelled: Some(("The completed todos were retained".into(), Level::Info)),
                        cancel_mode: Mode::Normal,
                    });
                } else {
                    self.remove_completed(ws);
                }
            }
            Action::StartMove => self.start_move(tree)?,
            Action::Undo => self.undo(false)?,
            Action::Redo => self.undo(true)?,
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
