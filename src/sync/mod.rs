//! Syncing workspaces through a server (the Cloudflare Worker in `worker/`). Each synced
//! workspace is a log of encrypted batches of changes on top of a base; shares hand out the
//! secrets of a set of workspaces. See `engine` for a round of syncing

pub mod crypto;
pub mod engine;
pub mod merge;
pub mod remote;
pub mod share;
pub mod workspace;
#[cfg(test)]
mod tests;

use remote::{Http, Remote};
use rusqlite::Connection;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

pub type Result<T> = std::result::Result<T, String>;

/// Where the server is, and the token that lets this machine start new logs on it
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Config {
    pub url: Option<String>,
    pub token: Option<String>,
}

#[derive(Deserialize)]
struct ConfigFile {
    sync: Option<Config>,
}

impl Config {
    pub fn file() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("fooit").join("config.toml")
    }

    /// `[sync]` in the config file, under FOOIT_SYNC_URL and FOOIT_SYNC_TOKEN
    pub fn load() -> Result<Config> {
        let path = Self::file();
        let mut config = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str::<ConfigFile>(&text)
                .map_err(|e| format!("{}: {}", path.display(), e))?
                .sync
                .unwrap_or_default(),
            Err(_) => Config::default(),
        };
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        config.url = env("FOOIT_SYNC_URL").or(config.url).map(|u| u.trim().trim_end_matches('/').to_string());
        config.token = env("FOOIT_SYNC_TOKEN").or(config.token);
        Ok(config)
    }

    pub fn require_url(&self) -> Result<&str> {
        self.url.as_deref().ok_or_else(|| {
            format!("Set the sync server with FOOIT_SYNC_URL, or `url` under [sync] in {}", Self::file().display())
        })
    }

    /// The server at `endpoint`, with the token only when it is the configured one
    pub fn remote(&self, endpoint: &str) -> Box<dyn Remote> {
        let token = (self.url.as_deref() == Some(endpoint)).then(|| self.token.clone()).flatten();
        Box::new(Http::new(endpoint, token))
    }
}

#[derive(Debug, Default)]
pub struct Report {
    /// Todos here changed
    pub pulled: bool,
    pub pushed: bool,
    pub errors: Vec<String>,
}

pub fn has_logs(conn: &Connection) -> Result<bool> {
    let n: i64 = conn
        .query_row("SELECT (SELECT count(*) FROM share) + (SELECT count(*) FROM sync_link)", [], |r| r.get(0))
        .map_err(engine::err)?;
    Ok(n > 0)
}

/// Sync every share, then every workspace, each on its own server
pub fn sync_all<'a>(conn: &mut Connection, remote_for: &dyn Fn(&str) -> Box<dyn Remote + 'a>) -> Report {
    let mut report = Report::default();
    let note = |what: &str, result: Result<engine::Outcome>, report: &mut Report| match result {
        Ok(outcome) => {
            report.pulled |= outcome.pulled;
            report.pushed |= outcome.pushed;
        }
        Err(e) => report.errors.push(format!("{what}: {e}")),
    };
    match share::shares(conn) {
        Ok(shares) => {
            for s in shares {
                let remote = remote_for(&s.endpoint);
                let name = format!("share {}", s.name);
                let result = engine::sync(conn, remote.as_ref(), &share::ShareLog { share: s });
                note(&name, result, &mut report);
            }
        }
        Err(e) => report.errors.push(e),
    }
    match workspace::links(conn) {
        Ok(links) => {
            for (ws_uid, secret, endpoint) in links {
                let remote = remote_for(&endpoint);
                let name = workspace_name(conn, &ws_uid);
                let result = engine::sync(conn, remote.as_ref(), &workspace::WorkspaceLog { ws_uid, secret });
                note(&name, result, &mut report);
            }
        }
        Err(e) => report.errors.push(e),
    }
    report
}

pub fn workspace_name(conn: &Connection, ws_uid: &str) -> String {
    conn.query_row("SELECT description FROM workspace WHERE uid = ?1", [ws_uid], |r| r.get::<_, String>(0))
        .unwrap_or_else(|_| "a new workspace".into())
}

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).map_err(engine::err)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(engine::err)?;
    crate::db::Db::create_schema(&conn)?;
    Ok(conn)
}

/// What the background sync last did, for the app to show
#[derive(Debug, Default)]
pub struct Status {
    pub error: Option<String>,
    /// Bumped after every round, so the app sees new errors once
    pub rounds: u64,
}

/// How often to look for changes on the server when nothing changes here
const POLL: Duration = Duration::from_secs(30);

/// Sync in the background: soon after the database changes, and every so often to fetch
/// what changed elsewhere. The app sees what was written through the database file
pub fn spawn(path: PathBuf, config: Config) -> Arc<Mutex<Status>> {
    let status = Arc::new(Mutex::new(Status::default()));
    let shared = Arc::clone(&status);
    std::thread::spawn(move || {
        let modified = || std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let mut seen: Option<SystemTime> = None;
        let mut last = Instant::now() - POLL;
        loop {
            let now_modified = modified();
            if now_modified != seen || last.elapsed() >= POLL {
                // a little time for more edits to land in the same batch
                if now_modified != seen && seen.is_some() {
                    std::thread::sleep(Duration::from_secs(2));
                }
                let error = match open(&path) {
                    Ok(mut conn) => match has_logs(&conn) {
                        Ok(true) => {
                            let report = sync_all(&mut conn, &|e| config.remote(e));
                            (!report.errors.is_empty()).then(|| report.errors.join("; "))
                        }
                        Ok(false) => None,
                        Err(e) => Some(e),
                    },
                    Err(e) => Some(e),
                };
                seen = modified();
                last = Instant::now();
                let mut s = shared.lock().unwrap();
                s.error = error;
                s.rounds += 1;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    status
}
