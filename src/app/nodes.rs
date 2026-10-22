//! Adding, removing, nesting and copying items

use super::*;

impl App {
    /// Start adding a new item under `parent`, after the sibling `after` (or first)
    pub(super) fn start_draft(&mut self, tree: TreeRef, parent: Parent, after: Option<Item>) -> Result<()> {
        let item = if tree.is_todos() { Item::Todo(DRAFT_ID) } else { Item::Ws(DRAFT_ID) };
        self.tree_mut(tree).draft = Some(Draft { item, parent, after });
        self.refresh_all();
        self.highlight_item(tree, item);
        self.start_edit(tree, Field::Description)
    }

    pub(super) fn add_last_item(&mut self, tree: TreeRef) -> Result<()> {
        if is_gathered(tree) || self.tree(tree).and_then(|t| t.editing()).is_some() {
            return Ok(());
        }
        let parent = match tree {
            TreeRef::Workspaces => Parent::Ws(self.db.ensure_root()?),
            TreeRef::Todos(ws) => Parent::Ws(ws),
        };
        let last = self.db.children(parent, tree.is_todos()).last().copied();
        self.start_draft(tree, parent, last)
    }

    pub(super) fn add_sibling(&mut self, tree: TreeRef) -> Result<()> {
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

    pub(super) fn add_child(&mut self, tree: TreeRef) -> Result<()> {
        let current = self.current_item(tree)?;
        let last = self.db.children(current.as_parent(), current.is_todo()).last().copied();
        self.collapsed.remove(&current);
        self.start_draft(tree, current.as_parent(), last)
    }

    /// Nest the item being edited under the one above it, or move it out of its parent, and
    /// keep editing it; a new item that is not saved yet only moves on screen
    pub(super) fn reindent_edit(&mut self, tree: TreeRef, indent: bool) -> Result<()> {
        let Some(item) = self.tree(tree).and_then(|t| t.editing()).map(|e| e.item) else { return Ok(()) };
        let Some(draft) = self.tree_mut(tree).draft.filter(|d| d.item == item) else {
            if indent {
                if let Some(parent) = self.indent_shown(item)? {
                    self.collapsed.remove(&parent);
                }
            } else {
                self.db.dedent(item)?;
            }
            self.refresh_all();
            return Ok(());
        };

        let moved = if indent {
            // the last child of the sibling above, skipping hidden ones
            let siblings = self.db.children(draft.parent, item.is_todo());
            let upto = draft.after.and_then(|a| siblings.iter().position(|s| *s == a)).map_or(0, |i| i + 1);
            let Some(above) = siblings[..upto].iter().rev().copied().find(|s| self.is_shown(*s)) else {
                return Ok(());
            };
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

    pub(super) fn remove(&mut self, tree: TreeRef, item: Item) {
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

    /// Whether an item is shown as far as completion goes: completed ones are hidden unless shown
    pub(super) fn is_shown(&self, item: Item) -> bool {
        self.show_completed || !self.is_completed(item)
    }

    /// The siblings an item is shown between, itself included
    pub(super) fn shown_siblings(&self, item: Item) -> Vec<Item> {
        self.db.siblings(item).into_iter().filter(|s| *s == item || self.is_shown(*s)).collect()
    }

    /// Swap an item with the shown sibling above or below it, passing hidden ones
    pub(super) fn shift_shown(&mut self, item: Item, up: bool) -> Result<()> {
        let shown = self.shown_siblings(item);
        let Some(at) = shown.iter().position(|s| *s == item) else { return Ok(()) };
        let Some(parent) = self.db.parent_of(item) else { return Ok(()) };
        if up {
            let Some(above) = at.checked_sub(1).map(|i| shown[i]) else { return Ok(()) };
            let all = self.db.siblings(item);
            let before = all.iter().position(|s| *s == above).and_then(|i| i.checked_sub(1)).map(|i| all[i]);
            self.db.move_to(item, parent, before)
        } else {
            let Some(below) = shown.get(at + 1).copied() else { return Ok(()) };
            self.db.move_to(item, parent, Some(below))
        }
    }

    /// Make an item the last child of the shown sibling above it, returning that sibling
    pub(super) fn indent_shown(&mut self, item: Item) -> Result<Option<Item>> {
        let shown = self.shown_siblings(item);
        let Some(above) = shown.iter().position(|s| *s == item).and_then(|i| i.checked_sub(1)).map(|i| shown[i]) else {
            return Ok(None);
        };
        let last = self.db.children(above.as_parent(), item.is_todo()).last().copied();
        self.db.move_to(item, above.as_parent(), last)?;
        Ok(Some(above))
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
    pub(super) fn completed_todos(&self, parent: Parent) -> Vec<Item> {
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

    pub(super) fn remove_completed(&mut self, ws: i64) {
        for item in self.completed_todos(Parent::Ws(ws)) {
            self.remove(TreeRef::Todos(ws), item);
        }
    }

    pub(super) fn toggle_expand_item(&mut self, item: Item) {
        if !self.collapsed.remove(&item) {
            self.collapsed.insert(item);
        }
        self.refresh_all();
    }

    pub(super) fn paste(&mut self, tree: TreeRef, below: bool) -> Result<()> {
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

    pub(super) fn copy_to_clipboard(text: &str) {
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
