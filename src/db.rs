//! Storage layer: fooit's own SQLite database, with dooit's rules for ordering, completing
//! parents and children, and recurrence. Rows have a local integer id for the app and a
//! random uid that stays the same on every machine they are synced to

use crate::{dates, order};
use chrono::{NaiveDateTime, TimeDelta};
use rusqlite::{params, Connection, Transaction};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub type Result<T> = std::result::Result<T, String>;

/// Bumped when the schema changes, kept in `PRAGMA user_version`
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE workspace (
    id INTEGER PRIMARY KEY,
    uid TEXT NOT NULL UNIQUE,
    parent_id INTEGER,
    position TEXT NOT NULL,
    description TEXT NOT NULL,
    is_root INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE todo (
    id INTEGER PRIMARY KEY,
    uid TEXT NOT NULL UNIQUE,
    workspace_id INTEGER,
    parent_id INTEGER,
    position TEXT NOT NULL,
    description TEXT NOT NULL,
    due TEXT,
    recurrence INTEGER,
    urgency INTEGER NOT NULL,
    pending INTEGER NOT NULL
);
";

const DUE_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// A new random id for a row, the same on every machine the row is synced to
pub fn new_uid() -> String {
    let mut bytes = [0u8; 12];
    getrandom::getrandom(&mut bytes).expect("no random numbers");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn due_to_sql(due: NaiveDateTime) -> String {
    due.format(DUE_FORMAT).to_string()
}

pub fn due_from_sql(value: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(value, DUE_FORMAT).ok()
}

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
    pub uid: String,
    pub position: String,
    pub description: String,
    pub is_root: bool,
    pub parent_workspace_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    pub id: i64,
    pub uid: String,
    pub position: String,
    pub description: String,
    pub due: Option<NaiveDateTime>,
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
                SortField::Recurrence,
                SortField::Urgency,
                SortField::Pending,
            ],
        }
    }
}

/// Every row, to put back with `restore`
#[derive(Clone)]
pub struct Snapshot {
    pub workspaces: Vec<Workspace>,
    pub todos: Vec<Todo>,
}

pub struct Db {
    conn: Connection,
    path: PathBuf,
    last_modified: Option<SystemTime>,
    /// Bumped on every (re)load, so caches derived from the data know when they are stale
    pub generation: u64,

    pub workspaces: HashMap<i64, Workspace>,
    pub todos: HashMap<i64, Todo>,
    ws_children: HashMap<Option<i64>, Vec<i64>>,
    todo_children: HashMap<Parent, Vec<i64>>,
}

fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

fn opt_dt(value: Option<NaiveDateTime>) -> Option<String> {
    value.map(due_to_sql)
}

fn opt_td(value: Option<TimeDelta>) -> Option<i64> {
    value.map(|td| td.num_seconds())
}

/// Every workspace and todo in a database
pub fn read_rows(conn: &Connection) -> Result<(HashMap<i64, Workspace>, HashMap<i64, Todo>)> {
    let mut workspaces = HashMap::new();
    let mut stmt = conn
        .prepare("SELECT id, uid, position, description, is_root, parent_id FROM workspace")
        .map_err(err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Workspace {
                id: r.get(0)?,
                uid: r.get(1)?,
                position: r.get(2)?,
                description: r.get(3)?,
                is_root: r.get::<_, i64>(4)? != 0,
                parent_workspace_id: r.get(5)?,
            })
        })
        .map_err(err)?;
    for row in rows {
        let w = row.map_err(err)?;
        workspaces.insert(w.id, w);
    }

    let mut todos = HashMap::new();
    let mut stmt = conn
        .prepare(
            "SELECT id, uid, position, description, due, recurrence, urgency, pending, workspace_id, parent_id \
             FROM todo",
        )
        .map_err(err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Todo {
                id: r.get(0)?,
                uid: r.get(1)?,
                position: r.get(2)?,
                description: r.get(3)?,
                due: r.get::<_, Option<String>>(4)?.and_then(|s| due_from_sql(&s)),
                recurrence: r.get::<_, Option<i64>>(5)?.map(TimeDelta::seconds),
                urgency: r.get(6)?,
                pending: r.get::<_, i64>(7)? != 0,
                parent_workspace_id: r.get(8)?,
                parent_todo_id: r.get(9)?,
            })
        })
        .map_err(err)?;
    for row in rows {
        let t = row.map_err(err)?;
        todos.insert(t.id, t);
    }
    Ok((workspaces, todos))
}

/// Write a workspace, with its id when it has one
pub fn write_workspace(conn: &Connection, w: &Workspace) -> Result<i64> {
    conn.execute(
        "INSERT OR REPLACE INTO workspace (id, uid, parent_id, position, description, is_root) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![(w.id > 0).then_some(w.id), w.uid, w.parent_workspace_id, w.position, w.description, w.is_root as i64],
    )
    .map_err(err)?;
    Ok(conn.last_insert_rowid())
}

/// Write a todo, with its id when it has one
pub fn write_todo(conn: &Connection, t: &Todo) -> Result<i64> {
    if t.parent_workspace_id.is_none() && t.parent_todo_id.is_none() {
        return Err("Todo must have a parent workspace or todo".into());
    }
    if t.parent_workspace_id.is_some() && t.parent_todo_id.is_some() {
        return Err("Todo cannot have both a parent workspace and todo".into());
    }
    conn.execute(
        "INSERT OR REPLACE INTO todo (id, uid, workspace_id, parent_id, position, description, due, recurrence, \
         urgency, pending) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            (t.id > 0).then_some(t.id),
            t.uid,
            t.parent_workspace_id,
            t.parent_todo_id,
            t.position,
            t.description,
            opt_dt(t.due),
            opt_td(t.recurrence),
            t.urgency.clamp(1, 4),
            t.pending as i64,
        ],
    )
    .map_err(err)?;
    Ok(conn.last_insert_rowid())
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        let conn = Connection::open(path).map_err(err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(err)?;
        Self::create_schema(&conn)?;

        let mut db = Db {
            conn,
            path: path.to_path_buf(),
            last_modified: None,
            generation: 0,
            workspaces: HashMap::new(),
            todos: HashMap::new(),
            ws_children: HashMap::new(),
            todo_children: HashMap::new(),
        };
        db.load()?;
        db.last_modified = db.modified();
        Ok(db)
    }

    /// Create the tables in a new database, or refuse one that is not fooit's
    pub fn create_schema(conn: &Connection) -> Result<()> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(err)?;
        if version == SCHEMA_VERSION {
            return Ok(());
        }
        let tables: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table'", [], |r| r.get(0))
            .map_err(err)?;
        if version != 0 || tables != 0 {
            return Err(if version > SCHEMA_VERSION {
                "The database was made by a newer fooit".into()
            } else {
                "Not a fooit database (a dooit database is imported on the first start)".into()
            });
        }
        conn.execute_batch(&format!("BEGIN; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"))
            .map_err(err)
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
        let (workspaces, todos) = read_rows(&self.conn)?;

        let mut ws_children: HashMap<Option<i64>, Vec<i64>> = HashMap::new();
        for w in workspaces.values() {
            if !w.is_root {
                ws_children.entry(w.parent_workspace_id).or_default().push(w.id);
            }
        }
        for list in ws_children.values_mut() {
            list.sort_by(|a, b| {
                let (a, b) = (&workspaces[a], &workspaces[b]);
                (&a.position, &a.uid).cmp(&(&b.position, &b.uid))
            });
        }

        let mut todo_children: HashMap<Parent, Vec<i64>> = HashMap::new();
        for t in todos.values() {
            if let Some(p) = t.parent() {
                todo_children.entry(p).or_default().push(t.id);
            }
        }
        for list in todo_children.values_mut() {
            list.sort_by(|a, b| {
                let (a, b) = (&todos[a], &todos[b]);
                (&a.position, &a.uid).cmp(&(&b.position, &b.uid))
            });
        }

        self.workspaces = workspaces;
        self.todos = todos;
        self.ws_children = ws_children;
        self.todo_children = todo_children;
        self.generation += 1;
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

    fn position(&self, item: Item) -> &str {
        match item {
            Item::Ws(id) => self.workspaces.get(&id).map(|w| w.position.as_str()),
            Item::Todo(id) => self.todos.get(&id).map(|t| t.position.as_str()),
        }
        .unwrap_or("")
    }

    fn set_position(tx: &Transaction, item: Item, position: &str) -> Result<()> {
        match item {
            Item::Ws(id) => tx.execute("UPDATE workspace SET position = ?1 WHERE id = ?2", params![position, id]),
            Item::Todo(id) => tx.execute("UPDATE todo SET position = ?1 WHERE id = ?2", params![position, id]),
        }
        .map_err(err)?;
        Ok(())
    }

    /// Give every item in the list a new position, in list order
    fn set_order(tx: &Transaction, items: &[Item]) -> Result<()> {
        for (item, position) in items.iter().zip(order::spread(items.len())) {
            Self::set_position(tx, *item, &position)?;
        }
        Ok(())
    }

    /// A position for the item at `index` in `items`, between its neighbours there. The
    /// neighbours are renumbered when they leave no room, which only happens when two
    /// machines picked the same position
    fn place(&self, tx: &Transaction, items: &[Item], index: usize) -> Result<String> {
        let before = index.checked_sub(1).map(|i| self.position(items[i]));
        let after = items.get(index + 1).map(|i| self.position(*i));
        if let (Some(a), Some(b)) = (before, after) {
            if a >= b {
                Self::set_order(tx, items)?;
                let positions = order::spread(items.len());
                return Ok(positions[index].clone());
            }
        }
        let position = order::between(before, after);
        Self::set_position(tx, items[index], &position)?;
        Ok(position)
    }

    fn set_parent(tx: &Transaction, item: Item, parent: Parent) -> Result<()> {
        match (item, parent) {
            (Item::Ws(id), Parent::Ws(p)) => {
                tx.execute("UPDATE workspace SET parent_id = ?1 WHERE id = ?2", params![p, id])
            }
            (Item::Todo(id), Parent::Ws(p)) => tx.execute(
                "UPDATE todo SET workspace_id = ?1, parent_id = NULL WHERE id = ?2",
                params![p, id],
            ),
            (Item::Todo(id), Parent::Todo(p)) => tx.execute(
                "UPDATE todo SET workspace_id = NULL, parent_id = ?1 WHERE id = ?2",
                params![p, id],
            ),
            (Item::Ws(_), Parent::Todo(_)) => return Err("Cannot add a workspace to a todo".into()),
        }
        .map_err(err)?;
        Ok(())
    }

    fn new_todo(parent: Parent) -> Todo {
        let (ws, todo) = match parent {
            Parent::Ws(id) => (Some(id), None),
            Parent::Todo(id) => (None, Some(id)),
        };
        Todo {
            id: 0,
            uid: new_uid(),
            position: String::new(),
            description: String::new(),
            due: None,
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
        let tx = self.conn.unchecked_transaction().map_err(err)?;
        let id = write_workspace(
            &tx,
            &Workspace {
                id: 0,
                uid: new_uid(),
                position: String::new(),
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
    #[cfg(test)]
    pub fn insert(&mut self, parent: Parent, todo: bool, position: Option<usize>) -> Result<Item> {
        self.insert_with_description(parent, todo, position, "")
    }

    /// Insert a new item of the given kind under `parent`, at `position` among its
    /// siblings (or at the end)
    pub fn insert_with_description(
        &mut self,
        parent: Parent,
        todo: bool,
        position: Option<usize>,
        description: &str,
    ) -> Result<Item> {
        let mut siblings = self.children(parent, todo);
        let tx = self.conn.unchecked_transaction().map_err(err)?;

        let item = if todo {
            let todo = Todo { description: description.to_string(), ..Self::new_todo(parent) };
            Item::Todo(write_todo(&tx, &todo)?)
        } else {
            let Parent::Ws(pid) = parent else {
                return Err("Cannot add a workspace to a todo".into());
            };
            Item::Ws(write_workspace(
                &tx,
                &Workspace {
                    id: 0,
                    uid: new_uid(),
                    position: String::new(),
                    description: description.to_string(),
                    is_root: false,
                    parent_workspace_id: Some(pid),
                },
            )?)
        };

        let position = position.unwrap_or(siblings.len()).min(siblings.len());
        siblings.insert(position, item);
        self.place(&tx, &siblings, position)?;
        tx.commit().map_err(err)?;
        self.finish()?;
        Ok(item)
    }

    #[cfg(test)]
    pub fn add_sibling(&mut self, item: Item) -> Result<Item> {
        let parent = self
            .parent_of(item)
            .ok_or_else(|| "Cannot add a sibling to the root".to_string())?;
        let siblings = self.siblings(item);
        let index = siblings.iter().position(|s| *s == item).unwrap_or(siblings.len());
        self.insert(parent, item.is_todo(), Some(index + 1))
    }

    #[cfg(test)]
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

        let tx = self.conn.unchecked_transaction().map_err(err)?;
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
                let tx = self.conn.unchecked_transaction().map_err(err)?;
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

        let tx = self.conn.unchecked_transaction().map_err(err)?;

        // update_children_to_pending
        if new.pending && !any_child_pending {
            tx.execute("UPDATE todo SET pending = 1 WHERE parent_id = ?1", [new.id])
                .map_err(err)?;
        }

        // update_children_to_completed
        if !new.pending {
            tx.execute("UPDATE todo SET pending = 0 WHERE parent_id = ?1", [new.id])
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
            "UPDATE todo SET description = ?1, due = ?2, recurrence = ?3, urgency = ?4, pending = ?5 \
             WHERE id = ?6",
            params![
                new.description,
                opt_dt(new.due),
                opt_td(new.recurrence),
                new.urgency,
                new.pending as i64,
                new.id,
            ],
        )
        .map_err(err)?;
        { tx.commit().map_err(err)?; self.finish() }
    }

    #[cfg(test)]
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

        let tx = self.conn.unchecked_transaction().map_err(err)?;
        self.place(&tx, &siblings, other)?;
        tx.commit().map_err(err)?;
        self.finish()?;
        Ok(true)
    }

    #[cfg(test)]
    pub fn shift_up(&mut self, item: Item) -> Result<bool> {
        self.shift(item, true)
    }

    /// Make the item the last child of its previous sibling
    #[cfg(test)]
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

        let tx = self.conn.unchecked_transaction().map_err(err)?;
        Self::set_parent(&tx, item, new_parent.as_parent())?;
        self.place(&tx, &new_siblings, new_siblings.len() - 1)?;
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

        let mut new_siblings = self.siblings(parent);
        let parent_index = new_siblings.iter().position(|s| *s == parent).unwrap_or(0);
        new_siblings.insert(parent_index + 1, item);

        let tx = self.conn.unchecked_transaction().map_err(err)?;
        Self::set_parent(&tx, item, grandparent)?;
        self.place(&tx, &new_siblings, parent_index + 1)?;
        tx.commit().map_err(err)?;
        self.finish()?;
        Ok(true)
    }

    /// Add a todo from text that may end in a due date and hold an urgency, see `dates::quick_add`,
    /// and name a workspace to go to the bottom of with `#name` (or the start of the name)
    pub fn quick_add_todo(&mut self, parent: Parent, position: Option<usize>, text: &str) -> Result<Item> {
        let (text, workspace) = self.workspace_tag(text);
        let (parent, position) = match workspace {
            Some(ws) => (Parent::Ws(ws), None),
            None => (parent, position),
        };
        let read = dates::quick_add(&text);
        let item = self.insert_with_description(parent, true, position, &read.description)?;
        if read.due.is_some() || read.urgency.is_some() || read.recurrence.is_some() {
            let Item::Todo(id) = item else { return Ok(item) };
            if let Some(mut todo) = self.todo(id).cloned() {
                todo.due = read.due.or(todo.due);
                todo.urgency = read.urgency.unwrap_or(todo.urgency);
                todo.recurrence = read.recurrence.or(todo.recurrence);
                self.update_todo(todo)?;
            }
        }
        Ok(item)
    }

    /// The last `#word` in a text that names a workspace, and the text without it. A name
    /// that is typed out in full wins over one it is only the start of
    pub fn workspace_tag(&self, text: &str) -> (String, Option<i64>) {
        let names: Vec<(i64, String)> = self
            .workspaces
            .values()
            .filter(|w| !w.is_root)
            .map(|w| (w.id, w.description.to_lowercase().replace(' ', "")))
            .collect();
        let mut words: Vec<&str> = text.split_whitespace().collect();
        for i in (0..words.len()).rev() {
            let Some(tag) = words[i].strip_prefix('#').filter(|t| !t.is_empty()).map(str::to_lowercase) else {
                continue;
            };
            let exact = names.iter().find(|(_, name)| *name == tag);
            let mut prefixed: Vec<_> = names.iter().filter(|(_, name)| name.starts_with(&tag)).collect();
            prefixed.sort();
            if let Some((id, _)) = exact.or(prefixed.first().copied()) {
                words.remove(i);
                return (words.join(" "), Some(*id));
            }
        }
        (text.to_string(), None)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            workspaces: self.workspaces.values().cloned().collect(),
            todos: self.todos.values().cloned().collect(),
        }
    }

    /// Replace every row with the ones in a snapshot, ids included
    pub fn restore(&mut self, snapshot: &Snapshot) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(err)?;
        tx.execute("DELETE FROM todo", []).map_err(err)?;
        tx.execute("DELETE FROM workspace", []).map_err(err)?;
        for w in &snapshot.workspaces {
            write_workspace(&tx, w)?;
        }
        for t in &snapshot.todos {
            write_todo(&tx, t)?;
        }
        tx.commit().map_err(err)?;
        self.finish()
    }

    /// Move the item, with everything beneath it, under a parent: right after `after`, or first
    pub fn move_to(&mut self, item: Item, parent: Parent, after: Option<Item>) -> Result<()> {
        if self.parent_of(item).is_none() {
            return Ok(());
        }
        let mut siblings = self.children(parent, item.is_todo());
        siblings.retain(|s| *s != item);
        let index = after.and_then(|a| siblings.iter().position(|s| *s == a)).map_or(0, |i| i + 1);
        siblings.insert(index, item);

        let tx = self.conn.unchecked_transaction().map_err(err)?;
        Self::set_parent(&tx, item, parent)?;
        self.place(&tx, &siblings, index)?;
        tx.commit().map_err(err)?;
        self.finish()
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
                            SortField::Recurrence => nulls_last(&ta.recurrence, &tb.recurrence),
                            SortField::Urgency => ta.urgency.cmp(&tb.urgency),
                            SortField::Pending => {
                                let key = |t: &Todo| {
                                    (!t.pending, t.due.unwrap_or(NaiveDateTime::MAX), t.position.clone(), t.uid.clone())
                                };
                                return key(ta).cmp(&key(tb));
                            }
                            SortField::Reverse => unreachable!(),
                        };
                        ord.then(ta.position.cmp(&tb.position)).then(ta.uid.cmp(&tb.uid))
                    });
                }
            },
        }

        let tx = self.conn.unchecked_transaction().map_err(err)?;
        Self::set_order(&tx, &siblings)?;
        { tx.commit().map_err(err)?; self.finish() }
    }

    fn clone_todo_rec(&self, tx: &Transaction, source: i64, parent: Parent, position: &str) -> Result<i64> {
        let mut todo = self.todos.get(&source).cloned().ok_or("No model in clipboard")?;
        let (ws, parent_todo) = match parent {
            Parent::Ws(id) => (Some(id), None),
            Parent::Todo(id) => (None, Some(id)),
        };
        todo.id = 0;
        todo.uid = new_uid();
        todo.parent_workspace_id = ws;
        todo.parent_todo_id = parent_todo;
        todo.position = position.to_string();
        let new_id = write_todo(tx, &todo)?;

        for child in self.children(Parent::Todo(source), true) {
            if let Item::Todo(cid) = child {
                self.clone_todo_rec(tx, cid, Parent::Todo(new_id), &self.todos[&cid].position)?;
            }
        }
        Ok(new_id)
    }

    fn clone_ws_rec(&self, tx: &Transaction, source: i64, parent: i64, position: &str) -> Result<i64> {
        let ws = self.workspaces.get(&source).ok_or("No model in clipboard")?;
        let new_id = write_workspace(
            tx,
            &Workspace {
                id: 0,
                uid: new_uid(),
                position: position.to_string(),
                description: ws.description.clone(),
                is_root: false,
                parent_workspace_id: Some(parent),
            },
        )?;

        for child in self.children(Parent::Ws(source), false) {
            if let Item::Ws(cid) = child {
                self.clone_ws_rec(tx, cid, new_id, &self.workspaces[&cid].position)?;
            }
        }
        for child in self.children(Parent::Ws(source), true) {
            if let Item::Todo(cid) = child {
                self.clone_todo_rec(tx, cid, Parent::Ws(new_id), &self.todos[&cid].position)?;
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
            (Item::Todo(id), _) => Item::Todo(self.clone_todo_rec(&tx, id, parent, "")?),
            (Item::Ws(id), Parent::Ws(pid)) => Item::Ws(self.clone_ws_rec(&tx, id, pid, "")?),
            (Item::Ws(_), Parent::Todo(_)) => return Err("Cannot add a workspace to a todo".into()),
        };

        let position = position.unwrap_or(siblings.len()).min(siblings.len());
        siblings.insert(position, new_item);
        self.place(&tx, &siblings, position)?;
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
