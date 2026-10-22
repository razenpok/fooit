//! The server side of a log: a base (the whole state at some sequence number) and the
//! batches pushed after it, all sealed. `Http` talks to the Cloudflare Worker in `worker/`;
//! `Memory` keeps the same rules in memory, for tests

use super::crypto::Secret;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SealedBatch {
    pub seq: u64,
    #[serde(with = "b64")]
    pub data: Vec<u8>,
    /// Unix milliseconds, from the server's clock
    pub time: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pull {
    /// The sequence number of the last batch, or of the base when there are none after it
    pub head: u64,
    pub base_seq: u64,
    /// The base, when the client asked for everything or its cursor is older than the base
    #[serde(default, with = "b64_opt")]
    pub base: Option<Vec<u8>>,
    pub batches: Vec<SealedBatch>,
    /// How long ago, by the server's clock, the oldest batch after the base was pushed
    #[serde(default)]
    pub oldest_age: Option<u64>,
}

pub enum Push {
    Done(u64),
    /// Someone pushed first; pull and try again
    Behind,
}

pub trait Remote: Send + Sync {
    /// Start a log, with its first base. Only the owner of the server may do this
    fn create(&self, secret: &Secret, base: &[u8]) -> Result<()>;
    /// Batches after `since`, or the base and everything after it when `since` is None or
    /// older than the base. None when the log does not exist
    fn pull(&self, secret: &Secret, since: Option<u64>) -> Result<Option<Pull>>;
    /// Add a batch, if `expected` is still the newest sequence number
    fn push(&self, secret: &Secret, expected: u64, data: &[u8]) -> Result<Push>;
    /// Replace the base with the state at `seq`, dropping the batches it holds
    fn compact(&self, secret: &Secret, seq: u64, data: &[u8]) -> Result<bool>;
    /// Leave sealed data under a pairing code for ten minutes
    fn pair_put(&self, code: &Secret, data: &[u8]) -> Result<()>;
    /// Take what was left under a pairing code, which is then gone
    fn pair_take(&self, code: &Secret) -> Result<Option<Vec<u8>>>;
}

mod b64 {
    use super::*;
    pub fn serialize<S: serde::Serializer>(v: &[u8], s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&B64.encode(v))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        B64.decode(s).map_err(serde::de::Error::custom)
    }
}

mod b64_opt {
    use super::*;
    pub fn serialize<S: serde::Serializer>(v: &Option<Vec<u8>>, s: S) -> std::result::Result<S::Ok, S::Error> {
        match v {
            Some(v) => s.serialize_some(&B64.encode(v)),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Vec<u8>>, D::Error> {
        match Option::<String>::deserialize(d)? {
            Some(s) => B64.decode(s).map(Some).map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---------------------------------------------------------------------------

/// The Worker's API, see worker/src/index.js
pub struct Http {
    endpoint: String,
    owner_token: Option<String>,
    agent: ureq::Agent,
}

impl Http {
    pub fn new(endpoint: &str, owner_token: Option<String>) -> Http {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build();
        Http { endpoint: endpoint.trim_end_matches('/').to_string(), owner_token, agent }
    }

    fn url(&self, secret: &Secret, rest: &str) -> String {
        format!("{}/v1/logs/{}{}", self.endpoint, secret.log_id(), rest)
    }
}

fn http_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(403, _) => "The sync server refused the secret".into(),
        ureq::Error::Status(401, _) => "The sync server refused the token (FOOIT_SYNC_TOKEN)".into(),
        ureq::Error::Status(code, r) => format!("Sync server error {}: {}", code, r.into_string().unwrap_or_default()),
        ureq::Error::Transport(t) => format!("Could not reach the sync server: {}", t),
    }
}

impl Remote for Http {
    fn create(&self, secret: &Secret, base: &[u8]) -> Result<()> {
        let token = self.owner_token.as_deref().ok_or("Starting to sync needs FOOIT_SYNC_TOKEN, the server's token")?;
        let body = serde_json::json!({"auth": secret.auth_hash(), "base": B64.encode(base)});
        match self.agent.post(&self.url(secret, "")).set("Authorization", &format!("Bearer {token}")).send_json(body) {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(409, _)) => Err("That log exists already".into()),
            Err(e) => Err(http_error(e)),
        }
    }

    fn pull(&self, secret: &Secret, since: Option<u64>) -> Result<Option<Pull>> {
        let mut request = self.agent.get(&self.url(secret, "")).set("X-Fooit-Auth", &secret.auth());
        if let Some(since) = since {
            request = request.query("since", &since.to_string());
        }
        match request.call() {
            Ok(r) => r.into_json().map(Some).map_err(|e| format!("Unexpected answer from the sync server: {e}")),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(http_error(e)),
        }
    }

    fn push(&self, secret: &Secret, expected: u64, data: &[u8]) -> Result<Push> {
        let body = serde_json::json!({"expected": expected, "data": B64.encode(data)});
        match self.agent.post(&self.url(secret, "/batches")).set("X-Fooit-Auth", &secret.auth()).send_json(body) {
            Ok(r) => {
                let v: serde_json::Value = r.into_json().map_err(|e| e.to_string())?;
                Ok(Push::Done(v["head"].as_u64().ok_or("Unexpected answer from the sync server")?))
            }
            Err(ureq::Error::Status(409, _)) => Ok(Push::Behind),
            Err(e) => Err(http_error(e)),
        }
    }

    fn compact(&self, secret: &Secret, seq: u64, data: &[u8]) -> Result<bool> {
        let body = serde_json::json!({"seq": seq, "data": B64.encode(data)});
        match self.agent.post(&self.url(secret, "/base")).set("X-Fooit-Auth", &secret.auth()).send_json(body) {
            Ok(_) => Ok(true),
            Err(ureq::Error::Status(409, _)) => Ok(false),
            Err(e) => Err(http_error(e)),
        }
    }

    fn pair_put(&self, code: &Secret, data: &[u8]) -> Result<()> {
        let body = serde_json::json!({"id": code.log_id(), "data": B64.encode(data)});
        match self.agent.post(&format!("{}/v1/pair", self.endpoint)).send_json(body) {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(409, _)) => Err("That code is taken; try again".into()),
            Err(e) => Err(http_error(e)),
        }
    }

    fn pair_take(&self, code: &Secret) -> Result<Option<Vec<u8>>> {
        match self.agent.get(&format!("{}/v1/pair/{}", self.endpoint, code.log_id())).call() {
            Ok(r) => {
                let v: serde_json::Value = r.into_json().map_err(|e| e.to_string())?;
                let data = v["data"].as_str().ok_or("Unexpected answer from the sync server")?;
                B64.decode(data).map(Some).map_err(|e| e.to_string())
            }
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(ureq::Error::Status(429, _)) => Err("Too many wrong codes were tried; wait a minute".into()),
            Err(e) => Err(http_error(e)),
        }
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
pub use memory::Memory;

#[cfg(test)]
mod memory {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryLog {
        auth_hash: String,
        base_seq: u64,
        base: Vec<u8>,
        batches: Vec<SealedBatch>,
    }

    impl MemoryLog {
        fn head(&self) -> u64 {
            self.batches.last().map_or(self.base_seq, |b| b.seq)
        }
    }

    /// A server in memory, with the same rules as the Worker
    #[derive(Default)]
    pub struct Memory {
        logs: Mutex<HashMap<String, MemoryLog>>,
        codes: Mutex<HashMap<String, Vec<u8>>>,
        /// Added to the clock, to test what happens to old batches
        pub clock_offset_ms: Mutex<u64>,
    }

    impl Memory {
        fn now(&self) -> u64 {
            now_ms() + *self.clock_offset_ms.lock().unwrap()
        }

        fn with_log<T>(&self, secret: &Secret, f: impl FnOnce(&mut MemoryLog) -> T) -> Result<Option<T>> {
            let mut logs = self.logs.lock().unwrap();
            match logs.get_mut(&secret.log_id()) {
                None => Ok(None),
                Some(log) if log.auth_hash != secret.auth_hash() => Err("The sync server refused the secret".into()),
                Some(log) => Ok(Some(f(log))),
            }
        }

        pub fn batch_count(&self, secret: &Secret) -> usize {
            self.logs.lock().unwrap().get(&secret.log_id()).map_or(0, |l| l.batches.len())
        }
    }

    impl Remote for Memory {
        fn create(&self, secret: &Secret, base: &[u8]) -> Result<()> {
            let mut logs = self.logs.lock().unwrap();
            if logs.contains_key(&secret.log_id()) {
                return Err("That log exists already".into());
            }
            logs.insert(
                secret.log_id(),
                MemoryLog { auth_hash: secret.auth_hash(), base_seq: 0, base: base.to_vec(), batches: vec![] },
            );
            Ok(())
        }

        fn pull(&self, secret: &Secret, since: Option<u64>) -> Result<Option<Pull>> {
            let now = self.now();
            self.with_log(secret, |log| {
                let full = since.is_none_or(|s| s < log.base_seq);
                let since = if full { log.base_seq } else { since.unwrap() };
                Pull {
                    head: log.head(),
                    base_seq: log.base_seq,
                    base: full.then(|| log.base.clone()),
                    batches: log.batches.iter().filter(|b| b.seq > since).cloned().collect(),
                    oldest_age: log.batches.first().map(|b| now.saturating_sub(b.time)),
                }
            })
        }

        fn push(&self, secret: &Secret, expected: u64, data: &[u8]) -> Result<Push> {
            let now = self.now();
            self.with_log(secret, |log| {
                if log.head() != expected {
                    return Push::Behind;
                }
                let seq = expected + 1;
                log.batches.push(SealedBatch { seq, data: data.to_vec(), time: now });
                Push::Done(seq)
            })?
            .ok_or_else(|| "No such log on the sync server".to_string())
        }

        fn compact(&self, secret: &Secret, seq: u64, data: &[u8]) -> Result<bool> {
            self.with_log(secret, |log| {
                if seq <= log.base_seq || seq > log.head() {
                    return false;
                }
                log.base_seq = seq;
                log.base = data.to_vec();
                log.batches.retain(|b| b.seq > seq);
                true
            })?
            .ok_or_else(|| "No such log on the sync server".to_string())
        }

        fn pair_put(&self, code: &Secret, data: &[u8]) -> Result<()> {
            let mut codes = self.codes.lock().unwrap();
            if codes.contains_key(&code.log_id()) {
                return Err("That code is taken; try again".into());
            }
            codes.insert(code.log_id(), data.to_vec());
            Ok(())
        }

        fn pair_take(&self, code: &Secret) -> Result<Option<Vec<u8>>> {
            Ok(self.codes.lock().unwrap().remove(&code.log_id()))
        }
    }
}
