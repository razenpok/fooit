//! Clicking, dragging and dropping rows

use super::*;

impl App {
    /// Whether the mouse is ours; while editing it is left to the terminal for selecting text
    pub fn wants_mouse(&self) -> bool {
        self.mode != Mode::Insert
    }

    pub(super) fn handle_mouse(&mut self, event: MouseEvent) {
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
    pub(super) fn row_at(&self, x: u16, y: u16) -> Option<(TreeRef, usize)> {
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

    pub(super) fn mouse_down(&mut self, x: u16, y: u16) -> Result<()> {
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

        // the Today row and the Today list cannot be rearranged
        if item == TODAY_ROW || tree == TODAY {
            return Ok(());
        }
        self.drag = Some(Drag {
            tree,
            item,
            start: (x, y),
            target: None,
            onto: None,
            moved: false,
            at: (x, y),
            scrolled: Instant::now(),
        });
        Ok(())
    }

    /// The column under a screen column, with its content's screen column and width
    pub(super) fn cell_at(&self, tree: TreeRef, depth: usize, x: u16) -> Option<(Field, u16, usize)> {
        let view = &self.tree(tree)?.view;
        row_cells(&view.columns, depth, view.area.width as usize)
            .into_iter()
            .map(|(field, start, width)| (field, view.area.x + start as u16, width))
            .find(|(_, start, width)| (*start..start + *width as u16 + 1).contains(&x))
    }

    /// Start editing the clicked column of a todo, with the cursor where the click was
    pub(super) fn click_edit(&mut self, tree: TreeRef, item: Item, (x, y): (u16, u16)) -> Result<()> {
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

    pub(super) fn mouse_drag(&mut self, x: u16, y: u16) {
        let Some(drag) = &self.drag else { return };
        if !drag.moved && drag.start == (x, y) {
            return;
        }
        let (target, onto) = if drag.tree.is_todos() && self.ws_tree.view.area.contains(Position::new(x, y)) {
            let current = self.current_ws().map(Item::Ws);
            let onto = self
                .row_at(x, y)
                .map(|(_, i)| self.ws_tree.rows[i].item)
                .filter(|ws| Some(*ws) != current && *ws != TODAY_ROW);
            (None, onto)
        } else {
            (self.drop_target(drag, x, y), None)
        };
        if let Some(drag) = self.drag.as_mut() {
            drag.target = target;
            drag.onto = onto;
            drag.moved = true;
            drag.at = (x, y);
        }
    }

    /// Scroll the list a dragged row is in while the mouse is at its top or bottom edge;
    /// returns whether it scrolled
    pub(super) fn scroll_drag(&mut self) -> bool {
        let Some(drag) = self.drag.as_ref().filter(|d| d.moved && d.onto.is_none()) else { return false };
        if drag.scrolled.elapsed() < Duration::from_millis(60) {
            return false;
        }
        let (tree, (x, y)) = (drag.tree, drag.at);
        let Some(state) = self.tree(tree) else { return false };
        let area = state.view.area;
        if !(area.x..area.right()).contains(&x) {
            return false;
        }
        let last = state.rows.len().saturating_sub(1);
        let offset = match () {
            _ if y <= area.y => state.offset.saturating_sub(1),
            _ if y + 1 >= area.bottom() => (state.offset + 1).min(last),
            _ => return false,
        };
        if offset == state.offset {
            return false;
        }
        self.tree_mut(tree).offset = offset;
        if let Some(drag) = self.drag.as_mut() {
            drag.scrolled = Instant::now();
        }
        true
    }

    pub(super) fn mouse_up(&mut self) -> Result<()> {
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

    pub(super) fn move_to_workspace(&mut self, item: Item, ws: Item) -> Result<()> {
        let last = self.db.children(ws.as_parent(), true).last().copied();
        self.db.move_to(item, ws.as_parent(), last)?;
        self.refresh_all();
        self.notify(format!("Moved to {}", self.db.description(ws)), Level::Info);
        Ok(())
    }

    /// The rows a dragged item takes along: itself and everything shown beneath it
    pub(super) fn dragged_rows(state: &TreeState, item: Item) -> Option<std::ops::Range<usize>> {
        let from = state.index_of(item)?;
        let depth = state.rows[from].depth;
        let len = state.rows[from + 1..].iter().take_while(|r| r.depth > depth).count();
        Some(from..from + 1 + len)
    }

    /// The gap the pointer is over, and the depth the horizontal movement asks for, limited to
    /// what fits between the rows around the gap
    pub(super) fn drop_target(&self, drag: &Drag, x: u16, y: u16) -> Option<DropTarget> {
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
        // nothing goes above or under the Today row
        let above = above.filter(|a| rows[*a].item != TODAY_ROW);

        let max = above.map_or(0, |a| rows[a].depth + 1);
        let min = below.map_or(0, |b| rows[b].depth).min(max);
        let wanted = indent(rows[dragged.start].depth) as i32 + x as i32 - drag.start.0 as i32;
        let depth = (min..=max).min_by_key(|d| (indent(*d) as i32 - wanted).abs()).unwrap_or(min);
        Some(DropTarget { above, depth })
    }

    pub(super) fn drop_item(&mut self, tree: TreeRef, item: Item, target: DropTarget) -> Result<()> {
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
}
