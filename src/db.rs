//! Storage layer. Uses the exact schema SQLAlchemy creates for dooit and
//! reimplements the ORM hooks (ordering, completion propagation, recurrence).

use crate::dates;
use chrono::{NaiveDateTime, TimeDelta};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub type Result<T> = std::result::Result<T, String>;

const WORKSPACE_DDL: &str = "CREATE TABLE workspace (
\tid INTEGER NOT NULL,
\torder_index INTEGER NOT NULL,
\tdescription VARCHAR NOT NULL,
\tis_root BOOLEAN NOT NULL,
\tparent_workspace_id INTEGER,
\tPRIMARY KEY (id),
\tFOREIGN KEY(parent_workspace_id) REFERENCES workspace (id)
)";

const TODO_DDL: &str = "CREATE TABLE todo (
\tid INTEGER NOT NULL,
\torder_index INTEGER NOT NULL,
\tdescription VARCHAR NOT NULL,
\tdue DATETIME,
\teffort INTEGER NOT NULL,
\trecurrence DATETIME,
\turgency INTEGER NOT NULL,
\tpending BOOLEAN NOT NULL,
\tparent_workspace_id INTEGER,
\tparent_todo_id INTEGER,
\tPRIMARY KEY (id),
\tFOREIGN KEY(parent_workspace_id) REFERENCES workspace (id),
\tFOREIGN KEY(parent_todo_id) REFERENCES todo (id)
)";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Item {
    Ws(i64),
    Todo(i64),
}

impl Item {
    pub fn is_todo(&self) -> bool {
        matches!(self, Item::Todo(_))
    }

    pub fn as_parent(&self) -> Parent {
        match *self {
            Item::Ws(id) => Parent::Ws(id),
            Item::Todo(id) => Parent::Todo(id),
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Item::Ws(_) => "Workspace",
            Item::Todo(_) => "Todo",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Parent {
    Ws(i64),
    Todo(i64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: i64,
    pub order_index: i64,
    pub description: String,
    pub is_root: bool,
    pub parent_workspace_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    pub id: i64,
    pub order_index: i64,
    pub description: String,
    pub due: Option<NaiveDateTime>,
    pub effort: i64,
    pub recurrence: Option<TimeDelta>,
    pub urgency: i64,
    pub pending: bool,
    pub parent_workspace_id: Option<i64>,
    pub parent_todo_id: Option<i64>,
}

impl Todo {
    pub fn parent(&self) -> Option<Parent> {
        if let Some(id) = self.parent_workspace_id {
            return Some(Parent::Ws(id));
        }
        self.parent_todo_id.map(Parent::Todo)
    }

    pub fn is_overdue(&self) -> bool {
        match self.due {
            Some(due) => self.pending && due < dates::now(),
            None => false,
        }
    }

    pub fn status(&self) -> &'static str {
        if !self.pending {
            "completed"
        } else if self.is_overdue() {
            "overdue"
        } else {
            "pending"
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Reverse,
    Description,
    Due,
    Effort,
    Recurrence,
    Urgency,
    Pending,
}

impl SortField {
    pub fn name(&self) -> &'static str {
        match self {
            SortField::Reverse => "reverse",
            SortField::Description => "description",
            SortField::Due => "due",
            SortField::Effort => "effort",
            SortField::Recurrence => "recurrence",
            SortField::Urgency => "urgency",
            SortField::Pending => "pending",
        }
    }

    /// `["reverse"] + model.comparable_fields()`
    pub fn options_for(item: Item) -> Vec<SortField> {
        match item {
            Item::Ws(_) => vec![SortField::Reverse, SortField::Description],
            Item::Todo(_) => vec![
                SortField::Reverse,
                SortField::Description,
                SortField::Due,
                SortField::Effort,
                SortField::Recurrence,
                SortField::Urgency,
                SortField::Pending,
            ],
        }
    }
}

pub struct Db {
    conn: Connection,
    path: PathBuf,
    last_modified: Option<SystemTime>,

    pub workspaces: HashMap<i64, Workspace>,
    pub todos: HashMap<i64, Todo>,
    ws_children: HashMap<Option<i64>, Vec<i64>>,
    todo_children: HashMap<Parent, Vec<i64>>,
}

fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

fn opt_dt(value: Option<NaiveDateTime>) -> Option<String> {
    value.map(dates::datetime_to_sql)
}

fn opt_td(value: Option<TimeDelta>) -> Option<String> {
    value.map(dates::interval_to_sql)
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        let conn = Connection::open(path).map_err(err)?;
        // python's sqlite3 (and so dooit) runs without foreign key enforcement
        conn.execute_batch("PRAGMA foreign_keys = OFF").map_err(err)?;
        for (table, ddl) in [("workspace", WORKSPACE_DDL), ("todo", TODO_DDL)] {
            let exists: Option<String> = conn
                .query_row(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .optional()
                .map_err(err)?;
            if exists.is_none() {
                conn.execute(ddl, []).map_err(err)?;
            }
        }

        let mut db = Db {
            conn,
            path: path.to_path_buf(),
            last_modified: None,
            workspaces: HashMap::new(),
            todos: HashMap::new(),
            ws_children: HashMap::new(),
            todo_children: HashMap::new(),
        };
        db.load()?;
        db.last_modified = db.modified();
        Ok(db)
    }

    fn modified(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.path).and_then(|m| m.modified()).ok()
    }

    /// Returns true (and reloads) when the database file was changed by someone else
    pub fn has_changed(&mut self) -> bool {
        let current = self.modified();
        if current.is_some() && current != self.last_modified {
            self.last_modified = current;
            let _ = self.load();
            return true;
        }
        false
    }

    pub fn load(&mut self) -> Result<()> {
        let mut workspaces = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT id, order_index, description, is_root, parent_workspace_id FROM workspace")
                .map_err(err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(Workspace {
                        id: r.get(0)?,
                        order_index: r.get::<_, Option<i64>>(1)?.unwrap_or(-1),
                        description: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        is_root: r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                        parent_workspace_id: r.get(4)?,
                    })
                })
                .map_err(err)?;
            for row in rows {
                let w = row.map_err(err)?;
                workspaces.insert(w.id, w);
            }
        }

        let mut todos = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT id, order_index, description, due, effort, recurrence, urgency, pending, \
                     parent_workspace_id, parent_todo_id FROM todo",
                )
                .map_err(err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(Todo {
                        id: r.get(0)?,
                        order_index: r.get::<_, Option<i64>>(1)?.unwrap_or(-1),
                        description: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        due: r
                            .get::<_, Option<String>>(3)?
                            .and_then(|s| dates::datetime_from_sql(&s)),
                        effort: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        recurrence: r
                            .get::<_, Option<String>>(5)?
                            .and_then(|s| dates::interval_from_sql(&s)),
                        urgency: r.get::<_, Option<i64>>(6)?.unwrap_or(1),
                        pending: r.get::<_, Option<i64>>(7)?.unwrap_or(1) != 0,
                        parent_workspace_id: r.get(8)?,
                        parent_todo_id: r.get(9)?,
                    })
                })
                .map_err(err)?;
            for row in rows {
                let t = row.map_err(err)?;
                todos.insert(t.id, t);
            }
        }

        let mut ws_children: HashMap<Option<i64>, Vec<i64>> = HashMap::new();
        for w in workspaces.values() {
            if !w.is_root {
                ws_children.entry(w.parent_workspace_id).or_default().push(w.id);
            }
        }
        for list in ws_children.values_mut() {
            list.sort_by_key(|id| (workspaces[id].order_index, *id));
        }

        let mut todo_children: HashMap<Parent, Vec<i64>> = HashMap::new();
        for t in todos.values() {
            if let Some(p) = t.parent() {
                todo_children.entry(p).or_default().push(t.id);
            }
        }
        for list in todo_children.values_mut() {
            list.sort_by_key(|id| (todos[id].order_index, *id));
        }

        self.workspaces = workspaces;
        self.todos = todos;
        self.ws_children = ws_children;
        self.todo_children = todo_children;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.load()?;
        self.last_modified = self.modified();
        Ok(())
    }

    // ------------------------------------------------------------------
    // Queries

    pub fn root_id(&self) -> Option<i64> {
        let mut roots: Vec<i64> = self
            .workspaces
            .values()
            .filter(|w| w.is_root)
            .map(|w| w.id)
            .collect();
        roots.sort();
        roots.first().copied()
    }

    pub fn exists(&self, item: Item) -> bool {
        match item {
            Item::Ws(id) => self.workspaces.contains_key(&id),
            Item::Todo(id) => self.todos.contains_key(&id),
        }
    }

    pub fn description(&self, item: Item) -> &str {
        match item {
            Item::Ws(id) => self.workspaces.get(&id).map(|w| w.description.as_str()),
            Item::Todo(id) => self.todos.get(&id).map(|t| t.description.as_str()),
        }
        .unwrap_or("")
    }

    pub fn todo(&self, id: i64) -> Option<&Todo> {
        self.todos.get(&id)
    }

    /// Children of the same kind as the tree they are displayed in
    pub fn children(&self, parent: Parent, todos: bool) -> Vec<Item> {
        if todos {
            self.todo_children
                .get(&parent)
                .map(|v| v.iter().map(|id| Item::Todo(*id)).collect())
                .unwrap_or_default()
        } else {
            match parent {
                Parent::Ws(id) => self
                    .ws_children
                    .get(&Some(id))
                    .map(|v| v.iter().map(|id| Item::Ws(*id)).collect())
                    .unwrap_or_default(),
                Parent::Todo(_) => vec![],
            }
        }
    }

    pub fn parent_of(&self, item: Item) -> Option<Parent> {
        match item {
            Item::Ws(id) => self
                .workspaces
                .get(&id)
                .and_then(|w| w.parent_workspace_id)
                .map(Parent::Ws),
            Item::Todo(id) => self.todos.get(&id).and_then(|t| t.parent()),
        }
    }

    /// Parent of the same kind (excluding the root workspace), used for nesting
    pub fn same_kind_parent(&self, item: Item) -> Option<Item> {
        match self.parent_of(item)? {
            Parent::Ws(id) if !item.is_todo() => {
                let w = self.workspaces.get(&id)?;
                if w.is_root {
                    None
                } else {
                    Some(Item::Ws(id))
                }
            }
            Parent::Todo(id) if item.is_todo() => Some(Item::Todo(id)),
            _ => None,
        }
    }

    pub fn siblings(&self, item: Item) -> Vec<Item> {
        match self.parent_of(item) {
            Some(parent) => self.children(parent, item.is_todo()),
            None => vec![],
        }
    }

    // ------------------------------------------------------------------
    // Helpers inside transactions

    fn set_order(tx: &Transaction, items: &[Item]) -> Result<()> {
        for (index, item) in items.iter().enumerate() {
            match item {
                Item::Ws(id) => tx
                    .execute("UPDATE workspace SET order_index = ?1 WHERE id = ?2", params![index as i64, id]),
                Item::Todo(id) => tx
                    .execute("UPDATE todo SET order_index = ?1 WHERE id = ?2", params![index as i64, id]),
            }
            .map_err(err)?;
        }
        Ok(())
    }

    fn set_parent(tx: &Transaction, item: Item, parent: Parent) -> Result<()> {
        match (item, parent) {
            (Item::Ws(id), Parent::Ws(p)) => tx
                .execute("UPDATE workspace SET parent_workspace_id = ?1 WHERE id = ?2", params![p, id]),
            (Item::Todo(id), Parent::Ws(p)) => tx.execute(
                "UPDATE todo SET parent_workspace_id = ?1, parent_todo_id = NULL WHERE id = ?2",
                params![p, id],
            ),
            (Item::Todo(id), Parent::Todo(p)) => tx.execute(
                "UPDATE todo SET parent_workspace_id = NULL, parent_todo_id = ?1 WHERE id = ?2",
                params![p, id],
            ),
            (Item::Ws(_), Parent::Todo(_)) => return Err("Cannot add a workspace to a todo".into()),
        }
        .map_err(err)?;
        Ok(())
    }

    fn insert_workspace_row(tx: &Transaction, w: &Workspace) -> Result<i64> {
        tx.execute(
            "INSERT INTO workspace (order_index, description, is_root, parent_workspace_id) VALUES (?1, ?2, ?3, ?4)",
            params![w.order_index, w.description, w.is_root as i64, w.parent_workspace_id],
        )
        .map_err(err)?;
        Ok(tx.last_insert_rowid())
    }

    fn insert_todo_row(tx: &Transaction, t: &Todo) -> Result<i64> {
        if t.parent_workspace_id.is_none() && t.parent_todo_id.is_none() {
            return Err("Todo must have a parent workspace or todo".into());
        }
        if t.parent_workspace_id.is_some() && t.parent_todo_id.is_some() {
            return Err("Todo cannot have both a parent workspace and todo".into());
        }
        tx.execute(
            "INSERT INTO todo (order_index, description, due, effort, recurrence, urgency, pending, \
             parent_workspace_id, parent_todo_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                t.order_index,
                t.description,
                opt_dt(t.due),
                t.effort,
                opt_td(t.recurrence),
                t.urgency.clamp(1, 4),
                t.pending as i64,
                t.parent_workspace_id,
                t.parent_todo_id,
            ],
        )
        .map_err(err)?;
        Ok(tx.last_insert_rowid())
    }

    fn new_todo(parent: Parent) -> Todo {
        let (ws, todo) = match parent {
            Parent::Ws(id) => (Some(id), None),
            Parent::Todo(id) => (None, Some(id)),
        };
        Todo {
            id: 0,
            order_index: -1,
            description: String::new(),
            due: None,
            effort: 0,
            recurrence: None,
            urgency: 1,
            pending: true,
            parent_workspace_id: ws,
            parent_todo_id: todo,
        }
    }

    // ------------------------------------------------------------------
    // Mutations

    pub fn ensure_root(&mut self) -> Result<i64> {
        if let Some(id) = self.root_id() {
            return Ok(id);
        }
        let tx = self.conn.transaction().map_err(err)?;
        let id = Self::insert_workspace_row(
            &tx,
            &Workspace {
                id: 0,
                order_index: -1,
                description: String::new(),
                is_root: true,
                parent_workspace_id: None,
            },
        )?;
        tx.commit().map_err(err)?;
        self.finish()?;
        Ok(id)
    }

    /// Insert a new empty item of the given kind under `parent`, at `position`
    /// among its siblings (or at the end)
    pub fn insert(&mut self, parent: Parent, todo: bool, position: Option<usize>) -> Result<Item> {
        let mut siblings = self.children(parent, todo);
        let tx = self.conn.transaction().map_err(err)?;

        let item = if todo {
            Item::Todo(Self::insert_todo_row(&tx, &Self::new_todo(parent))?)
        } else {
            let Parent::Ws(pid) = parent else {
                return Err("Cannot add a workspace to a todo".into());
            };
            Item::Ws(Self::insert_workspace_row(
                &tx,
                &Workspace {
                    id: 0,
                    order_index: -1,
                    description: String::new(),
                    is_root: false,
                    parent_workspace_id: Some(pid),
                },
            )?)
        };

        let position = position.unwrap_or(siblings.len()).min(siblings.len());
        siblings.insert(position, item);
        Self::set_order(&tx, &siblings)?;
        tx.commit().map_err(err)?;
        self.finish()?;
        Ok(item)
    }

    pub fn add_sibling(&mut self, item: Item) -> Result<Item> {
        let parent = self
            .parent_of(item)
            .ok_or_else(|| "Cannot add a sibling to the root".to_string())?;
        let siblings = self.siblings(item);
        let index = siblings.iter().position(|s| *s == item).unwrap_or(siblings.len());
        self.insert(parent, item.is_todo(), Some(index + 1))
    }

    pub fn add_child(&mut self, item: Item) -> Result<Item> {
        self.insert(item.as_parent(), item.is_todo(), None)
    }

    fn collect_descendants(&self, item: Item, ws: &mut HashSet<i64>, todos: &mut HashSet<i64>) {
        match item {
            Item::Ws(id) => {
                ws.insert(id);
                for child in self.children(Parent::Ws(id), false) {
                    self.collect_descendants(child, ws, todos);
                }
                for child in self.children(Parent::Ws(id), true) {
                    self.collect_descendants(child, ws, todos);
                }
            }
            Item::Todo(id) => {
                todos.insert(id);
                for child in self.children(Parent::Todo(id), true) {
                    self.collect_descendants(child, ws, todos);
                }
            }
        }
    }

    /// Delete an item along with everything beneath it (ORM cascade)
    pub fn delete(&mut self, item: Item) -> Result<()> {
        let mut ws = HashSet::new();
        let mut todos = HashSet::new();
        self.collect_descendants(item, &mut ws, &mut todos);

        let tx = self.conn.transaction().map_err(err)?;
        for id in &todos {
            tx.execute("DELETE FROM todo WHERE id = ?1", [id]).map_err(err)?;
        }
        for id in &ws {
            tx.execute("DELETE FROM workspace WHERE id = ?1", [id]).map_err(err)?;
        }
        { tx.commit().map_err(err)?; self.finish() }
    }

    pub fn set_description(&mut self, item: Item, description: &str) -> Result<()> {
        match item {
            Item::Ws(id) => {
                let Some(w) = self.workspaces.get(&id) else { return Ok(()) };
                if w.description == description {
                    return Ok(());
                }
                let tx = self.conn.transaction().map_err(err)?;
                tx.execute(
                    "UPDATE workspace SET description = ?1 WHERE id = ?2",
                    params![description, id],
                )
                .map_err(err)?;
                { tx.commit().map_err(err)?; self.finish() }
            }
            Item::Todo(id) => {
                let Some(mut t) = self.todos.get(&id).cloned() else { return Ok(()) };
                t.description = description.to_string();
                self.update_todo(t)
            }
        }
    }

    /// Persist a modified todo, running the same hooks dooit runs before an update
    pub fn update_todo(&mut self, mut new: Todo) -> Result<()> {
        let Some(old) = self.todos.get(&new.id).cloned() else {
            return Ok(());
        };

        // validation hooks
        new.urgency = new.urgency.clamp(1, 4);
        if new == old {
            return Ok(());
        }

        let children: Vec<&Todo> = self
            .todo_children
            .get(&Parent::Todo(new.id))
            .map(|v| v.iter().map(|id| &self.todos[id]).collect())
            .unwrap_or_default();
        let any_child_pending = children.iter().any(|c| c.pending);

        let parent_all_completed = new.parent_todo_id.map(|pid| {
            self.todo_children
                .get(&Parent::Todo(pid))
                .map(|v| {
                    v.iter().all(|id| {
                        if *id == new.id {
                            !new.pending
                        } else {
                            !self.todos[id].pending
                        }
                    })
                })
                .unwrap_or(true)
        });

        let tx = self.conn.transaction().map_err(err)?;

        // update_children_to_pending
        if new.pending && !any_child_pending {
            tx.execute("UPDATE todo SET pending = 1 WHERE parent_todo_id = ?1", [new.id])
                .map_err(err)?;
        }

        // update_children_to_completed
        if !new.pending {
            tx.execute("UPDATE todo SET pending = 0 WHERE parent_todo_id = ?1", [new.id])
                .map_err(err)?;
        }

        if let Some(pid) = new.parent_todo_id {
            // update_parent_to_pending
            if new.pending {
                tx.execute("UPDATE todo SET pending = 1 WHERE id = ?1", [pid]).map_err(err)?;
            }
            // update_parent_to_completed
            if !new.pending && parent_all_completed == Some(true) {
                tx.execute("UPDATE todo SET pending = 0 WHERE id = ?1", [pid]).map_err(err)?;
            }
        }

        // update_due_for_recurrence
        if let Some(recurrence) = new.recurrence {
            if new.due.is_none() {
                new.due = Some(dates::now());
            }
            if !new.pending {
                new.pending = true;
                new.due = new.due.map(|d| d + recurrence);
            }
        }

        tx.execute(
            "UPDATE todo SET description = ?1, due = ?2, effort = ?3, recurrence = ?4, urgency = ?5, \
             pending = ?6 WHERE id = ?7",
            params![
                new.description,
                opt_dt(new.due),
                new.effort,
                opt_td(new.recurrence),
                new.urgency,
                new.pending as i64,
                new.id,
            ],
        )
        .map_err(err)?;
        { tx.commit().map_err(err)?; self.finish() }
    }

    fn shift(&mut self, item: Item, up: bool) -> Result<bool> {
        let mut siblings = self.siblings(item);
        let Some(index) = siblings.iter().position(|s| *s == item) else {
            return Ok(false);
        };
        let other = if up {
            if index == 0 {
                return Ok(false);
            }
            index - 1
        } else {
            if index + 1 >= siblings.len() {
                return Ok(false);
            }
            index + 1
        };
        siblings.swap(index, other);

        let tx = self.conn.transaction().map_err(err)?;
        Self::set_order(&tx, &siblings)?;
        tx.commit().map_err(err)?;
        self.finish()?;
        Ok(true)
    }

    pub fn shift_up(&mut self, item: Item) -> Result<bool> {
        self.shift(item, true)
    }

    pub fn shift_down(&mut self, item: Item) -> Result<bool> {
        self.shift(item, false)
    }

    /// Make the item the last child of its previous sibling
    pub fn indent(&mut self, item: Item) -> Result<Option<Item>> {
        let mut siblings = self.siblings(item);
        let Some(index) = siblings.iter().position(|s| *s == item) else {
            return Ok(None);
        };
        if index == 0 {
            return Ok(None);
        }

        let new_parent = siblings[index - 1];
        let mut new_siblings = self.children(new_parent.as_parent(), item.is_todo());
        siblings.remove(index);
        new_siblings.push(item);

        let tx = self.conn.transaction().map_err(err)?;
        Self::set_parent(&tx, item, new_parent.as_parent())?;
        Self::set_order(&tx, &siblings)?;
        Self::set_order(&tx, &new_siblings)?;
        tx.commit().map_err(err)?;
        self.finish()?;
        Ok(Some(new_parent))
    }

    /// Move the item out of its parent, placing it right after the parent
    pub fn dedent(&mut self, item: Item) -> Result<bool> {
        let Some(parent) = self.same_kind_parent(item) else {
            return Ok(false);
        };
        let Some(grandparent) = self.parent_of(parent) else {
            return Ok(false);
        };

        let mut siblings = self.siblings(item);
        siblings.retain(|s| *s != item);

        let mut new_siblings = self.siblings(parent);
        let parent_index = new_siblings.iter().position(|s| *s == parent).unwrap_or(0);
        new_siblings.insert(parent_index + 1, item);

        let tx = self.conn.transaction().map_err(err)?;
        Self::set_parent(&tx, item, grandparent)?;
        Self::set_order(&tx, &siblings)?;
        Self::set_order(&tx, &new_siblings)?;
        tx.commit().map_err(err)?;
        self.finish()?;
        Ok(true)
    }

    pub fn sort_siblings(&mut self, item: Item, field: SortField) -> Result<()> {
        let mut siblings = self.siblings(item);

        match field {
            SortField::Reverse => siblings.reverse(),
            _ => match item {
                Item::Ws(_) => {
                    siblings.sort_by(|a, b| {
                        let (Item::Ws(a), Item::Ws(b)) = (a, b) else { unreachable!() };
                        let (wa, wb) = (&self.workspaces[a], &self.workspaces[b]);
                        wa.description.cmp(&wb.description).then(a.cmp(b))
                    });
                }
                Item::Todo(_) => {
                    let todos = &self.todos;
                    let get = |i: &Item| match i {
                        Item::Todo(id) => &todos[id],
                        _ => unreachable!(),
                    };

                    // ascending with NULLs last, ties keep table order
                    fn nulls_last<T: Ord>(a: &Option<T>, b: &Option<T>) -> std::cmp::Ordering {
                        match (a, b) {
                            (Some(a), Some(b)) => a.cmp(b),
                            (Some(_), None) => std::cmp::Ordering::Less,
                            (None, Some(_)) => std::cmp::Ordering::Greater,
                            (None, None) => std::cmp::Ordering::Equal,
                        }
                    }

                    siblings.sort_by(|a, b| {
                        let (ta, tb) = (get(a), get(b));
                        let ord = match field {
                            SortField::Description => ta.description.cmp(&tb.description),
                            SortField::Due => nulls_last(&ta.due, &tb.due),
                            SortField::Effort => ta.effort.cmp(&tb.effort),
                            SortField::Recurrence => nulls_last(&ta.recurrence, &tb.recurrence),
                            SortField::Urgency => ta.urgency.cmp(&tb.urgency),
                            SortField::Pending => {
                                let key = |t: &Todo| {
                                    (!t.pending, t.due.unwrap_or(NaiveDateTime::MAX), t.order_index)
                                };
                                return key(ta).cmp(&key(tb));
                            }
                            SortField::Reverse => unreachable!(),
                        };
                        ord.then(ta.id.cmp(&tb.id))
                    });
                }
            },
        }

        let tx = self.conn.transaction().map_err(err)?;
        Self::set_order(&tx, &siblings)?;
        { tx.commit().map_err(err)?; self.finish() }
    }

    fn clone_todo_rec(&self, tx: &Transaction, source: i64, parent: Parent, order_index: i64) -> Result<i64> {
        let mut todo = self.todos.get(&source).cloned().ok_or("No model in clipboard")?;
        let (ws, parent_todo) = match parent {
            Parent::Ws(id) => (Some(id), None),
            Parent::Todo(id) => (None, Some(id)),
        };
        todo.parent_workspace_id = ws;
        todo.parent_todo_id = parent_todo;
        todo.order_index = order_index;
        let new_id = Self::insert_todo_row(tx, &todo)?;

        for child in self.children(Parent::Todo(source), true) {
            if let Item::Todo(cid) = child {
                let index = self.todos[&cid].order_index;
                self.clone_todo_rec(tx, cid, Parent::Todo(new_id), index)?;
            }
        }
        Ok(new_id)
    }

    fn clone_ws_rec(&self, tx: &Transaction, source: i64, parent: i64, order_index: i64) -> Result<i64> {
        let ws = self.workspaces.get(&source).ok_or("No model in clipboard")?;
        let new_id = Self::insert_workspace_row(
            tx,
            &Workspace {
                id: 0,
                order_index,
                description: ws.description.clone(),
                is_root: false,
                parent_workspace_id: Some(parent),
            },
        )?;

        for child in self.children(Parent::Ws(source), false) {
            if let Item::Ws(cid) = child {
                let index = self.workspaces[&cid].order_index;
                self.clone_ws_rec(tx, cid, new_id, index)?;
            }
        }
        for child in self.children(Parent::Ws(source), true) {
            if let Item::Todo(cid) = child {
                let index = self.todos[&cid].order_index;
                self.clone_todo_rec(tx, cid, Parent::Ws(new_id), index)?;
            }
        }
        Ok(new_id)
    }

    /// Deep copy `source` under `parent` at `position` among the new siblings
    pub fn clone_item(&mut self, source: Item, parent: Parent, position: Option<usize>) -> Result<Item> {
        if !self.exists(source) {
            return Err("No model in clipboard".into());
        }
        let mut siblings = self.children(parent, source.is_todo());
        let tx = self.conn.unchecked_transaction().map_err(err)?;

        let new_item = match (source, parent) {
            (Item::Todo(id), _) => Item::Todo(self.clone_todo_rec(&tx, id, parent, -1)?),
            (Item::Ws(id), Parent::Ws(pid)) => Item::Ws(self.clone_ws_rec(&tx, id, pid, -1)?),
            (Item::Ws(_), Parent::Todo(_)) => return Err("Cannot add a workspace to a todo".into()),
        };

        let position = position.unwrap_or(siblings.len()).min(siblings.len());
        siblings.insert(position, new_item);
        Self::set_order(&tx, &siblings)?;
        tx.commit().map_err(err)?;
        self.finish()?;
        Ok(new_item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> Db {
        let path = std::env::temp_dir().join(format!("fooit-test-{}-{}.db", name, std::process::id()));
        let _ = std::fs::remove_file(&path);
        Db::open(&path).unwrap()
    }

    #[test]
    fn ordering_and_nesting() {
        let mut db = temp_db("ordering");
        let root = db.ensure_root().unwrap();
        let a = db.insert(Parent::Ws(root), false, None).unwrap();
        let b = db.insert(Parent::Ws(root), false, None).unwrap();
        let c = db.add_sibling(a).unwrap();
        assert_eq!(db.children(Parent::Ws(root), false), vec![a, c, b]);

        db.shift_up(b).unwrap();
        assert_eq!(db.children(Parent::Ws(root), false), vec![a, b, c]);

        assert_eq!(db.indent(b).unwrap(), Some(a));
        assert_eq!(db.children(Parent::Ws(root), false), vec![a, c]);
        assert_eq!(db.children(a.as_parent(), false), vec![b]);

        assert!(db.dedent(b).unwrap());
        assert_eq!(db.children(Parent::Ws(root), false), vec![a, b, c]);
        assert!(!db.dedent(b).unwrap());

        let Item::Ws(aid) = a else { panic!() };
        let t1 = db.insert(Parent::Ws(aid), true, None).unwrap();
        let t2 = db.add_child(t1).unwrap();
        db.delete(a).unwrap();
        assert!(!db.exists(t1) && !db.exists(t2) && !db.exists(a));
    }

    #[test]
    fn completion_hooks() {
        let mut db = temp_db("hooks");
        let root = db.ensure_root().unwrap();
        let Item::Ws(w) = db.insert(Parent::Ws(root), false, None).unwrap() else { panic!() };
        let Item::Todo(parent) = db.insert(Parent::Ws(w), true, None).unwrap() else { panic!() };
        let Item::Todo(c1) = db.add_child(Item::Todo(parent)).unwrap() else { panic!() };
        let Item::Todo(c2) = db.add_child(Item::Todo(parent)).unwrap() else { panic!() };

        let mut t = db.todo(c1).unwrap().clone();
        t.pending = false;
        db.update_todo(t).unwrap();
        assert!(db.todo(parent).unwrap().pending);

        let mut t = db.todo(c2).unwrap().clone();
        t.pending = false;
        db.update_todo(t).unwrap();
        assert!(!db.todo(parent).unwrap().pending);

        let mut t = db.todo(parent).unwrap().clone();
        t.pending = true;
        db.update_todo(t).unwrap();
        assert!(db.todo(c1).unwrap().pending && db.todo(c2).unwrap().pending);

        // recurrence pushes the due date instead of completing
        let mut t = db.todo(c1).unwrap().clone();
        t.recurrence = Some(TimeDelta::days(1));
        t.due = Some(dates::now());
        db.update_todo(t).unwrap();
        let due = db.todo(c1).unwrap().due.unwrap();
        let mut t = db.todo(c1).unwrap().clone();
        t.pending = false;
        db.update_todo(t).unwrap();
        let t = db.todo(c1).unwrap();
        assert!(t.pending);
        assert_eq!(t.due.unwrap() - due, TimeDelta::days(1));

        // urgency is clamped
        let mut t = db.todo(c1).unwrap().clone();
        t.urgency = 9;
        db.update_todo(t).unwrap();
        assert_eq!(db.todo(c1).unwrap().urgency, 4);
    }

    #[test]
    fn cloning() {
        let mut db = temp_db("clone");
        let root = db.ensure_root().unwrap();
        let Item::Ws(w) = db.insert(Parent::Ws(root), false, None).unwrap() else { panic!() };
        let t = db.insert(Parent::Ws(w), true, None).unwrap();
        db.set_description(t, "hello").unwrap();
        let child = db.add_child(t).unwrap();
        db.set_description(child, "child").unwrap();

        let copy = db.clone_item(t, Parent::Ws(w), Some(0)).unwrap();
        assert_eq!(db.children(Parent::Ws(w), true), vec![copy, t]);
        assert_eq!(db.description(copy), "hello");
        let kids = db.children(copy.as_parent(), true);
        assert_eq!(kids.len(), 1);
        assert_eq!(db.description(kids[0]), "child");
    }
}

#[cfg(test)]
mod compat {
    use super::*;

    /// Run with COMPAT_DB pointing at a database written by python dooit
    #[test]
    #[ignore]
    fn read_and_modify_python_db() {
        let path = std::env::var("COMPAT_DB").unwrap();
        let mut db = Db::open(Path::new(&path)).unwrap();
        let root = db.root_id().unwrap();
        for w in db.children(Parent::Ws(root), false) {
            println!("ws {:?} {}", w, db.description(w));
            let Item::Ws(wid) = w else { continue };
            for t in db.children(Parent::Ws(wid), true) {
                let Item::Todo(id) = t else { continue };
                let todo = db.todo(id).unwrap().clone();
                println!("  todo {} due={:?} rec={:?} urg={} pending={}", todo.description, todo.due, todo.recurrence, todo.urgency, todo.pending);
                for c in db.children(t.as_parent(), true) {
                    println!("    child {}", db.description(c));
                }
            }
            // write a todo with a due date and recurrence for python to read back
            let new = db.insert(Parent::Ws(wid), true, None).unwrap();
            let Item::Todo(nid) = new else { unreachable!() };
            let mut t = db.todo(nid).unwrap().clone();
            t.description = "from fooit".into();
            t.due = crate::dates::parse_due("2031-02-03 04:05");
            t.recurrence = Some(TimeDelta::hours(36));
            t.urgency = 3;
            db.update_todo(t).unwrap();
            let child = db.add_child(new).unwrap();
            db.set_description(child, "fooit child").unwrap();
            break;
        }
    }
}
