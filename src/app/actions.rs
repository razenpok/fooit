//! The actions keys are bound to

use super::*;

impl App {
    /// Whether the cursor is on the first row of a workspace's todos
    pub(super) fn at_top_of_todos(&self, tree: TreeRef) -> bool {
        tree.is_todos()
            && !is_gathered(tree)
            && self.tree(tree).is_some_and(|t| t.highlighted.is_some() && t.highlighted == t.first_enabled())
    }

    /// The row moving the cursor up or down lands on, wrapping around the ends of the list
    pub(super) fn move_target(&mut self, tree: TreeRef, down: bool) -> Option<Item> {
        let state = self.tree_mut(tree);
        if state.rows.is_empty() {
            return None;
        }
        state.next_enabled(down).map(|i| state.rows[i].item)
    }

    /// The next or previous item on the highlighted item's level, wrapping around its siblings
    pub(super) fn sibling_target(&mut self, tree: TreeRef, down: bool) -> Option<Item> {
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

    pub(super) fn run(&mut self, action: Action) -> Result<()> {
        let tree = self.focused_tree();

        // the Today row is no workspace, and the Today list has no order or nesting of its own
        let changes_row = matches!(
            action,
            Action::AddSibling
                | Action::AddChild
                | Action::Indent
                | Action::Dedent
                | Action::ShiftUp
                | Action::ShiftDown
                | Action::StartMove
                | Action::PasteAbove
                | Action::PasteBelow
                | Action::StartSort
                | Action::RemoveCompleted
        );
        let on_today_row = self.tree(tree).and_then(|t| t.current()).is_some_and(is_view_row);
        if on_today_row
            && (changes_row
                || matches!(
                    action,
                    Action::RemoveNode
                        | Action::EditDescription
                        | Action::EditDue
                        | Action::EditRecurrence
                        | Action::CopyDescription
                        | Action::CopyModel
                ))
        {
            return Ok(());
        }
        // enter or right on a search result goes to the todo
        if tree == FOUND && matches!(action, Action::AddSibling | Action::IntoChildren) {
            let item = self.current_item(tree)?;
            self.jump_to(item);
            return Ok(());
        }
        if is_gathered(tree) && changes_row {
            self.notify("This lists todos from every workspace; add and arrange them there", Level::Info);
            return Ok(());
        }

        match action {
            Action::SwitchFocus => match self.focus {
                Focus::Workspaces => {
                    if let Some(ws) = self.current_ws() {
                        self.focus = Focus::Todos;
                        self.add_last_item(TreeRef::Todos(ws))?;
                        let state = self.tree_mut(TreeRef::Todos(ws));
                        if state.highlighted.is_none() {
                            state.highlighted = state.first_enabled();
                        }
                    }
                }
                Focus::Todos => {
                    if self.finding {
                        self.close_find();
                    }
                    self.focus = Focus::Workspaces;
                }
            },
            // up from the top of a workspace's todos goes round to adding one at the bottom
            Action::MoveUp | Action::PreviousSibling if self.at_top_of_todos(tree) => self.add_last_item(tree)?,
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
            // from the workspaces, right goes on to the todos like tab
            Action::IntoChildren if tree == TreeRef::Workspaces => return self.run(Action::SwitchFocus),
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
                match self.db.same_kind_parent(item) {
                    Some(parent) if !is_gathered(tree) => self.highlight_item(tree, parent),
                    // from the top of the todos, left goes back to the workspaces like tab
                    _ if tree.is_todos() => return self.run(Action::SwitchFocus),
                    _ => {}
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
                self.shift_shown(item, true)?;
                self.refresh_all();
            }
            Action::ShiftDown => {
                let item = self.current_item(tree)?;
                self.shift_shown(item, false)?;
                self.refresh_all();
            }
            Action::Indent => {
                let item = self.current_item(tree)?;
                if let Some(parent) = self.indent_shown(item)? {
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
                        message: "Are you sure? [Y/n]".into(),
                        confirmed: Some(("The items were deleted".into(), Level::Error)),
                        cancelled: Some(("The items were retained".into(), Level::Info)),
                        cancel_mode: Mode::Normal,
                        default_yes: true,
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
            Action::Snooze => {
                let Item::Todo(id) = self.current_item(tree)? else { return Ok(()) };
                let Some(mut todo) = self.db.todo(id).cloned() else { return Ok(()) };
                let due = snoozed(todo.due, self.count.unwrap_or(1) as i64);
                todo.due = Some(due);
                self.db.update_todo(todo)?;
                self.refresh_all();
                self.notify(format!("Snoozed to {}", crate::ui::due_text(due, dates::now())), Level::Info);
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
                        default_yes: false,
                    });
                } else {
                    self.remove_completed(ws);
                }
            }
            Action::StartMove => self.start_move(tree)?,
            Action::Undo => self.undo(false)?,
            Action::Redo => self.undo(true)?,
            Action::FindEverywhere => {
                self.finding = true;
                self.focus = Focus::Todos;
                self.tree_mut(FOUND);
                self.bar = Bar::Search { input: Input::new("/"), tree: FOUND };
                self.mode = Mode::Search;
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

/// A due date pushed some days later, keeping its time of day. One that has passed, or none,
/// is pushed from today, so a snoozed todo is never left overdue
fn snoozed(due: Option<chrono::NaiveDateTime>, days: i64) -> chrono::NaiveDateTime {
    let now = dates::now();
    let base = match due {
        Some(due) if due.date() >= now.date() => due,
        Some(due) => now.date().and_time(due.time()),
        None => now.date().and_hms_opt(0, 0, 0).unwrap(),
    };
    base + chrono::Duration::days(days)
}
