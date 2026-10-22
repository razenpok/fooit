//! What a log holds and how it changes. A state maps uids to records, a record maps field
//! names to values, and a batch of changes maps uids to the fields that changed, or to
//! nothing for a removed row. Merging follows "first wins": a change that reached the
//! server first stays, and a local change to the same field or row is dropped

use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub type Record = Map<String, Value>;
pub type State = BTreeMap<String, Record>;
pub type Batch = BTreeMap<String, Option<Record>>;

/// The field holding `[parent uid, position]`, the only one the tree rules look at
pub const AT: &str = "at";

/// The changes that turn `old` into `new`
pub fn diff(old: &State, new: &State) -> Batch {
    let mut batch = Batch::new();
    for (uid, record) in new {
        match old.get(uid) {
            None => {
                batch.insert(uid.clone(), Some(record.clone()));
            }
            Some(before) => {
                let changed: Record = record
                    .iter()
                    .filter(|(k, v)| before.get(*k) != Some(*v))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if !changed.is_empty() {
                    batch.insert(uid.clone(), Some(changed));
                }
            }
        }
    }
    for uid in old.keys() {
        if !new.contains_key(uid) {
            batch.insert(uid.clone(), None);
        }
    }
    batch
}

/// Apply changes as every client does, in the order the server gave them. Changes to a row
/// that is gone are skipped, so a row only comes back as a whole new record
pub fn apply(state: &mut State, batch: &Batch) {
    for (uid, change) in batch {
        match change {
            None => {
                state.remove(uid);
            }
            Some(fields) => match state.get_mut(uid) {
                Some(record) => {
                    for (k, v) in fields {
                        record.insert(k.clone(), v.clone());
                    }
                }
                None if fields.contains_key("k") => {
                    state.insert(uid.clone(), fields.clone());
                }
                None => {}
            },
        }
    }
    fix_tree(state, None);
}

/// Combine the local changes made since `old` with the state the server has now. Local
/// changes to fields or rows the server changed first are dropped
pub fn merge(old: &State, remote: &State, local: &State) -> State {
    let theirs = diff(old, remote);
    let ours = diff(old, local);
    let mut merged = remote.clone();
    for (uid, change) in &ours {
        match (change, theirs.get(uid)) {
            // a row the server changed stays as the server has it
            (None, Some(_)) => {}
            (None, None) => {
                merged.remove(uid);
            }
            // a row the server removed first stays removed
            (Some(_), Some(None)) => {}
            (Some(fields), Some(Some(their_fields))) => {
                if let Some(record) = merged.get_mut(uid) {
                    for (k, v) in fields {
                        if !their_fields.contains_key(k) {
                            record.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            (Some(fields), None) => match merged.get_mut(uid) {
                Some(record) => {
                    for (k, v) in fields {
                        record.insert(k.clone(), v.clone());
                    }
                }
                None => {
                    merged.insert(uid.clone(), fields.clone());
                }
            },
        }
    }
    fix_tree(&mut merged, Some(remote));
    merged
}

fn parent_of(record: &Record) -> Option<&str> {
    record.get(AT)?.as_array()?.first()?.as_str()
}

enum Problem {
    MissingParent(String),
    Cycle(Vec<String>),
}

fn find_problem(state: &State) -> Option<Problem> {
    for (uid, record) in state {
        let mut path = vec![uid.as_str()];
        let mut at = parent_of(record);
        while let Some(parent) = at {
            if let Some(start) = path.iter().position(|p| *p == parent) {
                return Some(Problem::Cycle(path[start..].iter().map(|s| s.to_string()).collect()));
            }
            match state.get(parent) {
                Some(p) => {
                    path.push(parent);
                    at = parent_of(p);
                }
                None => return Some(Problem::MissingParent(path.last().unwrap().to_string())),
            }
        }
    }
    None
}

/// Keep the rows a tree: a row whose parent is gone goes too, and rows that ended up
/// beneath themselves (two machines moving things into each other) go back to where
/// `fallback` has them, or are removed. Rows without a parent are the log's own top
pub fn fix_tree(state: &mut State, fallback: Option<&State>) {
    while let Some(problem) = find_problem(state) {
        match problem {
            Problem::MissingParent(uid) => {
                state.remove(&uid);
            }
            Problem::Cycle(members) => {
                let back = members.iter().find_map(|m| {
                    let at = fallback?.get(m)?.get(AT)?;
                    (state[m].get(AT) != Some(at)).then(|| (m.clone(), at.clone()))
                });
                match back {
                    Some((uid, at)) => {
                        state.get_mut(&uid).unwrap().insert(AT.into(), at);
                    }
                    None => {
                        state.remove(&members[0]);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(v: Value) -> Record {
        v.as_object().unwrap().clone()
    }

    fn state(rows: &[(&str, Value)]) -> State {
        rows.iter().map(|(k, v)| (k.to_string(), rec(v.clone()))).collect()
    }

    fn base() -> State {
        state(&[
            ("w", json!({"k": "w", "d": "Work"})),
            ("a", json!({"k": "t", "d": "a", "u": 1, "at": ["w", "V"]})),
            ("b", json!({"k": "t", "d": "b", "u": 1, "at": ["w", "k"]})),
        ])
    }

    #[test]
    fn diff_then_apply_gives_the_new_state() {
        let old = base();
        let mut new = base();
        new.get_mut("a").unwrap().insert("d".into(), json!("A"));
        new.remove("b");
        new.insert("c".into(), rec(json!({"k": "t", "d": "c", "u": 2, "at": ["a", "V"]})));
        let batch = diff(&old, &new);
        assert_eq!(batch["a"], Some(rec(json!({"d": "A"}))));
        assert_eq!(batch["b"], None);
        let mut applied = old.clone();
        apply(&mut applied, &batch);
        assert_eq!(applied, new);
    }

    #[test]
    fn first_change_wins() {
        let old = base();
        let mut remote = base();
        remote.get_mut("a").unwrap().insert("d".into(), json!("theirs"));
        let mut local = base();
        local.get_mut("a").unwrap().insert("d".into(), json!("ours"));
        local.get_mut("a").unwrap().insert("u".into(), json!(3));
        local.get_mut("b").unwrap().insert("d".into(), json!("ours too"));
        let merged = merge(&old, &remote, &local);
        assert_eq!(merged["a"]["d"], json!("theirs"));
        assert_eq!(merged["a"]["u"], json!(3));
        assert_eq!(merged["b"]["d"], json!("ours too"));
    }

    #[test]
    fn removals() {
        let old = base();
        // removed there, changed here: stays removed
        let mut remote = base();
        remote.remove("a");
        let mut local = base();
        local.get_mut("a").unwrap().insert("d".into(), json!("ours"));
        assert!(!merge(&old, &remote, &local).contains_key("a"));

        // changed there, removed here: comes back
        let mut remote = base();
        remote.get_mut("a").unwrap().insert("d".into(), json!("theirs"));
        let mut local = base();
        local.remove("a");
        local.remove("b");
        let merged = merge(&old, &remote, &local);
        assert_eq!(merged["a"]["d"], json!("theirs"));
        assert!(!merged.contains_key("b"));
    }

    #[test]
    fn a_new_child_of_a_removed_row_goes_too() {
        let old = base();
        let mut remote = base();
        remote.remove("a");
        let mut local = base();
        local.insert("c".into(), rec(json!({"k": "t", "d": "c", "u": 1, "at": ["a", "V"]})));
        let merged = merge(&old, &remote, &local);
        assert!(!merged.contains_key("c") && !merged.contains_key("a"));
    }

    #[test]
    fn moves_into_each_other_keep_the_first() {
        let old = base();
        let mut remote = base();
        remote.get_mut("a").unwrap().insert(AT.into(), json!(["b", "V"]));
        let mut local = base();
        local.get_mut("b").unwrap().insert(AT.into(), json!(["a", "V"]));
        let merged = merge(&old, &remote, &local);
        assert_eq!(merged["a"][AT], json!(["b", "V"]));
        assert_eq!(merged["b"][AT], json!(["w", "k"]));
    }
}
