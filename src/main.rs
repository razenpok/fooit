mod app;
mod dates;
mod db;
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

/// The version of dooit whose behaviour and database this port follows
const DOOIT_VERSION: &str = "3.3.4";

#[derive(Parser)]
#[command(name = "fooit", about = "A terminal todo manager that shares dooit's database", disable_version_flag = true)]
struct Cli {
    /// Show version and exit.
    #[arg(short = 'v', long)]
    version: bool,

    /// Path to config file (dooit's python configs are not supported and are ignored)
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
    /// Show config location.
    ConfigLoc,
}

fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("dooit")
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
    db.quick_add_todo(ws.as_parent(), None, description)?;
    Ok(db.description(ws).to_string())
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // anything that is not an option or command is a new task
            let task = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
            let task = task.trim();
            if task.is_empty() || matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                e.exit();
            }
            let path = data_dir().join("dooit.db");
            match db::Db::open(&path).and_then(|mut db| add_task(&mut db, task)) {
                Ok(ws) => println!("Added \"{}\" to {}", task, ws),
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
        let dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("dooit");
        println!("{}", dir.join("config.py").display());
        return;
    }

    if let Some(config) = &cli.config {
        if !config.is_file() {
            println!("Config file {} not found.", config.display());
            return;
        }
    }

    if data_dir().join("todo.yaml").exists() {
        eprintln!("Found todos for v2.");
        eprintln!("Please migrate to v3 using `dooit migrate` first.");
        return;
    }

    let db_path = match &cli.db {
        Some(path) => expand_user(path),
        None => data_dir().join("dooit.db"),
    };

    let db = match db::Db::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Could not open database {}: {}", db_path.display(), e);
            std::process::exit(1);
        }
    };

    let mut app = app::App::new(db);
    app.start();
    if let Err(e) = run(app) {
        eprintln!("{}", e);
        std::process::exit(1);
    }
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

            match wait_for_input(tty, Duration::from_millis(200)) {
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
