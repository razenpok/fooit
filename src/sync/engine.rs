//! One round of syncing a log: pull what the server has, merge it with what changed here
//! since the last round, write the result, and push what is left of the local changes.
//! The state the server had at the last round (the shadow) is kept per log, so local
//! changes are found by comparing, whoever made them

use super::crypto::Secret;
use super::merge::{self, Batch, State};
use super::remote::{Push, Remote};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub type Result<T> = std::result::Result<T, String>;

/// Batches older than this are folded into the base. A machine that has not synced for
/// longer starts over from the base, keeping its own changes
pub const COMPACT_AFTER_MS: u64 = 30 * 24 * 3600 * 1000;

pub fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

/// The local end of a log
pub trait Side {
    fn secret(&self) -> &Secret;
    fn cursor(&self, conn: &Connection) -> Result<Option<u64>>;
    fn set_cursor(&self, conn: &Connection, cursor: u64) -> Result<()>;
    /// The rows that belong to the log, as records
    fn read(&self, conn: &Connection) -> Result<State>;
    /// Make the rows match `merged`, given that they were `local`
    fn write(&self, conn: &Connection, local: &State, merged: &State) -> Result<()>;
}

pub fn load_shadow(conn: &Connection, log_id: &str) -> Result<State> {
    let mut stmt = conn.prepare("SELECT uid, record FROM sync_shadow WHERE log_id = ?1").map_err(err)?;
    let rows = stmt
        .query_map([log_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(err)?;
    let mut state = State::new();
    for row in rows {
        let (uid, record) = row.map_err(err)?;
        state.insert(uid, serde_json::from_str(&record).map_err(|e| e.to_string())?);
    }
    Ok(state)
}

pub fn save_shadow(conn: &Connection, log_id: &str, state: &State) -> Result<()> {
    conn.execute("DELETE FROM sync_shadow WHERE log_id = ?1", [log_id]).map_err(err)?;
    let mut stmt = conn.prepare("INSERT INTO sync_shadow (log_id, uid, record) VALUES (?1, ?2, ?3)").map_err(err)?;
    for (uid, record) in state {
        stmt.execute(params![log_id, uid, serde_json::to_string(record).unwrap()]).map_err(err)?;
    }
    Ok(())
}

pub fn drop_shadow(conn: &Connection, log_id: &str) -> Result<()> {
    conn.execute("DELETE FROM sync_shadow WHERE log_id = ?1", [log_id]).map_err(err)?;
    Ok(())
}

pub fn seal_state(secret: &Secret, state: &State) -> Vec<u8> {
    secret.seal("base", &serde_json::to_vec(state).unwrap())
}

fn open_state(secret: &Secret, data: &[u8]) -> Result<State> {
    serde_json::from_slice(&secret.open("base", data)?).map_err(|e| e.to_string())
}

fn open_batch(secret: &Secret, data: &[u8]) -> Result<Batch> {
    serde_json::from_slice(&secret.open("batch", data)?).map_err(|e| e.to_string())
}

/// Start a log on the server with the side's rows as its base
pub fn create(conn: &Connection, remote: &dyn Remote, side: &dyn Side) -> Result<()> {
    let state = side.read(conn)?;
    remote.create(side.secret(), &seal_state(side.secret(), &state))?;
    save_shadow(conn, &side.secret().log_id(), &state)?;
    side.set_cursor(conn, 0)
}

#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// Rows here changed
    pub pulled: bool,
    /// Local changes went to the server
    pub pushed: bool,
}

pub fn sync(conn: &mut Connection, remote: &dyn Remote, side: &dyn Side) -> Result<Outcome> {
    let secret = side.secret().clone();
    let log_id = secret.log_id();
    let mut outcome = Outcome::default();

    for _ in 0..5 {
        let cursor = side.cursor(conn)?;
        let pull = remote.pull(&secret, cursor)?.ok_or("The sync server does not have this log")?;

        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(err)?;
        let old = load_shadow(&tx, &log_id)?;
        let mut remote_state = match &pull.base {
            Some(base) => open_state(&secret, base)?,
            None => old.clone(),
        };
        for batch in &pull.batches {
            merge::apply(&mut remote_state, &open_batch(&secret, &batch.data)?);
        }

        let local = side.read(&tx)?;
        let merged = merge::merge(&old, &remote_state, &local);
        if merged != local {
            side.write(&tx, &local, &merged)?;
            outcome.pulled |= merge::diff(&local, &merged).len() > 0;
        }
        if remote_state != old {
            save_shadow(&tx, &log_id, &remote_state)?;
        }
        if cursor != Some(pull.head) {
            side.set_cursor(&tx, pull.head)?;
        }
        tx.commit().map_err(err)?;

        let changes = merge::diff(&remote_state, &merged);
        if changes.is_empty() {
            compact(remote, &secret, pull.head, pull.oldest_age, &remote_state)?;
            return Ok(outcome);
        }
        let sealed = secret.seal("batch", &serde_json::to_vec(&changes).unwrap());
        match remote.push(&secret, pull.head, &sealed)? {
            Push::Done(head) => {
                let tx = conn.transaction().map_err(err)?;
                let mut confirmed = remote_state;
                merge::apply(&mut confirmed, &changes);
                save_shadow(&tx, &log_id, &confirmed)?;
                side.set_cursor(&tx, head)?;
                tx.commit().map_err(err)?;
                outcome.pushed = true;
                return Ok(outcome);
            }
            // someone else pushed first: take theirs in and try again
            Push::Behind => continue,
        }
    }
    Err("The sync server kept changing; try again later".into())
}

/// Fold old batches into the base, once this machine has everything up to `head`
fn compact(remote: &dyn Remote, secret: &Secret, head: u64, oldest_age: Option<u64>, state: &State) -> Result<()> {
    if oldest_age.is_some_and(|age| age > COMPACT_AFTER_MS) {
        remote.compact(secret, head, &seal_state(secret, state))?;
    }
    Ok(())
}

/// The cursor column of a row, for sides that keep it in a table
pub fn read_cursor(conn: &Connection, sql: &str, key: &dyn rusqlite::ToSql) -> Result<Option<u64>> {
    let value: Option<Option<i64>> = conn.query_row(sql, [key], |r| r.get(0)).optional().map_err(err)?;
    Ok(value.flatten().map(|v| v as u64))
}
