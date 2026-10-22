//! A synced workspace as a log: the workspace itself, and everything beneath it except
//! workspaces that are synced on their own

use super::crypto::Secret;
use super::engine::{self, Result, Side, err};
use super::merge::{AT, Record, State};
use crate::db::{self, Todo, Workspace};
use crate::order;
use chrono::TimeDelta;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub struct WorkspaceLog {
    pub ws_uid: String,
    pub secret: Secret,
}

/// Every workspace uid that is synced, with its secret and server
pub fn links(conn: &Connection) -> Result<Vec<(String, Secret, String)>> {
    let mut stmt = conn.prepare("SELECT ws_uid, secret, endpoint FROM sync_link ORDER BY ws_uid").map_err(err)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .map_err(err)?;
    let mut out = Vec::new();
    for row in rows {
        let (uid, secret, endpoint) = row.map_err(err)?;
        out.push((uid, Secret::decode(&secret)?, endpoint));
    }
    Ok(out)
}

pub fn link(conn: &Connection, ws_uid: &str) -> Result<Option<(Secret, String)>> {
    let row: Option<(String, String)> = conn
        .query_row("SELECT secret, endpoint FROM sync_link WHERE ws_uid = ?1", [ws_uid], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(err)?;
    row.map(|(s, e)| Ok((Secret::decode(&s)?, e))).transpose()
}

/// Stop syncing a workspace. Its todos stay
pub fn unlink(conn: &Connection, ws_uid: &str) -> Result<()> {
    if let Some((secret, _)) = link(conn, ws_uid)? {
        engine::drop_shadow(conn, &secret.log_id())?;
    }
    conn.execute("DELETE FROM sync_link WHERE ws_uid = ?1", [ws_uid]).map_err(err)?;
    Ok(())
}

struct Rows {
    workspaces: HashMap<i64, Workspace>,
    todos: HashMap<i64, Todo>,
    ws_children: HashMap<i64, Vec<i64>>,
    todos_of_ws: HashMap<i64, Vec<i64>>,
    todos_of_todo: HashMap<i64, Vec<i64>>,
}

impl Rows {
    fn read(conn: &Connection) -> Result<Rows> {
        let (workspaces, todos) = db::read_rows(conn)?;
        let mut rows = Rows {
            workspaces,
            todos,
            ws_children: HashMap::new(),
            todos_of_ws: HashMap::new(),
            todos_of_todo: HashMap::new(),
        };
        for w in rows.workspaces.values() {
            if let Some(p) = w.parent_workspace_id {
                rows.ws_children.entry(p).or_default().push(w.id);
            }
        }
        for t in rows.todos.values() {
            match (t.parent_workspace_id, t.parent_todo_id) {
                (Some(w), _) => rows.todos_of_ws.entry(w).or_default().push(t.id),
                (_, Some(p)) => rows.todos_of_todo.entry(p).or_default().push(t.id),
                _ => {}
            }
        }
        Ok(rows)
    }

    fn ws_by_uid(&self, uid: &str) -> Option<&Workspace> {
        self.workspaces.values().find(|w| w.uid == uid)
    }
}

fn ws_record(w: &Workspace, parent_uid: Option<&str>) -> Record {
    let mut r = json!({"k": "w", "d": w.description}).as_object().unwrap().clone();
    if let Some(p) = parent_uid {
        r.insert(AT.into(), json!([p, w.position]));
    }
    r
}

fn todo_record(t: &Todo, parent_uid: &str) -> Record {
    json!({
        "k": "t",
        "d": t.description,
        "due": t.due.map(db::due_to_sql),
        "rec": t.recurrence.map(|r| r.num_seconds()),
        "u": t.urgency,
        "p": t.pending,
        AT: [parent_uid, t.position],
    })
    .as_object()
    .unwrap()
    .clone()
}

impl WorkspaceLog {
    fn linked(conn: &Connection) -> Result<HashSet<String>> {
        Ok(links(conn)?.into_iter().map(|(uid, _, _)| uid).collect())
    }

    fn state(&self, rows: &Rows, linked: &HashSet<String>) -> State {
        let mut state = State::new();
        let Some(root) = rows.ws_by_uid(&self.ws_uid) else { return state };
        state.insert(root.uid.clone(), ws_record(root, None));

        fn todos(rows: &Rows, ids: Option<&Vec<i64>>, parent_uid: &str, state: &mut State) {
            for id in ids.into_iter().flatten() {
                let t = &rows.todos[id];
                state.insert(t.uid.clone(), todo_record(t, parent_uid));
                todos(rows, rows.todos_of_todo.get(id), &t.uid, state);
            }
        }
        let mut stack = vec![root.id];
        while let Some(ws) = stack.pop() {
            let uid = &rows.workspaces[&ws].uid;
            todos(rows, rows.todos_of_ws.get(&ws), uid, &mut state);
            for child in rows.ws_children.get(&ws).into_iter().flatten() {
                let c = &rows.workspaces[child];
                if !linked.contains(&c.uid) {
                    state.insert(c.uid.clone(), ws_record(c, Some(uid)));
                    stack.push(*child);
                }
            }
        }
        state
    }
}

fn at(record: &Record) -> Option<(&str, &str)> {
    let at = record.get(AT)?.as_array()?;
    Some((at.first()?.as_str()?, at.get(1)?.as_str()?))
}

fn text(record: &Record, field: &str) -> String {
    record.get(field).and_then(Value::as_str).unwrap_or_default().to_string()
}

impl Side for WorkspaceLog {
    fn secret(&self) -> &Secret {
        &self.secret
    }

    fn cursor(&self, conn: &Connection) -> Result<Option<u64>> {
        engine::read_cursor(conn, "SELECT cursor FROM sync_link WHERE ws_uid = ?1", &self.ws_uid)
    }

    fn set_cursor(&self, conn: &Connection, cursor: u64) -> Result<()> {
        conn.execute("UPDATE sync_link SET cursor = ?1 WHERE ws_uid = ?2", params![cursor as i64, self.ws_uid])
            .map_err(err)?;
        Ok(())
    }

    fn read(&self, conn: &Connection) -> Result<State> {
        Ok(self.state(&Rows::read(conn)?, &Self::linked(conn)?))
    }

    fn write(&self, conn: &Connection, local: &State, merged: &State) -> Result<()> {
        let rows = Rows::read(conn)?;
        let ids: HashMap<String, (bool, i64)> = rows
            .workspaces
            .values()
            .map(|w| (w.uid.clone(), (false, w.id)))
            .chain(rows.todos.values().map(|t| (t.uid.clone(), (true, t.id))))
            .collect();

        // removed everywhere: so it goes here too, and is no longer synced
        let Some(root_record) = merged.get(&self.ws_uid) else {
            for uid in local.keys() {
                delete(conn, ids.get(uid))?;
            }
            conn.execute("DELETE FROM share_member WHERE ws_uid = ?1", [&self.ws_uid]).map_err(err)?;
            return unlink(conn, &self.ws_uid);
        };

        // a workspace new to this machine goes at the bottom of the top level
        let mut ids = ids;
        match rows.ws_by_uid(&self.ws_uid) {
            Some(root) => {
                if local.get(&self.ws_uid) != Some(root_record) {
                    conn.execute(
                        "UPDATE workspace SET description = ?1 WHERE id = ?2",
                        params![text(root_record, "d"), root.id],
                    )
                    .map_err(err)?;
                }
            }
            None => {
                let top = match rows.workspaces.values().find(|w| w.is_root) {
                    Some(w) => w.id,
                    None => db::write_workspace(
                        conn,
                        &Workspace {
                            id: 0,
                            uid: db::new_uid(),
                            position: String::new(),
                            description: String::new(),
                            is_root: true,
                            parent_workspace_id: None,
                        },
                    )?,
                };
                let last = rows
                    .ws_children
                    .get(&top)
                    .into_iter()
                    .flatten()
                    .map(|id| rows.workspaces[id].position.as_str())
                    .max();
                let id = db::write_workspace(
                    conn,
                    &Workspace {
                        id: 0,
                        uid: self.ws_uid.clone(),
                        position: order::between(last, None),
                        description: text(root_record, "d"),
                        is_root: false,
                        parent_workspace_id: Some(top),
                    },
                )?;
                ids.insert(self.ws_uid.clone(), (false, id));
            }
        }

        // parents before children
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for (uid, record) in merged {
            if let Some((parent, _)) = at(record) {
                children.entry(parent).or_default().push(uid);
            }
        }
        let mut queue: Vec<&str> = vec![self.ws_uid.as_str()];
        while let Some(parent) = queue.pop() {
            for uid in children.get(parent).cloned().unwrap_or_default() {
                queue.push(uid);
                let record = &merged[uid];
                if local.get(uid) == Some(record) {
                    continue;
                }
                let (parent_uid, position) = at(record).unwrap();
                let Some(&(parent_is_todo, parent_id)) = ids.get(parent_uid) else { continue };
                let existing = ids.get(uid).map(|(_, id)| *id).unwrap_or(0);
                let id = if text(record, "k") == "w" {
                    if parent_is_todo {
                        continue;
                    }
                    db::write_workspace(
                        conn,
                        &Workspace {
                            id: existing,
                            uid: uid.to_string(),
                            position: position.to_string(),
                            description: text(record, "d"),
                            is_root: false,
                            parent_workspace_id: Some(parent_id),
                        },
                    )?
                } else {
                    db::write_todo(
                        conn,
                        &Todo {
                            id: existing,
                            uid: uid.to_string(),
                            position: position.to_string(),
                            description: text(record, "d"),
                            due: record.get("due").and_then(Value::as_str).and_then(db::due_from_sql),
                            recurrence: record.get("rec").and_then(Value::as_i64).map(TimeDelta::seconds),
                            urgency: record.get("u").and_then(Value::as_i64).unwrap_or(1),
                            pending: record.get("p").and_then(Value::as_bool).unwrap_or(true),
                            parent_workspace_id: (!parent_is_todo).then_some(parent_id),
                            parent_todo_id: parent_is_todo.then_some(parent_id),
                        },
                    )?
                };
                ids.insert(uid.to_string(), (text(record, "k") != "w", id));
            }
        }

        for uid in local.keys() {
            if !merged.contains_key(uid) {
                delete(conn, ids.get(uid))?;
            }
        }
        // a workspace synced on its own whose parent went keeps its place at the top level
        conn.execute(
            "UPDATE workspace SET parent_id = (SELECT id FROM workspace WHERE is_root = 1 ORDER BY id LIMIT 1) \
             WHERE is_root = 0 AND parent_id NOT IN (SELECT id FROM workspace)",
            [],
        )
        .map_err(err)?;
        Ok(())
    }
}

fn delete(conn: &Connection, row: Option<&(bool, i64)>) -> Result<()> {
    match row {
        Some((true, id)) => conn.execute("DELETE FROM todo WHERE id = ?1", [id]),
        Some((false, id)) => conn.execute("DELETE FROM workspace WHERE id = ?1", [id]),
        None => return Ok(()),
    }
    .map_err(err)?;
    Ok(())
}
