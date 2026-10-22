//! A one-time import of dooit's database, which uses the schema SQLAlchemy made for it

use crate::db::{self, Todo, Workspace};
use crate::order;
use chrono::{NaiveDate, NaiveDateTime, TimeDelta};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::Path;

fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

/// SQLAlchemy stores `DateTime` on SQLite as `YYYY-MM-DD HH:MM:SS.ffffff`
fn datetime_from_sql(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim();
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(value, fmt) {
            return Some(dt);
        }
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0))
}

/// and `Interval` as a datetime relative to the unix epoch
fn interval_from_sql(value: &str) -> Option<TimeDelta> {
    datetime_from_sql(value).map(|dt| dt - chrono::DateTime::UNIX_EPOCH.naive_utc())
}

/// Write a new fooit database at `to` with everything in the dooit database at `from`, which
/// is left as it was. Ids stay the same, and each list of siblings keeps its order
pub fn from_dooit(from: &Path, to: &Path) -> Result<(), String> {
    let old = Connection::open_with_flags(from, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(err)?;

    let mut workspaces: Vec<(i64, Workspace)> = Vec::new();
    {
        let mut stmt = old
            .prepare("SELECT id, order_index, description, is_root, parent_workspace_id FROM workspace")
            .map_err(err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, Option<i64>>(1)?.unwrap_or(-1),
                    Workspace {
                        id: r.get(0)?,
                        uid: db::new_uid(),
                        position: String::new(),
                        description: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        is_root: r.get::<_, Option<i64>>(3)?.unwrap_or(0) != 0,
                        parent_workspace_id: r.get(4)?,
                    },
                ))
            })
            .map_err(err)?;
        for row in rows {
            workspaces.push(row.map_err(err)?);
        }
    }

    let mut todos: Vec<(i64, Todo)> = Vec::new();
    {
        let mut stmt = old
            .prepare(
                "SELECT id, order_index, description, due, recurrence, urgency, pending, \
                 parent_workspace_id, parent_todo_id FROM todo",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, Option<i64>>(1)?.unwrap_or(-1),
                    Todo {
                        id: r.get(0)?,
                        uid: db::new_uid(),
                        position: String::new(),
                        description: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        due: r.get::<_, Option<String>>(3)?.and_then(|s| datetime_from_sql(&s)),
                        recurrence: r.get::<_, Option<String>>(4)?.and_then(|s| interval_from_sql(&s)),
                        urgency: r.get::<_, Option<i64>>(5)?.unwrap_or(1).clamp(1, 4),
                        pending: r.get::<_, Option<i64>>(6)?.unwrap_or(1) != 0,
                        parent_workspace_id: r.get(7)?,
                        parent_todo_id: r.get(8)?,
                    },
                ))
            })
            .map_err(err)?;
        for row in rows {
            let (index, todo) = row.map_err(err)?;
            // a todo with both or neither parent is one dooit could not show either
            if todo.parent_workspace_id.is_some() != todo.parent_todo_id.is_some() {
                todos.push((index, todo));
            }
        }
    }

    // positions from dooit's order, per list of siblings
    let mut ws_lists: HashMap<Option<i64>, Vec<usize>> = HashMap::new();
    for (i, (_, w)) in workspaces.iter().enumerate() {
        ws_lists.entry(w.parent_workspace_id).or_default().push(i);
    }
    for list in ws_lists.values_mut() {
        list.sort_by_key(|i| (workspaces[*i].0, workspaces[*i].1.id));
        for (i, position) in list.iter().zip(order::spread(list.len())) {
            workspaces[*i].1.position = position;
        }
    }
    let mut todo_lists: HashMap<(Option<i64>, Option<i64>), Vec<usize>> = HashMap::new();
    for (i, (_, t)) in todos.iter().enumerate() {
        todo_lists.entry((t.parent_workspace_id, t.parent_todo_id)).or_default().push(i);
    }
    for list in todo_lists.values_mut() {
        list.sort_by_key(|i| (todos[*i].0, todos[*i].1.id));
        for (i, position) in list.iter().zip(order::spread(list.len())) {
            todos[*i].1.position = position;
        }
    }

    // written next to the destination first, so a failed import leaves nothing behind
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let partial = to.with_extension("importing");
    let _ = std::fs::remove_file(&partial);
    {
        let mut new = Connection::open(&partial).map_err(err)?;
        db::Db::create_schema(&new)?;
        let tx = new.transaction().map_err(err)?;
        for (_, w) in &workspaces {
            db::write_workspace(&tx, w)?;
        }
        for (_, t) in &todos {
            db::write_todo(&tx, t)?;
        }
        tx.commit().map_err(err)?;
    }
    std::fs::rename(&partial, to).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Item, Parent};

    #[test]
    fn imports_a_dooit_database() {
        let dir = std::env::temp_dir().join(format!("fooit-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (from, to) = (dir.join("dooit.db"), dir.join("fooit.db"));

        let old = Connection::open(&from).unwrap();
        old.execute_batch(
            "CREATE TABLE workspace (id INTEGER NOT NULL, order_index INTEGER NOT NULL, description VARCHAR NOT NULL,
                is_root BOOLEAN NOT NULL, parent_workspace_id INTEGER, PRIMARY KEY (id));
             CREATE TABLE todo (id INTEGER NOT NULL, order_index INTEGER NOT NULL, description VARCHAR NOT NULL,
                due DATETIME, effort INTEGER NOT NULL, recurrence DATETIME, urgency INTEGER NOT NULL,
                pending BOOLEAN NOT NULL, parent_workspace_id INTEGER, parent_todo_id INTEGER, PRIMARY KEY (id));
             INSERT INTO workspace VALUES (1, -1, '', 1, NULL), (2, 1, 'Home', 0, 1), (3, 0, 'Work', 0, 1);
             INSERT INTO todo VALUES
                (1, 1, 'second', '2024-05-06 07:08:00.000000', 0, '1970-01-02 00:00:00.000000', 3, 1, 3, NULL),
                (2, 0, 'first', NULL, 0, NULL, 1, 0, 3, NULL),
                (3, 0, 'child', NULL, 0, NULL, 9, 1, NULL, 1);",
        )
        .unwrap();
        drop(old);

        from_dooit(&from, &to).unwrap();
        let db = db::Db::open(&to).unwrap();
        let root = db.root_id().unwrap();
        let names = |items: Vec<Item>| items.iter().map(|i| db.description(*i).to_string()).collect::<Vec<_>>();
        assert_eq!(names(db.children(Parent::Ws(root), false)), ["Work", "Home"]);
        assert_eq!(names(db.children(Parent::Ws(3), true)), ["first", "second"]);
        let second = db.todo(1).unwrap();
        assert_eq!(second.due, datetime_from_sql("2024-05-06 07:08"));
        assert_eq!(second.recurrence, Some(chrono::TimeDelta::days(1)));
        assert_eq!((second.urgency, second.pending), (3, true));
        assert!(!db.todo(2).unwrap().pending);
        assert_eq!(db.todo(3).unwrap().urgency, 4);
        assert_eq!(names(db.children(Parent::Todo(1), true)), ["child"]);

        // the dooit database is not one fooit opens itself
        assert!(db::Db::open(&from).is_err());
        assert_eq!(interval_from_sql("1970-01-01 03:00:00.000000"), Some(TimeDelta::hours(3)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
