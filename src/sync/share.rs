//! Shares: a secret that opens a list of workspaces. The list is a log too, mapping each
//! workspace's uid to that workspace's own secret, so importing one share brings in all of
//! its workspaces, and workspaces added to it later

use super::crypto::Secret;
use super::engine::{self, Result, Side, err};
use super::merge::State;
use super::workspace::{self, WorkspaceLog};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Share {
    pub id: i64,
    pub name: String,
    pub secret: Secret,
    pub endpoint: String,
}

pub fn shares(conn: &Connection) -> Result<Vec<Share>> {
    let mut stmt = conn.prepare("SELECT id, name, secret, endpoint FROM share ORDER BY name, id").map_err(err)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
        .map_err(err)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, secret, endpoint) = row.map_err(err)?;
        out.push(Share { id, name, secret: Secret::decode(&secret)?, endpoint });
    }
    Ok(out)
}

/// The share with this name, or the only one whose name starts with it
pub fn find(conn: &Connection, name: &str) -> Result<Share> {
    let all = shares(conn)?;
    let wanted = name.to_lowercase();
    if let Some(s) = all.iter().find(|s| s.name.to_lowercase() == wanted) {
        return Ok(s.clone());
    }
    let mut prefixed = all.iter().filter(|s| s.name.to_lowercase().starts_with(&wanted));
    match (prefixed.next(), prefixed.next()) {
        (Some(s), None) => Ok(s.clone()),
        (Some(_), Some(_)) => Err(format!("More than one share starts with \"{name}\"")),
        _ => Err(format!("No share called \"{name}\"")),
    }
}

/// The workspace uids in a share
pub fn members(conn: &Connection, share_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT ws_uid FROM share_member WHERE share_id = ?1 ORDER BY ws_uid").map_err(err)?;
    let rows = stmt.query_map([share_id], |r| r.get::<_, String>(0)).map_err(err)?;
    rows.map(|r| r.map_err(err)).collect()
}

fn in_any_share(conn: &Connection, ws_uid: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM share_member WHERE ws_uid = ?1 LIMIT 1", [ws_uid], |r| r.get(0))
        .optional()
        .map_err(err)?;
    Ok(found.is_some())
}

/// Stop syncing a workspace once no share holds it any more
pub fn unlink_if_unshared(conn: &Connection, ws_uid: &str) -> Result<()> {
    if !in_any_share(conn, ws_uid)? {
        workspace::unlink(conn, ws_uid)?;
    }
    Ok(())
}

pub struct ShareLog {
    pub share: Share,
}

impl Side for ShareLog {
    fn secret(&self) -> &Secret {
        &self.share.secret
    }

    fn cursor(&self, conn: &Connection) -> Result<Option<u64>> {
        engine::read_cursor(conn, "SELECT cursor FROM share WHERE id = ?1", &self.share.id)
    }

    fn set_cursor(&self, conn: &Connection, cursor: u64) -> Result<()> {
        conn.execute("UPDATE share SET cursor = ?1 WHERE id = ?2", params![cursor as i64, self.share.id])
            .map_err(err)?;
        Ok(())
    }

    fn read(&self, conn: &Connection) -> Result<State> {
        let mut stmt = conn.prepare("SELECT ws_uid, secret FROM share_member WHERE share_id = ?1").map_err(err)?;
        let rows = stmt
            .query_map([self.share.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(err)?;
        let mut state = State::new();
        for row in rows {
            let (uid, secret) = row.map_err(err)?;
            state.insert(uid, json!({"k": "m", "s": secret}).as_object().unwrap().clone());
        }
        Ok(state)
    }

    fn write(&self, conn: &Connection, local: &State, merged: &State) -> Result<()> {
        conn.execute("DELETE FROM share_member WHERE share_id = ?1", [self.share.id]).map_err(err)?;
        for (uid, record) in merged {
            let Some(secret) = record.get("s").and_then(Value::as_str) else { continue };
            conn.execute(
                "INSERT INTO share_member (share_id, ws_uid, secret) VALUES (?1, ?2, ?3)",
                params![self.share.id, uid, secret],
            )
            .map_err(err)?;
            let secret = Secret::decode(secret)?;
            match workspace::link(conn, uid)? {
                Some((current, _)) if current == secret => {}
                // a new secret: the workspace moved to a new log, so start over on that one
                Some(_) => {
                    workspace::unlink(conn, uid)?;
                    link_new(conn, uid, &secret, &self.share.endpoint)?;
                }
                None => link_new(conn, uid, &secret, &self.share.endpoint)?,
            }
        }
        for uid in local.keys() {
            if !merged.contains_key(uid) {
                unlink_if_unshared(conn, uid)?;
            }
        }
        Ok(())
    }
}

fn link_new(conn: &Connection, ws_uid: &str, secret: &Secret, endpoint: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_link (ws_uid, secret, endpoint, cursor) VALUES (?1, ?2, ?3, NULL)",
        params![ws_uid, secret.encode(), endpoint],
    )
    .map_err(err)?;
    Ok(())
}

/// Start syncing a workspace on a new log, unless it is synced already
pub fn ensure_linked(conn: &Connection, remote: &dyn super::remote::Remote, ws_uid: &str, endpoint: &str) -> Result<Secret> {
    if let Some((secret, _)) = workspace::link(conn, ws_uid)? {
        return Ok(secret);
    }
    let secret = Secret::new();
    link_new(conn, ws_uid, &secret, endpoint)?;
    let side = WorkspaceLog { ws_uid: ws_uid.to_string(), secret: secret.clone() };
    if let Err(e) = engine::create(conn, remote, &side) {
        workspace::unlink(conn, ws_uid)?;
        return Err(e);
    }
    Ok(secret)
}

/// A new share holding these workspaces
pub fn create(conn: &Connection, remote: &dyn super::remote::Remote, name: &str, endpoint: &str, ws_uids: &[String]) -> Result<Share> {
    if shares(conn)?.iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
        return Err(format!("There is a share called \"{name}\" already"));
    }
    let secret = Secret::new();
    conn.execute(
        "INSERT INTO share (name, secret, endpoint, cursor) VALUES (?1, ?2, ?3, NULL)",
        params![name, secret.encode(), endpoint],
    )
    .map_err(err)?;
    let share = Share { id: conn.last_insert_rowid(), name: name.to_string(), secret, endpoint: endpoint.to_string() };
    let result = (|| {
        for uid in ws_uids {
            add_member(conn, remote, &share, uid)?;
        }
        engine::create(conn, remote, &ShareLog { share: share.clone() })
    })();
    if let Err(e) = result {
        forget(conn, &share)?;
        return Err(e);
    }
    Ok(share)
}

/// Put a workspace in a share locally; the next sync of the share sends it
pub fn add_member(conn: &Connection, remote: &dyn super::remote::Remote, share: &Share, ws_uid: &str) -> Result<()> {
    let secret = ensure_linked(conn, remote, ws_uid, &share.endpoint)?;
    conn.execute(
        "INSERT OR REPLACE INTO share_member (share_id, ws_uid, secret) VALUES (?1, ?2, ?3)",
        params![share.id, ws_uid, secret.encode()],
    )
    .map_err(err)?;
    Ok(())
}

/// Take a workspace out of a share. Whoever had the share keeps what they have, but the
/// workspace moves to a new secret, given to the shares that still hold it, so they get
/// nothing new
pub fn remove_member(conn: &Connection, remote: &dyn super::remote::Remote, share: &Share, ws_uid: &str) -> Result<()> {
    conn.execute("DELETE FROM share_member WHERE share_id = ?1 AND ws_uid = ?2", params![share.id, ws_uid])
        .map_err(err)?;
    if !in_any_share(conn, ws_uid)? {
        return workspace::unlink(conn, ws_uid);
    }
    let Some((_, endpoint)) = workspace::link(conn, ws_uid)? else { return Ok(()) };
    workspace::unlink(conn, ws_uid)?;
    let secret = ensure_linked(conn, remote, ws_uid, &endpoint)?;
    conn.execute("UPDATE share_member SET secret = ?1 WHERE ws_uid = ?2", params![secret.encode(), ws_uid])
        .map_err(err)?;
    Ok(())
}

/// Add a share from its link. Its workspaces arrive with the next sync
pub fn import(conn: &Connection, link: &str, name: Option<&str>) -> Result<Share> {
    let (endpoint, secret) = super::crypto::parse_link(link)?;
    if let Some(s) = shares(conn)?.into_iter().find(|s| s.secret == secret) {
        return Err(format!("That is the share \"{}\", which is here already", s.name));
    }
    let mut name = name.map(str::to_string).unwrap_or_else(|| "shared".into());
    let taken: Vec<String> = shares(conn)?.into_iter().map(|s| s.name.to_lowercase()).collect();
    let base = name.clone();
    let mut n = 2;
    while taken.contains(&name.to_lowercase()) {
        name = format!("{base}-{n}");
        n += 1;
    }
    conn.execute(
        "INSERT INTO share (name, secret, endpoint, cursor) VALUES (?1, ?2, ?3, NULL)",
        params![name, secret.encode(), endpoint],
    )
    .map_err(err)?;
    Ok(Share { id: conn.last_insert_rowid(), name, secret, endpoint })
}

/// Forget a share here. Its workspaces that no other share holds stop syncing, and keep
/// their todos
pub fn forget(conn: &Connection, share: &Share) -> Result<()> {
    let uids = members(conn, share.id)?;
    conn.execute("DELETE FROM share_member WHERE share_id = ?1", [share.id]).map_err(err)?;
    conn.execute("DELETE FROM share WHERE id = ?1", [share.id]).map_err(err)?;
    engine::drop_shadow(conn, &share.secret.log_id())?;
    for uid in uids {
        unlink_if_unshared(conn, &uid)?;
    }
    Ok(())
}

/// Leave a share's link on the server under a new pairing code, for another machine to
/// take with `import_code` within ten minutes
pub fn pair(remote: &dyn super::remote::Remote, share: &Share) -> Result<String> {
    let code = super::crypto::new_code();
    let payload = json!({"link": super::crypto::link(&share.endpoint, &share.secret), "name": share.name});
    let key = Secret::from_code(&code)?;
    remote.pair_put(&key, &key.seal("pair", &serde_json::to_vec(&payload).unwrap()))?;
    Ok(code)
}

/// The link and name of the share a pairing code stands for. A code works once
pub fn take_code(remote: &dyn super::remote::Remote, code: &str) -> Result<(String, String)> {
    let key = Secret::from_code(code)?;
    let sealed = remote.pair_take(&key)?.ok_or("No such code; it may have been used, or be over ten minutes old")?;
    let payload: Value = serde_json::from_slice(&key.open("pair", &sealed)?).map_err(|e| e.to_string())?;
    let field = |name: &str| payload[name].as_str().map(str::to_string).ok_or("Damaged pairing data".to_string());
    Ok((field("link")?, field("name")?))
}

pub fn rename(conn: &Connection, share: &Share, name: &str) -> Result<()> {
    conn.execute("UPDATE share SET name = ?1 WHERE id = ?2", params![name, share.id]).map_err(err)?;
    Ok(())
}
