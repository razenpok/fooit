mod app;
mod dates;
mod db;
mod migrate;
mod order;
mod sync;
mod input;
mod keys;
mod ui;
#[cfg(test)]
mod bench;
#[cfg(test)]
mod scenario;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The version of dooit whose behaviour this port follows
const DOOIT_VERSION: &str = "3.3.4";

#[derive(Parser)]
#[command(name = "fooit", about = "A terminal todo manager", disable_version_flag = true)]
struct Cli {
    /// Show version and exit.
    #[arg(short = 'v', long)]
    version: bool,

    /// Path to the keys file (default: see config-loc)
    #[arg(short = 'c', long)]
    config: Option<PathBuf>,

    /// Path to database file
    #[arg(long)]
    db: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show where the keys file is read from.
    ConfigLoc,
    /// Print the current key bindings as a keys file, to start one from.
    Keys,
    /// Share workspaces through the sync server, and add shares from others.
    Share {
        #[command(subcommand)]
        action: ShareCommand,
    },
    /// Sync now, and say how it went.
    Sync,
}

#[derive(Subcommand)]
enum ShareCommand {
    /// Make a share holding some workspaces, and print its link.
    New {
        name: String,
        #[arg(required = true)]
        workspaces: Vec<String>,
    },
    /// Put workspaces in a share.
    Add {
        share: String,
        #[arg(required = true)]
        workspaces: Vec<String>,
    },
    /// Take workspaces out of a share. Its people keep what they have and get nothing new.
    Remove {
        share: String,
        #[arg(required = true)]
        workspaces: Vec<String>,
    },
    /// Print the link of a share, to give to the people (or machines) it is for.
    Link { share: String },
    /// List the shares here, with their workspaces.
    List,
    /// Add a share from its link; its workspaces come in with it.
    Import {
        link: String,
        /// What to call the share here
        #[arg(long)]
        name: Option<String>,
    },
    /// Forget a share here. Workspaces that no other share holds stop syncing and stay.
    Leave { share: String },
    /// Call a share something else, here only.
    Rename { share: String, name: String },
}

/// The words that start a command, so a mistyped command is not added as a todo
const COMMANDS: &[&str] = &["config-loc", "keys", "share", "sync", "help"];

fn keys_file(cli: &Cli) -> PathBuf {
    match &cli.config {
        Some(path) => path.clone(),
        None => dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("fooit").join("keys.toml"),
    }
}

/// The key bindings, from the keys file when there is one
fn load_keys(cli: &Cli) -> Result<keys::KeyManager, String> {
    let path = keys_file(cli);
    match std::fs::read_to_string(&path) {
        Ok(text) => keys::KeyManager::from_file(&text).map_err(|e| format!("{}: {}", path.display(), e)),
        Err(_) if cli.config.is_none() => Ok(keys::KeyManager::with_defaults()),
        Err(e) => Err(format!("{}: {}", path.display(), e)),
    }
}

fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// The database to use: `--db`, or fooit's own, which starts as a copy of dooit's when there is one
fn db_path(cli_db: Option<&str>) -> Result<PathBuf, String> {
    if let Some(path) = cli_db {
        return Ok(expand_user(path));
    }
    let path = data_dir().join("fooit").join("fooit.db");
    let dooit = data_dir().join("dooit").join("dooit.db");
    if !path.exists() && dooit.exists() {
        migrate::from_dooit(&dooit, &path).map_err(|e| format!("Could not import {}: {}", dooit.display(), e))?;
        eprintln!("Imported dooit's todos from {}", dooit.display());
    }
    Ok(path)
}

fn open_db(cli_db: Option<&str>) -> Result<db::Db, String> {
    let path = db_path(cli_db)?;
    db::Db::open(&path).map_err(|e| format!("Could not open database {}: {}", path.display(), e))
}

fn expand_user(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~") {
        if rest.is_empty() || rest.starts_with('/') {
            if let Some(home) = dirs::home_dir() {
                return home.join(rest.trim_start_matches('/'));
            }
        }
    }
    PathBuf::from(path)
}

/// Add a todo at the bottom of the topmost workspace, returning that workspace's description
fn add_task(db: &mut db::Db, description: &str) -> Result<String, String> {
    let root = db.root_id().ok_or("No workspace to add the task to")?;
    let ws = *db.children(db::Parent::Ws(root), false).first().ok_or("No workspace to add the task to")?;
    let todo = db.quick_add_todo(ws.as_parent(), None, description)?;
    match db.parent_of(todo) {
        Some(db::Parent::Ws(id)) => Ok(db.description(db::Item::Ws(id)).to_string()),
        _ => Ok(db.description(ws).to_string()),
    }
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // anything that is not an option or command is a new task
            let task = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
            let task = task.trim();
            let first = std::env::args().nth(1).unwrap_or_default();
            if task.is_empty()
                || matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion)
                || COMMANDS.contains(&first.as_str())
            {
                e.exit();
            }
            // `--db` is not read here, as anything that fails to parse ends up here
            match open_db(None).and_then(|mut db| add_task(&mut db, task).map(|ws| (db, ws))) {
                Ok((db, ws)) => {
                    println!("Added \"{}\" to {}", task, ws);
                    sync_quietly(&db);
                }
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            }
            return;
        }
    };

    if cli.version {
        println!("fooit - {} (dooit {})", env!("CARGO_PKG_VERSION"), DOOIT_VERSION);
        return;
    }

    if let Some(Command::ConfigLoc) = cli.command {
        println!("{}", keys_file(&cli).display());
        return;
    }

    let keys = match load_keys(&cli) {
        Ok(keys) => keys,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };
    if let Some(Command::Keys) = cli.command {
        print!("{}", keys.to_file());
        return;
    }

    if let Some(command @ (Command::Share { .. } | Command::Sync)) = &cli.command {
        if let Err(e) = run_sync_command(command, cli.db.as_deref()) {
            eprintln!("{}", e);
            std::process::exit(1);
        }
        return;
    }

    let db_file = match db_path(cli.db.as_deref()) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };
    let db = match open_db(cli.db.as_deref()) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    let mut app = app::App::new(db);
    app.keys = keys;
    match sync::Config::load() {
        Ok(config) => {
            let wanted = sync::open(&db_file).and_then(|c| sync::has_logs(&c)).unwrap_or(false);
            if wanted {
                app.sync = Some(sync::spawn(db_file, config));
            }
        }
        Err(e) => eprintln!("{}", e),
    }
    app.start();
    if let Err(e) = run(app) {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

/// A workspace by name, as with `#name` when adding a todo
fn find_workspace(db: &db::Db, name: &str) -> Result<String, String> {
    let tag = format!("#{}", name.replace(' ', ""));
    match db.workspace_tag(&tag) {
        (_, Some(id)) => Ok(db.workspaces[&id].uid.clone()),
        _ => Err(format!("No workspace called \"{}\"", name)),
    }
}

/// Push a change made from the command line straight away, when anything is synced
fn sync_quietly(db: &db::Db) {
    let Ok(config) = sync::Config::load() else { return };
    let Ok(mut conn) = sync::open(db.path()) else { return };
    if sync::has_logs(&conn).unwrap_or(false) {
        let report = sync::sync_all(&mut conn, &|e| config.remote(e));
        for e in report.errors {
            eprintln!("Sync: {}", e);
        }
    }
}

fn run_sync_command(command: &Command, cli_db: Option<&str>) -> Result<(), String> {
    let db = open_db(cli_db)?;
    let config = sync::Config::load()?;
    let mut conn = sync::open(db.path())?;
    let names = |conn: &rusqlite::Connection, uids: &[String]| -> Vec<String> {
        uids.iter().map(|u| sync::workspace_name(conn, u)).collect()
    };
    let sync_now = |conn: &mut rusqlite::Connection| -> Result<sync::Report, String> {
        let report = sync::sync_all(conn, &|e| config.remote(e));
        if report.errors.is_empty() { Ok(report) } else { Err(report.errors.join("\n")) }
    };

    let Command::Share { action } = command else {
        if !sync::has_logs(&conn)? {
            return Err("Nothing is synced yet; start with `fooit share new NAME WORKSPACE...`".into());
        }
        let report = sync_now(&mut conn)?;
        println!(
            "Synced{}",
            match (report.pulled, report.pushed) {
                (true, true) => ": sent and got changes",
                (true, false) => ": got changes",
                (false, true) => ": sent changes",
                (false, false) => ", nothing new",
            }
        );
        return Ok(());
    };

    match action {
        ShareCommand::New { name, workspaces } => {
            let endpoint = config.require_url()?.to_string();
            let uids = workspaces.iter().map(|w| find_workspace(&db, w)).collect::<Result<Vec<_>, _>>()?;
            let remote = config.remote(&endpoint);
            let share = sync::share::create(&conn, remote.as_ref(), name, &endpoint, &uids)?;
            println!("Shared {} as \"{}\". Its link:\n", names(&conn, &uids).join(", "), share.name);
            println!("  {}\n", sync::crypto::link(&endpoint, &share.secret));
            println!("Anyone with the link can read and change these workspaces. On another machine:");
            println!("  fooit share import '<link>'");
        }
        ShareCommand::Add { share, workspaces } => {
            let share = sync::share::find(&conn, share)?;
            let remote = config.remote(&share.endpoint);
            let uids = workspaces.iter().map(|w| find_workspace(&db, w)).collect::<Result<Vec<_>, _>>()?;
            for uid in &uids {
                sync::share::add_member(&conn, remote.as_ref(), &share, uid)?;
            }
            sync_now(&mut conn)?;
            println!("Added {} to \"{}\"", names(&conn, &uids).join(", "), share.name);
        }
        ShareCommand::Remove { share, workspaces } => {
            let share = sync::share::find(&conn, share)?;
            let remote = config.remote(&share.endpoint);
            let uids = workspaces.iter().map(|w| find_workspace(&db, w)).collect::<Result<Vec<_>, _>>()?;
            // send what changed to the old log first
            sync_now(&mut conn)?;
            for uid in &uids {
                sync::share::remove_member(&conn, remote.as_ref(), &share, uid)?;
            }
            sync_now(&mut conn)?;
            println!("Took {} out of \"{}\"", names(&conn, &uids).join(", "), share.name);
        }
        ShareCommand::Link { share } => {
            let share = sync::share::find(&conn, share)?;
            println!("{}", sync::crypto::link(&share.endpoint, &share.secret));
        }
        ShareCommand::List => {
            let shares = sync::share::shares(&conn)?;
            if shares.is_empty() {
                println!("No shares yet; make one with `fooit share new NAME WORKSPACE...`");
            }
            for share in shares {
                let members = sync::share::members(&conn, share.id)?;
                println!("{}: {}", share.name, names(&conn, &members).join(", "));
            }
        }
        ShareCommand::Import { link, name } => {
            let share = sync::share::import(&conn, link, name.as_deref())?;
            if let Err(e) = sync_now(&mut conn) {
                sync::share::forget(&conn, &share)?;
                return Err(e);
            }
            let members = sync::share::members(&conn, share.id)?;
            println!("Added the share \"{}\": {}", share.name, names(&conn, &members).join(", "));
        }
        ShareCommand::Leave { share } => {
            let share = sync::share::find(&conn, share)?;
            sync::share::forget(&conn, &share)?;
            println!("Left \"{}\"; its workspaces stay here", share.name);
        }
        ShareCommand::Rename { share, name } => {
            let share = sync::share::find(&conn, share)?;
            sync::share::rename(&conn, &share, name)?;
        }
    }
    Ok(())
}

fn run(mut app: app::App) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;

    // closing the terminal window hangs up; stop like a quit instead of being killed
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGHUP, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
    let resized = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGWINCH, Arc::clone(&resized))?;

    // crossterm reads from stdin when it is a terminal, otherwise from /dev/tty
    let dev_tty;
    let tty = if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
        libc::STDIN_FILENO
    } else {
        dev_tty = std::fs::File::open("/dev/tty")?;
        std::os::fd::AsRawFd::as_raw_fd(&dev_tty)
    };

    let result = (|| -> std::io::Result<()> {
        // redraw only after an event, an outside change, or each minute so todos turn overdue
        let mut dirty = true;
        let mut shown_minute = 0;
        let mut capturing = true;
        loop {
            if app.wants_mouse() != capturing {
                capturing = !capturing;
                if capturing {
                    execute!(std::io::stdout(), EnableMouseCapture)?;
                } else {
                    execute!(std::io::stdout(), DisableMouseCapture)?;
                }
            }
            let minute = chrono::Local::now().timestamp() / 60;
            if dirty || minute != shown_minute {
                terminal.draw(|f| ui::draw(f, &mut app))?;
                dirty = false;
                shown_minute = minute;
            }
            if app.quit || stop.load(Ordering::Relaxed) {
                return Ok(());
            }

            // check more often while dragging, so the list scrolls smoothly at its edges
            let wait = if app.drag.is_some() { 30 } else { 200 };
            match wait_for_input(tty, Duration::from_millis(wait)) {
                Input::Gone => return Ok(()),
                Input::None => {}
                Input::Ready => {
                    while event::poll(Duration::ZERO)? {
                        match event::read()? {
                            Event::Key(key) if key.kind != KeyEventKind::Release => app.on_key(key),
                            Event::Paste(text) => app.on_paste(&text),
                            Event::Mouse(mouse) => app.on_mouse(mouse),
                            _ => {}
                        }
                        dirty = true;
                    }
                }
            }
            // the drawing picks up the new size
            dirty |= resized.swap(false, Ordering::Relaxed);
            dirty |= app.tick();
        }
    })();

    // the terminal may be gone already, so this comes before anything is written to it
    app.close();

    // not ratatui::restore, which panics reporting a failure to a terminal that is gone
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        ratatui::crossterm::terminal::LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show
    );
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
    // its drop would show the cursor again, and panic too when that fails
    std::mem::forget(terminal);
    result
}

enum Input {
    Ready,
    None,
    Gone,
}

/// Wait up to a timeout for terminal input. crossterm keeps re-reading a terminal that hung up,
/// at full speed and without ever returning, so it only gets to read once there is real input
fn wait_for_input(tty: i32, timeout: Duration) -> Input {
    let mut fd = libc::pollfd { fd: tty, events: libc::POLLIN, revents: 0 };
    let ready = unsafe { libc::poll(&mut fd, 1, timeout.as_millis() as i32) };
    if ready <= 0 {
        // timed out, or interrupted by a signal
        return Input::None;
    }
    if fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return Input::Gone;
    }
    // readable with nothing to read is the end of input
    let mut available: libc::c_int = 0;
    let result = unsafe { libc::ioctl(tty, libc::FIONREAD, &mut available) };
    if result == 0 && available > 0 { Input::Ready } else { Input::Gone }
}
