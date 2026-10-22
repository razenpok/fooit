//! Timings on a seeded 10k-todo database. Run with
//! `cargo test --release bench -- --ignored --nocapture`

use crate::{app, dates, db, ui};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const TOTAL: i64 = 10_000;

/// Seeds `workspaces` workspaces, each holding an even share of TOTAL todos where every
/// top-level todo has `children` children
fn seed(path: &Path, workspaces: i64, children: i64) {
    let _ = std::fs::remove_file(path);
    drop(db::Db::open(path).unwrap()); // creates the tables

    let mut conn = Connection::open(path).unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO workspace (id, order_index, description, is_root, parent_workspace_id) VALUES (1, 0, '', 1, NULL)",
        [],
    )
    .unwrap();
    let due = dates::datetime_to_sql(dates::now());
    let per_ws = TOTAL / workspaces;
    let mut id = 0;
    for w in 0..workspaces {
        let ws_id = w + 2;
        tx.execute(
            "INSERT INTO workspace (id, order_index, description, is_root, parent_workspace_id) VALUES (?1, ?2, ?3, 0, 1)",
            params![ws_id, w, format!("Workspace {w}")],
        )
        .unwrap();
        let mut stmt = tx
            .prepare(
                "INSERT INTO todo (id, order_index, description, due, effort, recurrence, urgency, pending, \
                 parent_workspace_id, parent_todo_id) VALUES (?1, ?2, ?3, ?4, 0, NULL, ?5, ?6, ?7, ?8)",
            )
            .unwrap();
        let mut top = 0;
        let mut made = 0;
        while made < per_ws {
            id += 1;
            let parent = id;
            stmt.execute(params![
                id,
                top,
                format!("Todo {id} with a reasonably long description"),
                (id % 3 == 0).then(|| due.clone()),
                id % 4 + 1,
                id % 5 != 0,
                ws_id,
                None::<i64>
            ])
            .unwrap();
            made += 1;
            top += 1;
            for c in 0..children.min(per_ws - made) {
                id += 1;
                stmt.execute(params![id, c, format!("Child {id}"), None::<String>, 1, true, None::<i64>, parent])
                    .unwrap();
                made += 1;
            }
        }
    }
    tx.commit().unwrap();
}

fn time<T>(n: usize, mut f: impl FnMut() -> T) -> (Duration, Duration) {
    let mut samples: Vec<Duration> = (0..n)
        .map(|_| {
            let start = Instant::now();
            std::hint::black_box(f());
            start.elapsed()
        })
        .collect();
    samples.sort();
    (samples[n / 2], samples[0])
}

fn report(label: &str, (median, min): (Duration, Duration)) {
    println!("  {label:<42} median {median:>10.2?}   min {min:>10.2?}");
}

fn press(app: &mut app::App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn run(name: &str, workspaces: i64, children: i64) {
    let path: PathBuf = std::env::temp_dir().join(format!("fooit-bench-{name}-{}.db", std::process::id()));
    seed(&path, workspaces, children);
    println!("\n{name}: {workspaces} workspace(s), {TOTAL} todos, {children} children per top-level todo");

    report("Db::open (startup load)", time(20, || db::Db::open(&path).unwrap()));
    report("Db::open + App::new", time(20, || app::App::new(db::Db::open(&path).unwrap())));

    let mut d = db::Db::open(&path).unwrap();
    report("Db::load (the reload after every write)", time(50, || d.load().unwrap()));
    let first = *d.todos.keys().min().unwrap();
    let mut i = 0;
    report(
        "Db::set_description (write + reload)",
        time(50, || {
            i += 1;
            d.set_description(db::Item::Todo(first), &format!("renamed {i}")).unwrap()
        }),
    );
    drop(d);

    let mut app = app::App::new(db::Db::open(&path).unwrap());
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    press(&mut app, KeyCode::Char('j')); // highlight Today
    press(&mut app, KeyCode::Char('j')); // and then the first workspace
    press(&mut app, KeyCode::Tab); // enter its todos (starts a new todo)
    press(&mut app, KeyCode::Esc); // and drop it again
    let rows = app.tree(app.focused_tree()).map_or(0, |t| t.rows.len());
    println!("  (todo pane shows {rows} rows)");

    report("draw one frame (120x40)", time(50, || term.draw(|f| ui::draw(f, &mut app)).unwrap().area));
    report("key j (move down)", time(50, || press(&mut app, KeyCode::Char('j'))));
    let mut up = false;
    report(
        "key +/- (urgency write + reload + rebuild)",
        time(50, || {
            up = !up;
            press(&mut app, KeyCode::Char(if up { '+' } else { '-' }))
        }),
    );
    report(
        "key +/- then draw (widths re-measured)",
        time(50, || {
            up = !up;
            press(&mut app, KeyCode::Char(if up { '+' } else { '-' }));
            term.draw(|f| ui::draw(f, &mut app)).unwrap().area
        }),
    );
    report("App::tick with no change (mtime poll)", time(50, || app.tick()));

    let _ = std::fs::remove_file(&path);
}

fn screen(term: &mut Terminal<TestBackend>, app: &mut app::App) -> String {
    term.draw(|f| ui::draw(f, app)).unwrap();
    let buf = term.backend().buffer();
    buf.content().iter().map(|c| c.symbol()).collect()
}

/// Viewport-only drawing still scrolls the highlight into view, including wrapped rows
#[test]
fn scrolls_with_wrapped_rows() {
    let path = std::env::temp_dir().join(format!("fooit-viewport-{}.db", std::process::id()));
    seed(&path, 50, 0); // 200 todos in each workspace
    let mut app = app::App::new(db::Db::open(&path).unwrap());
    app.show_completed = true;
    let mut term = Terminal::new(TestBackend::new(52, 12)).unwrap(); // narrow, so descriptions wrap
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Esc);

    press(&mut app, KeyCode::Char('G'));
    let s = screen(&mut term, &mut app);
    assert!(s.contains("Todo 200 "), "{s}");
    for _ in 0..3 {
        press(&mut app, KeyCode::Char('k'));
    }
    let s = screen(&mut term, &mut app);
    assert!(s.contains("Todo 197 ") && !s.contains("Todo 200 "), "{s}"); // each row wraps to 4 of the 9 lines
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('g'));
    let s = screen(&mut term, &mut app);
    assert!(s.contains("Todo 1 ") && !s.contains("Todo 197 "), "{s}");
    let _ = std::fs::remove_file(&path);
}

#[test]
#[ignore]
fn bench() {
    run("flat", 1, 0);
    run("nested", 10, 3);
}
