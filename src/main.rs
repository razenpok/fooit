mod app;
mod dates;
mod db;
mod input;
mod keys;
mod ui;
#[cfg(test)]
mod bench;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use ratatui::crossterm::execute;
use std::path::PathBuf;
use std::time::Duration;

/// The version of dooit whose behaviour and database this port follows
const DOOIT_VERSION: &str = "3.3.4";

#[derive(Parser)]
#[command(name = "fooit", about = "A Rust port of the dooit todo manager", disable_version_flag = true)]
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
    let todo = db.insert(ws.as_parent(), true, None)?;
    db.set_description(todo, description)?;
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

    if let Err(e) = run(app::App::new(db)) {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

fn run(mut app: app::App) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste)?;

    let result = (|| -> std::io::Result<()> {
        // redraw only after an event, an outside change, or when the status bar clock moves on
        let mut dirty = true;
        let mut shown_second = 0;
        loop {
            let second = chrono::Local::now().timestamp();
            if dirty || second != shown_second {
                terminal.draw(|f| ui::draw(f, &mut app))?;
                dirty = false;
                shown_second = second;
            }
            if app.quit {
                return Ok(());
            }

            if event::poll(Duration::from_millis(200))? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => app.on_key(key),
                    Event::Paste(text) => app.on_paste(&text),
                    _ => {}
                }
                dirty = true;
            }
            dirty |= app.tick();
        }
    })();

    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

#[cfg(test)]
mod scenario {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    fn press(app: &mut app::App, keys: &str) {
        for c in keys.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn key(app: &mut app::App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn screen(term: &mut Terminal<TestBackend>, app: &mut app::App, label: &str) -> String {
        term.draw(|f| ui::draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        if std::env::var("SHOW").is_ok() {
            println!("===== {} =====\n{}", label, out);
        }
        out
    }

    fn temp_app(name: &str) -> app::App {
        let p = std::env::temp_dir().join(format!("fooit-{}-{}.db", name, std::process::id()));
        let _ = std::fs::remove_file(&p);
        app::App::new(db::Db::open(&p).unwrap())
    }

    fn descriptions(app: &app::App) -> Vec<String> {
        let tree = app.tree(app.focused_tree()).unwrap();
        tree.rows.iter().map(|r| app.db.description(r.item).to_string()).collect()
    }

    fn highlighted(app: &app::App) -> String {
        let tree = app.tree(app.focused_tree()).unwrap();
        app.db.description(tree.current().unwrap()).to_string()
    }

    #[test]
    fn adds_task_to_topmost_workspace() {
        let mut app = temp_app("add-task");
        assert!(add_task(&mut app.db, "nowhere").is_err());
        press(&mut app, "aFirst");
        key(&mut app, KeyCode::Enter);
        press(&mut app, "Second");
        key(&mut app, KeyCode::Tab);
        press(&mut app, "existing");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);

        assert_eq!(add_task(&mut app.db, "buy milk").unwrap(), "First");
        let todos = |db: &db::Db, ws: &str| -> Vec<String> {
            let root = db::Parent::Ws(db.root_id().unwrap());
            let ws = db.children(root, false).into_iter().find(|w| db.description(*w) == ws).unwrap();
            db.children(ws.as_parent(), true).iter().map(|t| db.description(*t).to_string()).collect()
        };
        assert_eq!(todos(&app.db, "First"), ["buy milk"]);
        assert_eq!(todos(&app.db, "Second"), ["existing"]);
        add_task(&mut app.db, "then bread").unwrap();
        assert_eq!(todos(&app.db, "First"), ["buy milk", "then bread"]);
    }

    #[test]
    fn arrows_and_tab_finish_editing() {
        let mut app = temp_app("edit-nav");
        press(&mut app, "a");
        press(&mut app, "Work");
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.mode, app::Mode::Insert);
        assert!(app.focused_tree().is_todos());

        press(&mut app, "one");
        key(&mut app, KeyCode::Enter);
        press(&mut app, "two");
        key(&mut app, KeyCode::Enter);
        press(&mut app, "three");
        key(&mut app, KeyCode::Up);
        assert_eq!(app.mode, app::Mode::Normal);
        assert_eq!(highlighted(&app), "two");

        // an empty new item is dropped and the cursor lands where the arrow points
        press(&mut app, "a");
        key(&mut app, KeyCode::Down);
        assert_eq!(descriptions(&app), ["one", "two", "three"]);
        assert_eq!(highlighted(&app), "three");

        press(&mut app, "i");
        press(&mut app, "!");
        key(&mut app, KeyCode::Tab);
        assert_eq!(descriptions(&app), ["Work"]);
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Esc);
        assert_eq!(descriptions(&app), ["one", "two", "three!"]);
    }

    #[test]
    fn shift_arrows_move_between_top_level_items() {
        let mut app = temp_app("top-level");
        press(&mut app, "aWork");
        key(&mut app, KeyCode::Tab);
        for d in ["one", "two", "three"] {
            press(&mut app, d);
            key(&mut app, KeyCode::Enter);
        }
        key(&mut app, KeyCode::Esc);
        // nest "two" and a child of it under "one"
        press(&mut app, "k>>Achild");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        assert_eq!(descriptions(&app), ["one", "two", "child", "three"]);
        assert_eq!(highlighted(&app), "child");

        let shift = |app: &mut app::App, code| app.on_key(KeyEvent::new(code, KeyModifiers::SHIFT));
        shift(&mut app, KeyCode::Up);
        assert_eq!(highlighted(&app), "one");
        shift(&mut app, KeyCode::Up);
        assert_eq!(highlighted(&app), "one");
        shift(&mut app, KeyCode::Down);
        assert_eq!(highlighted(&app), "three");

        // also finishes an edit
        key(&mut app, KeyCode::Up);
        press(&mut app, "i!");
        shift(&mut app, KeyCode::Up);
        assert_eq!(app.mode, app::Mode::Normal);
        assert_eq!(highlighted(&app), "one");
        assert_eq!(descriptions(&app), ["one", "two", "child!", "three"]);
    }

    #[test]
    fn walkthrough() {
        let path = std::env::var("SCENARIO_DB").map(PathBuf::from).unwrap_or_else(|_| {
            let p = std::env::temp_dir().join(format!("fooit-scenario-{}.db", std::process::id()));
            let _ = std::fs::remove_file(&p);
            p
        });
        let mut app = app::App::new(db::Db::open(&path).unwrap());
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();

        screen(&mut term, &mut app, "start");
        press(&mut app, "j");
        screen(&mut term, &mut app, "first workspace selected");

        // add a workspace, type its name and confirm with enter (adds a sibling), escape drops it
        press(&mut app, "a");
        press(&mut app, "Scenario");
        let s = screen(&mut term, &mut app, "typing workspace");
        assert!(s.contains("Scenario|") && s.contains("INSERT"));
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        let s = screen(&mut term, &mut app, "workspace added");
        assert!(s.contains("Scenario") && s.contains("NORMAL"));

        // tab into the todo list starts a new todo
        key(&mut app, KeyCode::Tab);
        press(&mut app, "Write the port");
        key(&mut app, KeyCode::Enter);
        press(&mut app, "Second todo");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        let s = screen(&mut term, &mut app, "todos added");
        assert!(s.contains("Write the port") && s.contains("Second todo"));

        // a new item left empty is dropped again
        press(&mut app, "a");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.tree(app.focused_tree()).unwrap().rows.len(), 2);

        // due date, urgency, completion
        press(&mut app, "d");
        press(&mut app, "2020-01-02 10:30");
        key(&mut app, KeyCode::Enter);
        press(&mut app, "++");
        let s = screen(&mut term, &mut app, "due + urgency");
        assert!(s.contains("2020-01-02 (10:30)") && s.contains("!3") && s.contains("!"));

        // indent the second todo under the first, then toggle completion of the child
        press(&mut app, ">>");
        let s = screen(&mut term, &mut app, "indented");
        assert!(s.contains("Second todo"));
        press(&mut app, "c");
        screen(&mut term, &mut app, "child completed => parent completed");
        press(&mut app, "<<");
        screen(&mut term, &mut app, "dedented");

        // unknown column
        press(&mut app, "r");
        let s = screen(&mut term, &mut app, "no such column");
        assert!(s.contains("No such column: recurrence"));

        // search
        press(&mut app, "/Sec");
        let s = screen(&mut term, &mut app, "searching");
        assert!(s.contains("/Sec|") && app.mode == app::Mode::Search);
        key(&mut app, KeyCode::Esc);

        // sort
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        key(&mut app, KeyCode::Right);
        let s = screen(&mut term, &mut app, "sort bar");
        assert!(s.contains("reverse") && s.contains("description"));
        key(&mut app, KeyCode::Enter);

        // copy / paste
        press(&mut app, "Yp");
        screen(&mut term, &mut app, "pasted");

        // discard edit with confirmation
        press(&mut app, "i");
        press(&mut app, " changed");
        key(&mut app, KeyCode::Esc);
        let s = screen(&mut term, &mut app, "discard confirm");
        assert!(s.contains("Discard changes? [y/N]"));
        press(&mut app, "n");
        let s = screen(&mut term, &mut app, "still editing");
        assert!(s.contains("INSERT"));
        key(&mut app, KeyCode::Esc);
        press(&mut app, "y");

        // delete everything we made in the scenario workspace
        key(&mut app, KeyCode::Tab);
        press(&mut app, "xx");
        let s = screen(&mut term, &mut app, "delete confirm");
        assert!(s.contains("Are you sure? [y/N]"));
        press(&mut app, "y");
        let s = screen(&mut term, &mut app, "deleted");
        assert!(!s.contains("Scenario"));

        press(&mut app, "?");
        let s = screen(&mut term, &mut app, "help");
        assert!(s.contains("Key Bindings"));
        press(&mut app, &"j".repeat(100));
        let s = screen(&mut term, &mut app, "help scrolled");
        assert!(s.contains("Quit dooit") && s.contains("escape"));
        key(&mut app, KeyCode::Esc);
        screen(&mut term, &mut app, "end");
    }
}
