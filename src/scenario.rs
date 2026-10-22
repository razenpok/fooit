//! Scenario tests driving the app through keys and the mouse, with screens drawn to a test backend

use super::*;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;

fn press(app: &mut app::App, keys: &str) {
    for c in keys.chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

/// Save the name being typed and open the todos of that workspace, starting a new todo
fn save_and_open(app: &mut app::App) {
    key(app, KeyCode::Enter);
    key(app, KeyCode::Esc);
    key(app, KeyCode::Tab);
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
    save_and_open(&mut app);
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
    save_and_open(&mut app);
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

    press(&mut app, "i!");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    assert_eq!(descriptions(&app), ["one", "two", "three!"]);
}

#[test]
fn tab_indents_while_editing() {
    let mut app = temp_app("edit-indent");
    press(&mut app, "aWork");
    save_and_open(&mut app);
    for d in ["one", "two"] {
        press(&mut app, d);
        key(&mut app, KeyCode::Enter);
    }
    // a new item nests under the one above it before it is saved
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.mode, app::Mode::Insert);
    press(&mut app, "sub");
    key(&mut app, KeyCode::Tab);
    assert_eq!(depths(&app)[2].1, 1);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    assert_eq!(depths(&app), [("one".into(), 0), ("two".into(), 0), ("sub".into(), 1)]);

    // an existing item too, and shift+tab moves it back out
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Left);
    press(&mut app, "i!");
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.mode, app::Mode::Insert);
    assert_eq!(depths(&app), [("one".into(), 0), ("two".into(), 1), ("sub".into(), 2)]);
    key(&mut app, KeyCode::BackTab);
    key(&mut app, KeyCode::BackTab);
    assert_eq!(depths(&app), [("one".into(), 0), ("two".into(), 0), ("sub".into(), 1)]);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    assert_eq!(descriptions(&app), ["one", "two!", "sub"]);
}

#[test]
fn arrows_move_between_levels() {
    let mut app = temp_app("levels");
    press(&mut app, "aWork");
    save_and_open(&mut app);
    for d in ["one", "two", "three"] {
        press(&mut app, d);
        key(&mut app, KeyCode::Enter);
    }
    key(&mut app, KeyCode::Esc);
    // nest "two" and a child of it under "one"
    press(&mut app, "k>>Achild");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    assert_eq!(depths(&app), [("one".into(), 0), ("two".into(), 1), ("child".into(), 2), ("three".into(), 0)]);
    assert_eq!(highlighted(&app), "child");

    // left goes to the parent, right into the subtasks
    key(&mut app, KeyCode::Left);
    assert_eq!(highlighted(&app), "two");
    key(&mut app, KeyCode::Left);
    assert_eq!(highlighted(&app), "one");
    // left from the top level goes to the workspaces like tab, right comes back like tab
    key(&mut app, KeyCode::Left);
    assert!(!app.focused_tree().is_todos());
    key(&mut app, KeyCode::Right);
    assert_eq!(app.mode, app::Mode::Insert);
    key(&mut app, KeyCode::Esc);
    press(&mut app, "gg");
    assert_eq!(highlighted(&app), "one");
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Right);
    assert_eq!(highlighted(&app), "child");
    key(&mut app, KeyCode::Right);
    assert_eq!(highlighted(&app), "child");

    // up and down stay on the level, wrapping around it; j and k go row by row
    key(&mut app, KeyCode::Up);
    assert_eq!(highlighted(&app), "child");
    press(&mut app, "j");
    assert_eq!(highlighted(&app), "three");
    key(&mut app, KeyCode::Down);
    assert_eq!(highlighted(&app), "one");
    key(&mut app, KeyCode::Down);
    assert_eq!(highlighted(&app), "three");

    // a collapsed parent opens when going into it
    key(&mut app, KeyCode::Up);
    press(&mut app, "z");
    assert_eq!(descriptions(&app), ["one", "three"]);
    key(&mut app, KeyCode::Right);
    assert_eq!(highlighted(&app), "two");

    // also finishes an edit
    press(&mut app, "ji!");
    key(&mut app, KeyCode::Up);
    assert_eq!(app.mode, app::Mode::Normal);
    assert_eq!(highlighted(&app), "child!");
    assert_eq!(descriptions(&app), ["one", "two", "child!", "three"]);
}

#[test]
fn children_are_shown_on_start() {
    let path = std::env::temp_dir().join(format!("fooit-expanded-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut app = app::App::new(db::Db::open(&path).unwrap());
    press(&mut app, "aWork");
    save_and_open(&mut app);
    press(&mut app, "parent");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    press(&mut app, "Achild");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);

    let mut app = app::App::new(db::Db::open(&path).unwrap());
    press(&mut app, "jj");
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
    save_and_open(&mut app);
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
    save_and_open(&mut app);
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
        (view.area.x + (app::indent(depth) + app::MARKER_WIDTH) as u16, *top)
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
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (b.0 + 2, b.1));
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (b.0 + 2, b.1));
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
    save_and_open(&mut app);
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
        (state.view.area.x + app::MARKER_WIDTH as u16 + 3, *top)
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
    save_and_open(&mut app);
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
    save_and_open(&mut app);
    press(&mut app, "old");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);

    let mut app = app::App::new(db::Db::open(&path).unwrap());
    app.start();
    assert!(app.focused_tree().is_todos());
    assert_eq!(app.mode, app::Mode::Insert);
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Tab);
    assert_eq!(highlighted(&app), "First");
}

#[test]
fn completed_todo_stays_while_the_cursor_is_inside_it() {
    let mut app = temp_app("kept");
    press(&mut app, "aWork");
    save_and_open(&mut app);
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
    save_and_open(&mut app);
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
    press(&mut app, "jj");
    key(&mut app, KeyCode::Tab);
    app.close();
    assert_eq!(descriptions(&app), ["typed"]);
}

#[test]
fn move_mode() {
    let mut app = temp_app("move");
    press(&mut app, "aWork");
    key(&mut app, KeyCode::Enter);
    press(&mut app, "Other");
    save_and_open(&mut app);
    press(&mut app, "existing");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Tab);
    press(&mut app, "k");
    key(&mut app, KeyCode::Tab);
    for d in ["one", "two", "three"] {
        press(&mut app, d);
        key(&mut app, KeyCode::Enter);
    }
    key(&mut app, KeyCode::Esc);

    // up and down shift it, right and left nest it, enter keeps it there
    press(&mut app, "m");
    assert_eq!(app.mode, app::Mode::Move);
    key(&mut app, KeyCode::Up);
    assert_eq!(descriptions(&app), ["one", "three", "two"]);
    key(&mut app, KeyCode::Right);
    assert_eq!(depths(&app), [("one".into(), 0), ("three".into(), 1), ("two".into(), 0)]);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.mode, app::Mode::Normal);
    assert_eq!(highlighted(&app), "three");

    // tab picks a workspace, which the todo moves to straight away
    press(&mut app, "gg");
    press(&mut app, "m");
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.db.description(db::Item::Ws(app.current_ws().unwrap())), "Other");
    key(&mut app, KeyCode::Tab);
    assert_eq!(depths(&app), [("existing".into(), 0), ("one".into(), 0), ("three".into(), 1)]);
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Enter);
    assert!(app.focused_tree().is_todos());
    assert_eq!(descriptions(&app), ["one", "three", "existing"]);

    // left from the top level goes to the workspaces, right comes back
    press(&mut app, "m");
    key(&mut app, KeyCode::Left);
    assert!(!app.focused_tree().is_todos());
    key(&mut app, KeyCode::Right);
    assert!(app.focused_tree().is_todos());
    key(&mut app, KeyCode::Enter);
    assert_eq!(depths(&app), [("one".into(), 0), ("three".into(), 1), ("existing".into(), 0)]);

    // escape puts it back where it was, workspace included
    press(&mut app, "m");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Up);
    assert_eq!(app.db.description(db::Item::Ws(app.current_ws().unwrap())), "Work");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.mode, app::Mode::Normal);
    assert!(app.focused_tree().is_todos());
    assert_eq!(app.db.description(db::Item::Ws(app.current_ws().unwrap())), "Other");
    assert_eq!(descriptions(&app), ["one", "three", "existing"]);
    assert_eq!(highlighted(&app), "one");
}

#[test]
fn new_todos_read_due_dates_and_urgency() {
    let mut app = temp_app("quick-add");
    press(&mut app, "aWork");
    save_and_open(&mut app);
    press(&mut app, "pay rent tomorrow !3");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    let db::Item::Todo(id) = app.tree(app.focused_tree()).unwrap().rows[0].item else { panic!() };
    let todo = app.db.todo(id).unwrap();
    assert_eq!((todo.description.as_str(), todo.urgency), ("pay rent", 3));
    assert_eq!(todo.due.unwrap().date(), dates::now().date() + chrono::Duration::days(1));

    // only new todos: editing keeps the words
    press(&mut app, "i");
    press(&mut app, " tomorrow");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    assert_eq!(descriptions(&app), ["pay rent tomorrow"]);
}

#[test]
fn undo_and_redo() {
    let mut app = temp_app("undo");
    press(&mut app, "aWork");
    save_and_open(&mut app);
    for d in ["one", "two", "three"] {
        press(&mut app, d);
        key(&mut app, KeyCode::Enter);
    }
    key(&mut app, KeyCode::Esc);

    // removing, then undoing it, brings the todo back; enter answers yes
    press(&mut app, "xx");
    key(&mut app, KeyCode::Enter);
    assert_eq!(descriptions(&app), ["one", "two"]);
    press(&mut app, "u");
    assert_eq!(descriptions(&app), ["one", "two", "three"]);
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert_eq!(descriptions(&app), ["one", "two"]);
    press(&mut app, "u");

    // a whole move is one step, and one put back with escape is none
    press(&mut app, "Gm");
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Enter);
    assert_eq!(descriptions(&app), ["three", "one", "two"]);
    press(&mut app, "m");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Esc);
    press(&mut app, "u");
    assert_eq!(descriptions(&app), ["one", "two", "three"]);

    // editing and adding are undone too
    press(&mut app, "i!");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    assert_eq!(descriptions(&app), ["one", "two", "three!"]);
    press(&mut app, "u");
    assert_eq!(descriptions(&app), ["one", "two", "three"]);
    press(&mut app, "u");
    assert_eq!(descriptions(&app), ["one", "two"]);
    press(&mut app, "uuuu");
    assert_eq!(app.tree(app.focused_tree()).map_or(0, |t| t.rows.len()), 0);
}

#[test]
fn dragging_at_the_edge_scrolls() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let mut app = temp_app("drag-scroll");
    let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
    press(&mut app, "aWork");
    save_and_open(&mut app);
    for i in 0..30 {
        press(&mut app, &format!("todo {i}"));
        key(&mut app, KeyCode::Enter);
    }
    key(&mut app, KeyCode::Esc);
    press(&mut app, "gg");
    screen(&mut term, &mut app, "top");

    let area = app.tree(app.focused_tree()).unwrap().view.area;
    let (x, top, bottom) = (area.x + 6, area.y, area.bottom() - 1);
    let mouse = |app: &mut app::App, kind, y| {
        app.on_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
    };
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), top);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), bottom);
    for _ in 0..12 {
        screen(&mut term, &mut app, "dragging");
        std::thread::sleep(std::time::Duration::from_millis(70));
        app.tick();
    }
    let s = screen(&mut term, &mut app, "scrolled");
    assert!(!s.contains("todo 1 ") && s.contains("todo 20"), "{s}");
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), bottom);

    let rows = descriptions(&app);
    let at = rows.iter().position(|d| d == "todo 0").unwrap();
    assert!(at > 15, "{rows:?}");
}

#[test]
fn moving_passes_hidden_completed_todos() {
    let mut app = temp_app("move-hidden");
    press(&mut app, "aWork");
    save_and_open(&mut app);
    for d in ["a", "done", "c"] {
        press(&mut app, d);
        key(&mut app, KeyCode::Enter);
    }
    key(&mut app, KeyCode::Esc);
    press(&mut app, "kcj");
    assert_eq!(descriptions(&app), ["a", "c"]);

    // up passes the hidden todo in one step, in move mode and with K alike
    press(&mut app, "m");
    key(&mut app, KeyCode::Up);
    assert_eq!(descriptions(&app), ["c", "a"]);
    key(&mut app, KeyCode::Down);
    assert_eq!(descriptions(&app), ["a", "c"]);

    // nesting goes under the shown todo above, not the hidden one
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Enter);
    assert_eq!(depths(&app), [("a".into(), 0), ("c".into(), 1)]);
    press(&mut app, "<<K");
    assert_eq!(descriptions(&app), ["c", "a"]);

    // and so does tab on a new todo
    press(&mut app, "Ga");
    key(&mut app, KeyCode::Tab);
    press(&mut app, "new");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    assert_eq!(depths(&app), [("c".into(), 0), ("a".into(), 0), ("new".into(), 1)]);
}

#[test]
fn today_lists_what_is_due_across_workspaces() {
    let mut app = temp_app("today");
    let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
    press(&mut app, "aWork");
    key(&mut app, KeyCode::Enter);
    press(&mut app, "Home");
    save_and_open(&mut app);
    for d in ["laundry today", "plants tomorrow", "no date"] {
        press(&mut app, d);
        key(&mut app, KeyCode::Enter);
    }
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Tab);
    press(&mut app, "k");
    key(&mut app, KeyCode::Tab);
    press(&mut app, "report yesterday");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);

    // Today sits at the top of the workspaces, and opening it adds nothing
    key(&mut app, KeyCode::Tab);
    press(&mut app, "gg");
    let s = screen(&mut term, &mut app, "today row");
    assert!(s.contains("Today") && app.current_ws() == Some(app::TODAY_ID));
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.mode, app::Mode::Normal);
    assert_eq!(descriptions(&app), ["report", "laundry"]);
    let s = screen(&mut term, &mut app, "today");
    assert!(s.contains("[2]─Today") && s.contains("#Work") && s.contains("#Home"), "{s}");

    // things can be done there, but not arranged
    press(&mut app, "a");
    assert_eq!(app.mode, app::Mode::Normal);
    press(&mut app, "c");
    assert_eq!(descriptions(&app), ["report", "laundry"]);
    press(&mut app, "j");
    assert_eq!(descriptions(&app), ["laundry"]);
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
    assert!(s.contains("2020-01-02 10:30") && s.contains("!3") && s.contains("!"));

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

    // recurrence
    press(&mut app, "r1w");
    key(&mut app, KeyCode::Enter);
    let s = screen(&mut term, &mut app, "recurrence");
    assert!(s.contains("↻1w"));

    // search, ignoring case
    press(&mut app, "/sEC");
    assert!(app.tree(app.focused_tree()).unwrap().rows.iter().any(|r| r.enabled && app.db.description(r.item) == "Second todo"));
    for _ in 0..3 {
        key(&mut app, KeyCode::Backspace);
    }
    press(&mut app, "Sec");
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
    assert!(s.contains("Are you sure? [Y/n]"));
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
