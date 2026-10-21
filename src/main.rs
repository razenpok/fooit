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
    db.insert_with_description(ws.as_parent(), true, None, description)?;
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

            if event::poll(Duration::from_millis(200))? {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => app.on_key(key),
                    Event::Paste(text) => app.on_paste(&text),
                    Event::Mouse(mouse) => app.on_mouse(mouse),
                    _ => {}
                }
                dirty = true;
            }
            dirty |= app.tick();
        }
    })();

    // the terminal may be gone already, so this comes before anything is written to it
    app.close();

    let _ = execute!(std::io::stdout(), DisableBracketedPaste, DisableMouseCapture);
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
    fn children_are_shown_on_start() {
        let path = std::env::temp_dir().join(format!("fooit-expanded-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut app = app::App::new(db::Db::open(&path).unwrap());
        press(&mut app, "aWork");
        key(&mut app, KeyCode::Tab);
        press(&mut app, "parent");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        press(&mut app, "Achild");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);

        let mut app = app::App::new(db::Db::open(&path).unwrap());
        press(&mut app, "j");
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Esc);
        assert_eq!(descriptions(&app), ["parent", "child"]);
        press(&mut app, "kz");
        assert_eq!(descriptions(&app), ["parent"]);
    }

    #[test]
    fn completed_todos_are_hidden_and_removed() {
        let mut app = temp_app("completed");
        press(&mut app, "aWork");
        key(&mut app, KeyCode::Enter);
        press(&mut app, "Other");
        key(&mut app, KeyCode::Tab);
        press(&mut app, "elsewhere");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        press(&mut app, "c");
        key(&mut app, KeyCode::Tab);
        press(&mut app, "k");
        key(&mut app, KeyCode::Tab);
        for d in ["done", "keep"] {
            press(&mut app, d);
            key(&mut app, KeyCode::Enter);
        }
        key(&mut app, KeyCode::Esc);
        press(&mut app, "Anested done");
        key(&mut app, KeyCode::Enter);
        press(&mut app, "nested keep");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        // completed todos stay until the cursor leaves them
        press(&mut app, "kc");
        assert_eq!(descriptions(&app), ["done", "keep", "nested done", "nested keep"]);
        press(&mut app, "gg");
        assert_eq!(descriptions(&app), ["done", "keep", "nested keep"]);
        press(&mut app, "c");
        assert_eq!(descriptions(&app), ["done", "keep", "nested keep"]);
        press(&mut app, "j");
        assert_eq!(descriptions(&app), ["keep", "nested keep"]);

        press(&mut app, "H");
        assert_eq!(descriptions(&app), ["done", "keep", "nested done", "nested keep"]);

        press(&mut app, "X");
        assert_eq!(app.mode, app::Mode::Confirm);
        press(&mut app, "y");
        assert_eq!(descriptions(&app), ["keep", "nested keep"]);

        // the other workspace keeps its completed todo
        key(&mut app, KeyCode::Tab);
        press(&mut app, "j");
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Esc);
        assert_eq!(descriptions(&app), ["elsewhere"]);
    }

    fn depths(app: &app::App) -> Vec<(String, usize)> {
        let tree = app.tree(app.focused_tree()).unwrap();
        tree.rows.iter().map(|r| (app.db.description(r.item).to_string(), r.depth)).collect()
    }

    #[test]
    fn mouse_completes_and_drags() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

        let mut app = temp_app("mouse");
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        press(&mut app, "aWork");
        key(&mut app, KeyCode::Tab);
        for d in ["A", "B", "C"] {
            press(&mut app, d);
            key(&mut app, KeyCode::Enter);
        }
        key(&mut app, KeyCode::Esc);

        // (x of a depth's status column, y of a row) as drawn
        let at = |term: &mut Terminal<TestBackend>, app: &mut app::App, name: &str, depth: usize| {
            screen(term, app, name);
            let view = &app.tree(app.focused_tree()).unwrap().view;
            let index = descriptions(app).iter().position(|d| d == name).unwrap();
            let (_, top, _) = view.rows.iter().find(|(i, _, _)| *i == index).unwrap();
            (view.area.x + app::indent(depth) as u16 + 1, *top)
        };
        let mouse = |app: &mut app::App, kind, (x, y): (u16, u16)| {
            app.on_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
        };
        // grabbed by the description, a few columns right of the status
        let drag = |term: &mut Terminal<TestBackend>, app: &mut app::App, from: (u16, u16), to: (u16, u16)| {
            let (from, to) = ((from.0 + 4, from.1), (to.0 + 4, to.1));
            mouse(app, MouseEventKind::Down(MouseButton::Left), from);
            screen(term, app, "drag start");
            mouse(app, MouseEventKind::Drag(MouseButton::Left), to);
            screen(term, app, "dragging");
            mouse(app, MouseEventKind::Up(MouseButton::Left), to);
        };

        // clicking the status completes, which hides the todo once the cursor leaves it
        let b = at(&mut term, &mut app, "B", 0);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), b);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), b);
        assert_eq!(descriptions(&app), ["A", "B", "C"]);
        press(&mut app, "j");
        assert_eq!(descriptions(&app), ["A", "C"]);
        press(&mut app, "H");
        let b = at(&mut term, &mut app, "B", 0);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), b);
        assert_eq!(descriptions(&app), ["A", "B", "C"]);
        press(&mut app, "H");

        // dragging C onto A puts it first
        let (c, a) = (at(&mut term, &mut app, "C", 0), at(&mut term, &mut app, "A", 0));
        drag(&mut term, &mut app, c, a);
        assert_eq!(depths(&app), [("C".into(), 0), ("A".into(), 0), ("B".into(), 0)]);

        // dragging B up onto A and to the right nests it under C
        let (b, a) = (at(&mut term, &mut app, "B", 0), at(&mut term, &mut app, "A", 0));
        drag(&mut term, &mut app, b, (a.0 + 4, a.1));
        assert_eq!(depths(&app), [("C".into(), 0), ("B".into(), 1), ("A".into(), 0)]);

        // C takes its child along when dropped below A
        let (c, a) = (at(&mut term, &mut app, "C", 0), at(&mut term, &mut app, "A", 0));
        drag(&mut term, &mut app, c, a);
        assert_eq!(depths(&app), [("A".into(), 0), ("C".into(), 0), ("B".into(), 1)]);

        // moving only sideways changes the nesting in place
        let b = at(&mut term, &mut app, "B", 1);
        drag(&mut term, &mut app, b, (b.0 - 4, b.1));
        assert_eq!(depths(&app), [("A".into(), 0), ("C".into(), 0), ("B".into(), 0)]);
        let b = at(&mut term, &mut app, "B", 0);
        drag(&mut term, &mut app, b, (b.0 + 8, b.1));
        assert_eq!(depths(&app), [("A".into(), 0), ("C".into(), 0), ("B".into(), 1)]);

        // while editing the mouse is left to the terminal
        press(&mut app, "i");
        assert!(!app.wants_mouse());
        let c = at(&mut term, &mut app, "C", 0);
        drag(&mut term, &mut app, c, (c.0, c.1 - 1));
        key(&mut app, KeyCode::Esc);
        assert!(app.wants_mouse());
        assert_eq!(depths(&app), [("A".into(), 0), ("C".into(), 0), ("B".into(), 1)]);

        // a click starts editing, with the cursor where the click was
        let c = at(&mut term, &mut app, "C", 0);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (c.0 + 20, c.1));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (c.0 + 20, c.1));
        assert_eq!(highlighted(&app), "C");
        assert_eq!(app.mode, app::Mode::Insert);
        press(&mut app, "x");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        let b = at(&mut term, &mut app, "B", 1);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (b.0 + 3, b.1));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (b.0 + 3, b.1));
        press(&mut app, "y");
        key(&mut app, KeyCode::Up);
        assert_eq!(depths(&app), [("A".into(), 0), ("Cx".into(), 0), ("yB".into(), 1)]);
    }

    #[test]
    fn drag_todo_onto_workspace() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

        let mut app = temp_app("drag-ws");
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        press(&mut app, "aWork");
        key(&mut app, KeyCode::Enter);
        press(&mut app, "Other");
        key(&mut app, KeyCode::Tab);
        press(&mut app, "existing");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Tab);
        press(&mut app, "k");
        key(&mut app, KeyCode::Tab);
        for d in ["A", "C"] {
            press(&mut app, d);
            key(&mut app, KeyCode::Enter);
        }
        key(&mut app, KeyCode::Esc);
        press(&mut app, "kAB");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        assert_eq!(depths(&app), [("A".into(), 0), ("B".into(), 1), ("C".into(), 0)]);

        let position = |term: &mut Terminal<TestBackend>, app: &mut app::App, tree: app::TreeRef, name: &str| {
            screen(term, app, name);
            let state = app.tree(tree).unwrap();
            let index = state.rows.iter().position(|r| app.db.description(r.item) == name).unwrap();
            let (_, top, _) = state.view.rows.iter().find(|(i, _, _)| *i == index).unwrap();
            (state.view.area.x + 6, *top)
        };
        let mouse = |app: &mut app::App, kind, (x, y): (u16, u16)| {
            app.on_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
        };

        let todos = app.focused_tree();
        let a = position(&mut term, &mut app, todos, "A");
        let other = position(&mut term, &mut app, app::TreeRef::Workspaces, "Other");
        let work = position(&mut term, &mut app, app::TreeRef::Workspaces, "Work");

        // over its own workspace nothing happens
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), a);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), work);
        let s = screen(&mut term, &mut app, "over own workspace");
        assert!(s.contains("Work"));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), work);
        assert_eq!(descriptions(&app), ["A", "B", "C"]);

        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), a);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), other);
        screen(&mut term, &mut app, "over other workspace");
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), other);
        assert_eq!(descriptions(&app), ["C"]);

        key(&mut app, KeyCode::Tab);
        press(&mut app, "j");
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Esc);
        assert_eq!(depths(&app), [("existing".into(), 0), ("A".into(), 0), ("B".into(), 1)]);
    }

    #[test]
    fn up_and_down_wrap_around() {
        let mut app = temp_app("wrap");
        press(&mut app, "aWork");
        key(&mut app, KeyCode::Tab);
        for d in ["one", "two"] {
            press(&mut app, d);
            key(&mut app, KeyCode::Enter);
        }
        key(&mut app, KeyCode::Esc);
        assert_eq!(highlighted(&app), "two");
        press(&mut app, "j");
        assert_eq!(highlighted(&app), "one");
        key(&mut app, KeyCode::Up);
        assert_eq!(highlighted(&app), "two");

        // also when leaving an edit with the arrows
        press(&mut app, "i");
        key(&mut app, KeyCode::Down);
        assert_eq!(app.mode, app::Mode::Normal);
        assert_eq!(highlighted(&app), "one");
    }

    #[test]
    fn starts_adding_a_todo_to_the_first_workspace() {
        let path = std::env::temp_dir().join(format!("fooit-start-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut app = app::App::new(db::Db::open(&path).unwrap());
        app.start();
        assert!(!app.focused_tree().is_todos() && app.mode == app::Mode::Normal);

        press(&mut app, "aFirst");
        key(&mut app, KeyCode::Enter);
        press(&mut app, "Second");
        key(&mut app, KeyCode::Tab);
        press(&mut app, "old");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);

        let mut app = app::App::new(db::Db::open(&path).unwrap());
        app.start();
        assert!(app.focused_tree().is_todos());
        assert_eq!(app.mode, app::Mode::Insert);
        key(&mut app, KeyCode::Tab);
        assert_eq!(highlighted(&app), "First");
    }

    #[test]
    fn completed_todo_stays_while_the_cursor_is_inside_it() {
        let mut app = temp_app("kept");
        press(&mut app, "aWork");
        key(&mut app, KeyCode::Tab);
        for d in ["parent", "other"] {
            press(&mut app, d);
            key(&mut app, KeyCode::Enter);
        }
        key(&mut app, KeyCode::Esc);
        press(&mut app, "kAchild");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);

        // completing the parent completes the child too; both stay while the cursor is in there
        press(&mut app, "kc");
        assert_eq!(descriptions(&app), ["parent", "child", "other"]);
        press(&mut app, "j");
        assert_eq!(highlighted(&app), "child");
        assert_eq!(descriptions(&app), ["parent", "child", "other"]);
        press(&mut app, "k");
        assert_eq!(descriptions(&app), ["parent", "child", "other"]);
        press(&mut app, "jj");
        assert_eq!(highlighted(&app), "other");
        assert_eq!(descriptions(&app), ["other"]);
    }

    #[test]
    fn closing_while_adding_keeps_no_empty_todo() {
        let path = std::env::temp_dir().join(format!("fooit-close-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut app = app::App::new(db::Db::open(&path).unwrap());
        press(&mut app, "aWork");
        key(&mut app, KeyCode::Tab);
        let ws = app.current_ws().unwrap();
        let in_db = |app: &app::App| app.db.children(db::Parent::Ws(ws), true).len();
        assert_eq!(app.mode, app::Mode::Insert);
        assert_eq!(in_db(&app), 0);
        press(&mut app, "x");
        assert_eq!(in_db(&app), 0);
        key(&mut app, KeyCode::Backspace);
        app.close();
        assert_eq!(in_db(&app), 0);

        let mut app = app::App::new(db::Db::open(&path).unwrap());
        app.start();
        press(&mut app, "typed");
        app.close();

        let mut app = app::App::new(db::Db::open(&path).unwrap());
        press(&mut app, "j");
        key(&mut app, KeyCode::Tab);
        app.close();
        assert_eq!(descriptions(&app), ["typed"]);
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
        assert!(s.contains("Scenario|") && s.contains("Save: enter") && app.mode == app::Mode::Insert);
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        let s = screen(&mut term, &mut app, "workspace added");
        assert!(s.contains("Scenario") && s.contains("Keybindings: ?") && app.mode == app::Mode::Normal);

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
        let s = screen(&mut term, &mut app, "child completed => parent completed, both kept under the cursor");
        assert!(s.contains("Second todo") && s.contains("Write the port"));
        press(&mut app, "HG");
        let s = screen(&mut term, &mut app, "completed shown");
        assert!(s.contains("Second todo") && s.contains("Write the port"));
        press(&mut app, "<<");
        screen(&mut term, &mut app, "dedented");

        // unknown column
        press(&mut app, "r");
        let s = screen(&mut term, &mut app, "no such column");
        assert!(s.contains("No such column: recurrence"));

        // search
        press(&mut app, "/Sec");
        let s = screen(&mut term, &mut app, "searching");
        assert!(s.contains("Search: Sec|") && app.mode == app::Mode::Search);
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
        assert!(s.contains("Save: enter"));
        assert!(app.mode == app::Mode::Insert);
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
        assert!(s.contains("Keybindings") && s.contains("Close: esc"));
        press(&mut app, &"j".repeat(100));
        let s = screen(&mut term, &mut app, "help scrolled");
        assert!(s.contains("Quit fooit"));
        key(&mut app, KeyCode::Esc);
        screen(&mut term, &mut app, "end");
    }
}
