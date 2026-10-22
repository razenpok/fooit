//! Editing fields and adding new items

use super::*;

impl App {
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

    pub(super) fn start_edit(&mut self, tree: TreeRef, field: Field) -> Result<()> {
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

    pub(super) fn save_edit(&mut self, edit: &Edit) -> Result<()> {
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
    pub(super) fn stop_edit(&mut self, tree: TreeRef, discard: bool) -> bool {
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

    pub(super) fn discard_edit(&mut self, tree: TreeRef) {
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
            default_yes: false,
        });
    }

    /// Add a new item to the database once it has a description, otherwise drop it and
    /// highlight the row above, as also when it went to another workspace; returns whether it
    /// was added
    pub(super) fn finish_draft(&mut self, tree: TreeRef, draft: Draft, edit: &Edit, discard: bool) -> bool {
        let description = edit.input.value().trim().to_string();
        let mut added_elsewhere = false;
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
                    let workspace = self.workspace_of(item);
                    if !tree.is_todos() || workspace == self.current_ws().map(Item::Ws) {
                        self.refresh_all();
                        self.highlight_item(tree, item);
                        return true;
                    }
                    // a #workspace sent it elsewhere, so the cursor goes back like for a dropped one
                    if let Some(ws) = workspace {
                        self.notify(format!("Added to {}", self.db.description(ws)), Level::Info);
                    }
                    added_elsewhere = true;
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
        added_elsewhere
    }

    pub(super) fn set_filter(&mut self, tree: TreeRef, filter: String) {
        self.tree_mut(tree).filter = filter;
        self.rebuild(tree);
        if tree == TreeRef::Workspaces {
            self.refresh_all();
        }
    }
}
