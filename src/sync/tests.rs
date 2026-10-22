//! Machines syncing through a server in memory

use super::remote::{Memory, Remote};
use super::*;
use crate::db::{Db, Item, Parent};

const ENDPOINT: &str = "https://sync.test";

struct Machine {
    db: Db,
    conn: Connection,
}

impl Machine {
    fn new(name: &str) -> Machine {
        let path = std::env::temp_dir().join(format!("fooit-sync-{}-{}.db", name, std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).unwrap();
        let conn = open(&path).unwrap();
        Machine { db, conn }
    }

    fn sync(&mut self, server: &Memory) -> Report {
        let report = sync_all(&mut self.conn, &|_| Box::new(Shared(server)) as Box<dyn Remote + '_>);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        self.db.load().unwrap();
        report
    }

    fn workspace(&mut self, name: &str) -> Item {
        let root = self.db.ensure_root().unwrap();
        self.db.insert_with_description(Parent::Ws(root), false, None, name).unwrap()
    }

    fn todo(&mut self, ws: Item, text: &str) -> Item {
        self.db.insert_with_description(ws.as_parent(), true, None, text).unwrap()
    }

    fn find(&self, name: &str) -> Option<Item> {
        let root = self.db.root_id()?;
        self.db.children(Parent::Ws(root), false).into_iter().find(|w| self.db.description(*w) == name)
    }

    fn uid(&self, ws: Item) -> String {
        let Item::Ws(id) = ws else { panic!() };
        self.db.workspaces[&id].uid.clone()
    }

    /// Sorted, as imported workspaces arrive in no particular order
    fn workspaces(&self) -> Vec<String> {
        let Some(root) = self.db.root_id() else { return vec![] };
        let mut names: Vec<String> =
            self.db.children(Parent::Ws(root), false).iter().map(|w| self.db.description(*w).to_string()).collect();
        names.sort();
        names
    }

    fn todos(&self, ws: &str) -> Vec<String> {
        let ws = self.find(ws).unwrap();
        self.db.children(ws.as_parent(), true).iter().map(|t| self.db.description(*t).to_string()).collect()
    }

    fn todo_named(&self, ws: &str, text: &str) -> Item {
        let ws = self.find(ws).unwrap();
        self.db.children(ws.as_parent(), true).into_iter().find(|t| self.db.description(*t) == text).unwrap()
    }

    fn share(&mut self, server: &Memory, name: &str, workspaces: &[&str]) -> String {
        let uids: Vec<String> = workspaces.iter().map(|w| self.uid(self.find(w).unwrap())).collect();
        let share = share::create(&self.conn, &Shared(server), name, ENDPOINT, &uids).unwrap();
        crypto::link(ENDPOINT, &share.secret)
    }

    fn import(&mut self, server: &Memory, link: &str) {
        share::import(&self.conn, link, None).unwrap();
        self.sync(server);
    }
}

/// The one server, handed to the engine as many times as it asks
struct Shared<'a>(&'a Memory);

impl Remote for Shared<'_> {
    fn create(&self, s: &crypto::Secret, base: &[u8]) -> Result<()> {
        self.0.create(s, base)
    }
    fn pull(&self, s: &crypto::Secret, since: Option<u64>) -> Result<Option<remote::Pull>> {
        self.0.pull(s, since)
    }
    fn push(&self, s: &crypto::Secret, expected: u64, data: &[u8]) -> Result<remote::Push> {
        self.0.push(s, expected, data)
    }
    fn compact(&self, s: &crypto::Secret, seq: u64, data: &[u8]) -> Result<bool> {
        self.0.compact(s, seq, data)
    }
}

fn three_workspaces(me: &mut Machine) {
    for name in ["general", "dev", "art"] {
        let ws = me.workspace(name);
        me.todo(ws, &format!("{name} 1"));
        me.todo(ws, &format!("{name} 2"));
    }
}

#[test]
fn shares_bring_in_their_workspaces() {
    let server = Memory::default();
    let mut me = Machine::new("me");
    three_workspaces(&mut me);
    let dev_link = me.share(&server, "example-dev", &["general", "dev"]);
    let art_link = me.share(&server, "example-art", &["general", "art"]);

    let mut dev = Machine::new("dev");
    dev.import(&server, &dev_link);
    assert_eq!(dev.workspaces(), ["dev", "general"]);
    assert_eq!(dev.todos("dev"), ["dev 1", "dev 2"]);

    let mut art = Machine::new("art");
    art.workspace("mine");
    art.import(&server, &art_link);
    assert_eq!(art.workspaces(), ["art", "general", "mine"]);

    // a change in general reaches everyone, through either share
    let t = dev.todo_named("general", "general 1");
    dev.db.set_description(t, "general one").unwrap();
    let t = dev.todo_named("general", "general 2");
    let mut todo = dev.db.todo(match t { Item::Todo(id) => id, _ => panic!() }).unwrap().clone();
    todo.pending = false;
    todo.urgency = 3;
    dev.db.update_todo(todo).unwrap();
    let general = dev.find("general").unwrap();
    dev.todo(general, "from dev");
    assert!(dev.sync(&server).pushed);

    assert!(art.sync(&server).pulled);
    assert_eq!(art.todos("general"), ["general one", "general 2", "from dev"]);
    let Item::Todo(id) = art.todo_named("general", "general 2") else { panic!() };
    let t = art.db.todo(id).unwrap();
    assert_eq!((t.pending, t.urgency), (false, 3));
    me.sync(&server);
    assert_eq!(me.todos("general"), ["general one", "general 2", "from dev"]);

    // nothing to do the second time round
    let report = art.sync(&server);
    assert!(!report.pulled && !report.pushed);
}

#[test]
fn the_first_change_to_reach_the_server_wins() {
    let server = Memory::default();
    let mut me = Machine::new("first-me");
    three_workspaces(&mut me);
    let link = me.share(&server, "all", &["general"]);
    let mut other = Machine::new("first-other");
    other.import(&server, &link);

    let t = me.todo_named("general", "general 1");
    me.db.set_description(t, "mine").unwrap();
    let t = other.todo_named("general", "general 1");
    other.db.set_description(t, "theirs").unwrap();
    let t = other.todo_named("general", "general 2");
    other.db.set_description(t, "also theirs").unwrap();

    me.sync(&server);
    other.sync(&server);
    me.sync(&server);
    assert_eq!(me.todos("general"), ["mine", "also theirs"]);
    assert_eq!(other.todos("general"), ["mine", "also theirs"]);
}

#[test]
fn moves_and_removals_follow() {
    let server = Memory::default();
    let mut me = Machine::new("moves-me");
    three_workspaces(&mut me);
    let link = me.share(&server, "all", &["general", "dev"]);
    let mut other = Machine::new("moves-other");
    other.import(&server, &link);

    // reorder, nest, and move a todo to another synced workspace
    let one = me.todo_named("general", "general 1");
    let two = me.todo_named("general", "general 2");
    me.db.move_to(one, two.as_parent(), None).unwrap();
    let d1 = me.todo_named("dev", "dev 1");
    let general = me.find("general").unwrap();
    me.db.move_to(d1, general.as_parent(), None).unwrap();
    let d2 = me.todo_named("dev", "dev 2");
    me.db.delete(d2).unwrap();
    me.sync(&server);
    other.sync(&server);
    assert_eq!(other.todos("general"), ["dev 1", "general 2"]);
    assert_eq!(other.todos("dev"), Vec::<String>::new());
    let two = other.todo_named("general", "general 2");
    let kids: Vec<_> = other.db.children(two.as_parent(), true).iter().map(|t| other.db.description(*t).to_string()).collect();
    assert_eq!(kids, ["general 1"]);

    // removing a synced workspace removes it everywhere, and stops syncing it
    let dev = other.find("dev").unwrap();
    other.db.delete(dev).unwrap();
    other.sync(&server);
    me.sync(&server);
    assert_eq!(me.workspaces(), ["art", "general"]);
    assert_eq!(workspace::links(&me.conn).unwrap().len(), 1);
}

#[test]
fn leaving_and_removing_from_shares() {
    let server = Memory::default();
    let mut me = Machine::new("rm-me");
    three_workspaces(&mut me);
    let dev_link = me.share(&server, "example-dev", &["general", "dev"]);
    let art_link = me.share(&server, "example-art", &["general", "art"]);
    let mut dev = Machine::new("rm-dev");
    dev.import(&server, &dev_link);
    let mut art = Machine::new("rm-art");
    art.import(&server, &art_link);

    // general leaves example-art: art keeps what it had and gets nothing new
    let s = share::find(&me.conn, "example-art").unwrap();
    let general = me.uid(me.find("general").unwrap());
    share::remove_member(&me.conn, &Shared(&server), &s, &general).unwrap();
    me.sync(&server);
    dev.sync(&server);
    art.sync(&server);
    let ws = me.find("general").unwrap();
    me.todo(ws, "secret plans");
    me.sync(&server);
    dev.sync(&server);
    art.sync(&server);
    assert_eq!(dev.todos("general"), ["general 1", "general 2", "secret plans"]);
    assert_eq!(art.todos("general"), ["general 1", "general 2"]);
    assert_eq!(art.workspaces(), ["art", "general"]);
    assert!(workspace::link(&art.conn, &general).unwrap().is_none());

    // and changes from dev still reach me on the new log
    let t = dev.todo_named("general", "secret plans");
    dev.db.set_description(t, "plans").unwrap();
    dev.sync(&server);
    me.sync(&server);
    assert_eq!(me.todos("general"), ["general 1", "general 2", "plans"]);

    // leaving a share stops syncing its workspaces, which stay
    let s = share::find(&dev.conn, "shared").unwrap();
    share::forget(&dev.conn, &s).unwrap();
    assert!(!has_logs(&dev.conn).unwrap());
    dev.db.load().unwrap();
    assert_eq!(dev.workspaces(), ["dev", "general"]);
}

#[test]
fn old_batches_fold_into_the_base() {
    let server = Memory::default();
    let mut me = Machine::new("compact-me");
    three_workspaces(&mut me);
    let link = me.share(&server, "all", &["general"]);
    let mut away = Machine::new("compact-away");
    away.import(&server, &link);
    let secret = workspace::links(&me.conn).unwrap()[0].1.clone();

    for i in 0..3 {
        let ws = me.find("general").unwrap();
        me.todo(ws, &format!("new {i}"));
        me.sync(&server);
    }
    assert_eq!(server.batch_count(&secret), 3);

    // a local change on a machine that was away for long
    let t = away.todo_named("general", "general 1");
    away.db.set_description(t, "edited while away").unwrap();

    *server.clock_offset_ms.lock().unwrap() = engine::COMPACT_AFTER_MS + 1000;
    let ws = me.find("general").unwrap();
    me.todo(ws, "later");
    me.sync(&server);
    me.sync(&server);
    assert_eq!(server.batch_count(&secret), 0);

    away.sync(&server);
    assert_eq!(away.todos("general"), ["edited while away", "general 2", "new 0", "new 1", "new 2", "later"]);
    me.sync(&server);
    assert_eq!(me.todos("general"), away.todos("general"));
}
