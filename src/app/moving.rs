//! Move mode: moving an item with the keyboard

use super::*;

impl App {
    pub(super) fn start_move(&mut self, tree: TreeRef) -> Result<()> {
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
    pub(super) fn moving_tree(&self, item: Item) -> TreeRef {
        match (item, self.current_ws()) {
            (Item::Todo(_), Some(ws)) => TreeRef::Todos(ws),
            _ => TreeRef::Workspaces,
        }
    }

    pub(super) fn move_keypress(&mut self, key: &str) -> Result<()> {
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
            // left out of the top level goes on to the workspaces, right comes back
            "left" | "h" if item.is_todo() && self.focus == Focus::Todos && self.db.same_kind_parent(item).is_none() => {
                self.focus = Focus::Workspaces;
            }
            "right" | "l" if item.is_todo() && self.focus == Focus::Workspaces => self.focus = Focus::Todos,
            "left" | "h" if item.is_todo() && self.focus == Focus::Workspaces => return Ok(()),
            "up" | "k" | "down" | "j" if item.is_todo() && self.focus == Focus::Workspaces => {
                let down = matches!(key, "down" | "j");
                // past the view rows to the next workspace
                let mut target = self.move_target(TreeRef::Workspaces, down);
                for _ in 0..2 {
                    match target {
                        Some(row) if is_view_row(row) => {
                            self.highlight_item(TreeRef::Workspaces, row);
                            target = self.move_target(TreeRef::Workspaces, down);
                        }
                        _ => break,
                    }
                }
                if let Some(Item::Ws(ws)) = target.filter(|t| !is_view_row(*t)) {
                    let last = self.db.children(Parent::Ws(ws), true).last().copied();
                    self.db.move_to(item, Parent::Ws(ws), last.filter(|l| *l != item))?;
                    self.highlight_item(TreeRef::Workspaces, Item::Ws(ws));
                }
            }
            "up" | "k" => {
                self.shift_shown(item, true)?;
            }
            "down" | "j" => {
                self.shift_shown(item, false)?;
            }
            "right" | "l" => {
                if let Some(parent) = self.indent_shown(item)? {
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
}
