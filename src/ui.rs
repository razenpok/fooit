//! Rendering, in the style of lazygit's default theme and layout

use crate::app::{indent, is_draft, layout, row_cells, App, MARKER_WIDTH, TODAY, TODAY_ROW, Bar, Field, Focus, Level, Mode, Row, TreeRef, View};
use crate::db::Item;
use crate::keys::Action;
use chrono::Timelike;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use ratatui::Frame;
use std::collections::HashMap;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// lazygit's default theme, in the terminal's own colors
const ACTIVE_BORDER: Color = Color::Green;
const SEARCHING_BORDER: Color = Color::Cyan;
const OPTIONS: Color = Color::Blue;
const DIM: Color = Color::DarkGray;

/// A lazygit panel: rounded border, green and bold when focused, "[n]─Title" at the top and
/// an optional "x of y" at the bottom right
fn panel(number: Option<u8>, title: &str, border: Option<Color>, counter: Option<String>) -> Block<'static> {
    let border = match border {
        Some(color) => Style::new().fg(color).add_modifier(Modifier::BOLD),
        None => Style::new(),
    };
    let mut spans = vec![];
    if let Some(n) = number {
        spans.push(Span::raw(format!("[{}]", n)));
        spans.push(Span::styled("─", border));
    }
    spans.push(Span::raw(title.to_string()));

    let mut block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Line::from(spans));
    if let Some(counter) = counter {
        block = block.title_bottom(Line::from(vec![Span::styled(counter, border), Span::styled("─", border)]).right_aligned());
    }
    block
}

/// A focused panel drawn over the others, centered in an area
fn popup(buf: &mut Buffer, area: Rect, width: u16, height: u16, title: &str) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let rect = Rect::new(area.x + (area.width - width) / 2, area.y + (area.height - height) / 2, width, height);
    Clear.render(rect, buf);
    let block = panel(None, title, Some(ACTIVE_BORDER), None);
    let inner = block.inner(rect);
    block.render(rect, buf);
    inner
}

fn text_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn display(s: &str) -> String {
    s.replace('\t', " ")
}

/// Word wrap, breaking words longer than the width
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = vec![];
    let mut line = String::new();
    let mut line_width = 0;

    for word in text.split(' ') {
        let word_width = text_width(word);
        let sep = if line.is_empty() { 0 } else { 1 };

        if line_width + sep + word_width <= width {
            if sep == 1 {
                line.push(' ');
            }
            line.push_str(word);
            line_width += sep + word_width;
            continue;
        }

        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }

        for c in word.chars() {
            let cw = c.width().unwrap_or(0);
            if line_width + cw > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                line_width = 0;
            }
            line.push(c);
            line_width += cw;
        }
    }
    lines.push(line);
    lines
}

/// The index of the character shown at a line and column once text is wrapped to a width
pub fn char_at(text: &str, width: usize, line: usize, column: usize) -> usize {
    let chars: Vec<char> = display(text).chars().collect();
    let lines = wrap(&display(text), width);
    let mut start = 0;
    for (i, wrapped) in lines.iter().enumerate() {
        // wrapping drops the spaces a line breaks at, and lines never start with one
        while start < chars.len() && chars[start] == ' ' && !wrapped.starts_with(' ') && !wrapped.is_empty() {
            start += 1;
        }
        let len = wrapped.chars().count();
        if i == line.min(lines.len() - 1) {
            let mut used = 0;
            let mut offset = 0;
            for c in wrapped.chars() {
                let w = c.width().unwrap_or(0);
                if used + w > column {
                    break;
                }
                used += w;
                offset += 1;
            }
            return start + offset;
        }
        start += len;
    }
    chars.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_positions_in_wrapped_text() {
        assert_eq!(wrap("one two  three", 8), ["one two ", "three"]);
        assert_eq!(char_at("one two  three", 8, 1, 1), 10);
        assert_eq!(char_at("one two three", 8, 0, 0), 0);
        assert_eq!(char_at("one two three", 8, 0, 5), 5);
        assert_eq!(char_at("one two three", 8, 0, 30), 7);
        assert_eq!(char_at("one two three", 8, 1, 2), 10);
        assert_eq!(char_at("one two three", 8, 5, 30), 13);
        assert_eq!(char_at("abcdefghij", 4, 2, 1), 9);
        assert_eq!(char_at("", 4, 0, 3), 0);
    }
}

fn truncate(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for c in text.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [main, bar] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, right] = Layout::horizontal([Constraint::Fill(2), Constraint::Fill(8)]).areas(main);

    draw_tree(f.buffer_mut(), app, TreeRef::Workspaces, left);
    match app.current_ws() {
        Some(ws) => draw_tree(f.buffer_mut(), app, TreeRef::Todos(ws), right),
        None => draw_dashboard(f.buffer_mut(), right),
    }
    draw_bar(f.buffer_mut(), app, bar);

    if let Some(scroll) = app.help {
        app.help = Some(draw_help(f.buffer_mut(), main, scroll));
    } else if let Bar::Confirm(confirm) = &app.bar {
        let width = text_width(&confirm.message) as u16 + 4;
        let inner = popup(f.buffer_mut(), main, width.max(40), 3, "Confirm");
        Paragraph::new(confirm.message.as_str()).render(inner, f.buffer_mut());
    }
}

fn draw_dashboard(buf: &mut Buffer, area: Rect) {
    let block = panel(Some(2), "Todos", None, None);
    let inner = block.inner(area);
    block.render(area, buf);

    let lines = ["Welcome to fooit!", "", "Press '?' for the keybindings."];
    for (i, line) in lines.iter().enumerate() {
        if i as u16 >= inner.height {
            break;
        }
        buf.set_stringn(inner.x, inner.y + i as u16, line, inner.width as usize, Style::new());
    }
}

// ----------------------------------------------------------------------
// Trees

fn formatted(app: &App, item: Item, field: Field) -> Vec<Span<'static>> {
    if item == TODAY_ROW {
        return vec![Span::raw("Today")];
    }
    let Item::Todo(id) = item else {
        return vec![Span::raw(display(app.db.description(item)))];
    };
    let bold = |text: String, color: Color| {
        Span::styled(text, Style::new().fg(color).add_modifier(Modifier::BOLD))
    };
    if is_draft(item) && field == Field::Status {
        return vec![Span::raw("○")];
    }
    let Some(todo) = app.db.todo(id) else { return vec![] };
    let done = !todo.pending;

    match field {
        Field::Description if done => vec![Span::styled(display(&todo.description), Style::new().fg(DIM))],
        Field::Description => vec![Span::raw(display(&todo.description))],
        Field::Status => vec![match () {
            _ if done => Span::styled("✓", Style::new().fg(Color::Green)),
            _ if overdue(todo) => bold("!".into(), Color::Red),
            _ => Span::raw("○"),
        }],
        Field::Due => match todo.due {
            None => vec![],
            Some(due) => {
                let now = crate::dates::now();
                let color = match () {
                    _ if done => DIM,
                    _ if overdue(todo) => Color::Red,
                    _ if due.date() == now.date() => Color::Yellow,
                    _ => Color::Reset,
                };
                vec![Span::styled(due_text(due, now), Style::new().fg(color))]
            }
        },
        // the default urgency is not worth a column
        Field::Urgency if done && todo.urgency > 1 => vec![Span::styled(format!("!{}", todo.urgency), Style::new().fg(DIM))],
        Field::Urgency => {
            let color = match todo.urgency {
                ..=1 => return vec![],
                2 => Color::Yellow,
                3 => Color::LightRed,
                _ => Color::Red,
            };
            vec![bold(format!("!{}", todo.urgency), color)]
        }
        Field::Effort => vec![Span::raw(todo.effort.to_string())],
        Field::Recurrence => match todo.recurrence {
            None => vec![],
            Some(r) => {
                let color = if done { DIM } else { Color::Cyan };
                vec![Span::styled(format!("↻{}", crate::dates::recurrence_to_string(r)), Style::new().fg(color))]
            }
        },
    }
}

/// Whether a pending todo is past its due date. One without a time is due by the end of
/// its day, rather than from midnight on as dooit has it
fn overdue(todo: &crate::db::Todo) -> bool {
    let now = crate::dates::now();
    match todo.due {
        _ if !todo.pending => false,
        Some(due) if due.hour() == 0 && due.minute() == 0 => due.date() < now.date(),
        Some(due) => due < now,
        None => false,
    }
}

/// A due date relative to now: today, tomorrow, a weekday within the week, otherwise the date
fn due_text(due: chrono::NaiveDateTime, now: chrono::NaiveDateTime) -> String {
    use chrono::Datelike;
    let days = (due.date() - now.date()).num_days();
    let day = match days {
        0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        -1 => "yesterday".to_string(),
        2..=6 => due.format("%a").to_string().to_lowercase(),
        -6..=-2 => format!("{}d ago", -days),
        _ if due.year() == now.year() => due.format("%b %-d").to_string(),
        _ => due.format("%Y-%m-%d").to_string(),
    };
    if due.hour() != 0 || due.minute() != 0 {
        format!("{} {}", day, due.format("%H:%M"))
    } else {
        day
    }
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| text_width(&s.content)).sum()
}

/// The text shown for a cell: the live input while editing, otherwise the formatted value
fn cell_content(app: &App, tree: TreeRef, item: Item, field: Field) -> Vec<Span<'static>> {
    if let Some(edit) = app.tree(tree).and_then(|t| t.editing()) {
        if edit.item == item && edit.field == field {
            return vec![Span::raw(display(&edit.input.render()))];
        }
    }
    formatted(app, item, field)
}

/// Width one row needs for a field when it is not being edited
fn plain_width(app: &App, item: Item, field: Field) -> usize {
    spans_width(&formatted(app, item, field))
}

/// Column widths shared by every row of a tree, like `ModelTree.get_column_width`, leaving out
/// columns no row has a value for. The per-row widths are cached until the rows or the data
/// change; only the row being edited is re-measured
fn column_widths(app: &mut App, tree: TreeRef, rows: &[Row]) -> Vec<(Field, usize)> {
    let fields = layout(tree);
    let generation = app.db.generation;
    let fresh = matches!(app.tree(tree).and_then(|t| t.widths.as_ref()), Some((g, _)) if *g == generation);
    if !fresh {
        let measured = fields
            .iter()
            .map(|f| match f {
                Field::Description => vec![],
                _ => rows.iter().map(|r| plain_width(app, r.item, *f)).collect(),
            })
            .collect();
        app.tree_mut(tree).widths = Some((generation, measured));
    }

    let app: &App = app;
    let state = app.tree(tree).unwrap();
    let editing = state.editing().map(|e| (e.item, e.field, e.input.render()));
    let cached = &state.widths.as_ref().unwrap().1;
    fields
        .iter()
        .zip(cached)
        .map(|(field, per_row)| {
            if *field == Field::Description {
                return (*field, 0);
            }
            let width = rows
                .iter()
                .zip(per_row)
                .map(|(row, width)| match &editing {
                    Some((item, f, value)) if *item == row.item && f == field => text_width(value) + 1,
                    _ => *width,
                })
                .max()
                .unwrap_or(0);
            (*field, width)
        })
        .filter(|(field, width)| *field == Field::Description || *width > 0)
        .collect()
}

fn build_row(app: &App, tree: TreeRef, row: &Row, widths: &[(Field, usize)], total: usize) -> Vec<Line<'static>> {
    let cells = row_cells(widths, row.depth, total);
    let editing = app.tree(tree).and_then(|t| t.editing()).is_some_and(|e| e.item == row.item);
    let dim = Style::new().fg(DIM);

    // a dim note after the description: done and all children of a todo, open todos of a workspace
    let note = match row.item {
        _ if editing || is_draft(row.item) => None,
        // in Today, the workspace each todo comes from
        Item::Todo(_) if tree == TODAY => app.workspace_of(row.item).map(|ws| format!("#{}", app.db.description(ws))),
        _ if row.item == TODAY_ROW => Some(app.due_today()).filter(|due| *due > 0).map(|due| due.to_string()),
        Item::Todo(_) => Some(app.progress(row.item)).filter(|(_, all)| *all > 0).map(|(done, all)| format!("{}/{}", done, all)),
        Item::Ws(_) => Some(app.open_todos(row.item)).filter(|open| *open > 0).map(|open| open.to_string()),
    };

    let columns: Vec<Vec<Vec<Span<'static>>>> = cells
        .iter()
        .map(|(field, _, width)| {
            let spans = cell_content(app, tree, row.item, *field);
            if *field != Field::Description {
                return vec![spans.into_iter().map(|s| Span::styled(truncate(&s.content, *width), s.style)).collect()];
            }

            let style = spans.first().map(|s| s.style).unwrap_or_default();
            let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
            match &note {
                // a workspace's count sits at the right edge of its first line
                Some(note) if !tree.is_todos() => {
                    let note_width = text_width(note);
                    let mut lines: Vec<Vec<Span>> = wrap(&text, width.saturating_sub(note_width + 1).max(1))
                        .into_iter()
                        .map(|l| vec![Span::styled(l, style)])
                        .collect();
                    let pad = width.saturating_sub(spans_width(&lines[0]) + note_width);
                    lines[0].push(Span::raw(" ".repeat(pad)));
                    lines[0].push(Span::styled(note.clone(), dim));
                    lines
                }
                _ => {
                    let mut lines: Vec<Vec<Span>> =
                        wrap(&text, *width).into_iter().map(|l| vec![Span::styled(l, style)]).collect();
                    if let Some(note) = &note {
                        let last = lines.last_mut().unwrap();
                        if spans_width(last) + 1 + text_width(note) <= *width {
                            last.push(Span::styled(format!(" {}", note), dim));
                        } else {
                            lines.push(vec![Span::styled(note.clone(), dim)]);
                        }
                    }
                    lines
                }
            }
        })
        .collect();

    let marker = match () {
        _ if tree == TODAY || !app.has_children(row.item) => "",
        _ if app.collapsed.contains(&row.item) => "▸",
        _ => "▾",
    };

    let height = columns.iter().map(|c| c.len()).max().unwrap_or(1);
    (0..height)
        .map(|i| {
            let mut spans = vec![Span::raw(" ".repeat(indent(row.depth)))];
            let marker = if i == 0 { marker } else { "" };
            spans.push(Span::styled(format!("{:<width$}", marker, width = MARKER_WIDTH), dim));
            for (lines, (_, _, width)) in columns.iter().zip(&cells) {
                let content = lines.get(i).cloned().unwrap_or_default();
                let pad = width.saturating_sub(spans_width(&content));
                spans.extend(content);
                spans.push(Span::raw(" ".repeat(pad + 1)));
            }
            Line::from(spans)
        })
        .collect()
}

fn draw_tree(buf: &mut Buffer, app: &mut App, tree: TreeRef, area: Rect) {
    let focused = app.focused_tree() == tree
        && match tree {
            TreeRef::Workspaces => app.focus == Focus::Workspaces,
            TreeRef::Todos(_) => app.focus == Focus::Todos,
        };

    let (number, title) = match tree {
        TODAY => (2, "Today"),
        TreeRef::Todos(_) => (2, "Todos"),
        TreeRef::Workspaces => (1, "Workspaces"),
    };
    let searching = matches!(app.bar, Bar::Search { tree: t, .. } if t == tree);
    let border = match () {
        _ if searching => Some(SEARCHING_BORDER),
        _ if focused => Some(ACTIVE_BORDER),
        _ => None,
    };
    let state = app.tree_mut(tree);
    let counter = format!("{} of {}", state.highlighted.map_or(0, |h| h + 1), state.rows.len());
    let block = panel(Some(number), title, border, Some(counter));
    let inner = block.inner(area);
    block.render(area, buf);

    let rows = app.tree_mut(tree).rows.clone();
    app.tree_mut(tree).view = View { area: inner, ..View::default() };
    if rows.is_empty() {
        return;
    }

    let widths = column_widths(app, tree, &rows);

    // only the rows that end up on screen are built
    let total = inner.width as usize;
    let mut built: HashMap<usize, Vec<Line<'static>>> = HashMap::new();
    let mut lines_of = |app: &App, index: usize| -> Vec<Line<'static>> {
        built
            .entry(index)
            .or_insert_with(|| build_row(app, tree, &rows[index], &widths, total))
            .clone()
    };

    // keep the highlighted option in view: scroll down just enough for it to fit
    let height = inner.height as usize;
    let state = app.tree(tree).unwrap();
    let highlighted = state.highlighted;
    let mut offset = state.offset.min(rows.len().saturating_sub(1));
    // while dragging the list scrolls on its own, away from the dragged row
    let dragging = app.drag.as_ref().is_some_and(|d| d.tree == tree && d.moved);
    if let Some(h) = highlighted.filter(|_| !dragging) {
        if h < offset {
            offset = h;
        }
        let mut start = h;
        let mut used = lines_of(app, h).len();
        while start > offset {
            let above = lines_of(app, start - 1).len();
            if used + above > height {
                break;
            }
            used += above;
            start -= 1;
        }
        offset = start;
    }
    app.tree_mut(tree).offset = offset;

    // the rows being dragged are dimmed
    let drag = app.drag.as_ref().filter(|d| d.tree == tree);
    let dragged = drag.and_then(|d| {
        let from = rows.iter().position(|r| r.item == d.item)?;
        let len = rows[from + 1..].iter().take_while(|r| r.depth > rows[from].depth).count();
        Some(from..from + 1 + len)
    });
    let target = drag.and_then(|d| d.target);
    // a workspace a dragged todo would move to
    let onto = app.drag.as_ref().and_then(|d| d.onto).filter(|_| tree == TreeRef::Workspaces);

    let mut shown = vec![];
    let mut y = inner.y;
    'rows: for index in offset..rows.len() {
        if y >= inner.bottom() {
            break;
        }
        let lines = lines_of(app, index);
        let row = &rows[index];
        let dimmed = !row.enabled || dragged.as_ref().is_some_and(|d| d.contains(&index));
        let mut style = Style::new();
        if dimmed {
            style = style.fg(DIM);
        }
        // the selected line reads in any theme: reversed, in plain colors
        let moving = app.moving.is_some_and(|m| m.item == row.item);
        let selected = (highlighted == Some(index) && focused) || onto == Some(row.item) || moving;
        if selected {
            style = Style::new().add_modifier(Modifier::REVERSED);
        } else if highlighted == Some(index) {
            style = style.add_modifier(Modifier::BOLD);
        }
        if onto == Some(row.item) {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(Color::Red);
        }

        let top = y;
        for line in lines {
            if y >= inner.bottom() {
                shown.push((index, top, y - top));
                break 'rows;
            }
            let rect = Rect::new(inner.x, y, inner.width, 1);
            buf.set_style(rect, style);
            let mut line = line.clone();
            for span in &mut line.spans {
                if selected {
                    span.style = Style::new();
                } else if dimmed {
                    span.style = span.style.fg(DIM);
                }
            }
            buf.set_line(inner.x, y, &line, inner.width);
            y += 1;
        }
        shown.push((index, top, y - top));
    }

    // where a dragged row would land: a line under the row above it, at the depth it would get
    if let Some(target) = target {
        let line_y = match target.above {
            Some(above) => shown.iter().find(|(i, _, _)| *i == above).map(|(_, top, h)| top + h - 1),
            None => None,
        }
        .unwrap_or(inner.y.saturating_sub(1));
        let x = inner.x + (indent(target.depth) as u16).min(inner.width.saturating_sub(1));
        let rect = Rect::new(x, line_y, inner.right() - x, 1);
        buf.set_style(rect, Style::new().add_modifier(Modifier::UNDERLINED).underline_color(Color::Red));
    }

    let view = &mut app.tree_mut(tree).view;
    view.rows = shown;
    view.columns = widths;
}

// ----------------------------------------------------------------------
// Bars

/// lazygit's "Action: key | Action: key" options line
fn options(pairs: &[(&str, &str)]) -> Line<'static> {
    let text = pairs.iter().map(|(action, key)| format!("{}: {}", action, key)).collect::<Vec<_>>().join(" | ");
    Line::styled(text, Style::new().fg(OPTIONS))
}

fn draw_bar(buf: &mut Buffer, app: &App, area: Rect) {
    let line = match &app.bar {
        _ if app.help.is_some() => options(&[("Close", "esc"), ("Scroll", "j/k")]),
        Bar::Status if app.mode == Mode::Move && app.focus == Focus::Workspaces => options(&[
            ("Move to workspace", "up/down"),
            ("Back to todos", "tab/right"),
            ("Done", "enter"),
            ("Put back", "esc"),
        ]),
        Bar::Status if app.mode == Mode::Move => options(&[
            ("Move", "up/down"),
            ("Indent", "right/left"),
            ("Workspace", "tab"),
            ("Done", "enter"),
            ("Put back", "esc"),
        ]),
        Bar::Status if app.mode == Mode::Insert => {
            options(&[("Save", "enter"), ("Discard", "esc"), ("Save and move", "up/down"), ("Indent", "tab/shift+tab")])
        }
        Bar::Status if app.focus == Focus::Todos => options(&[
            ("Add", "a"),
            ("Add child", "A"),
            ("Edit", "i"),
            ("Complete", "c"),
            ("Move", "m"),
            ("Remove", "xx"),
            ("Undo", "u"),
            ("Show completed", "H"),
            ("Keybindings", "?"),
        ]),
        Bar::Status => options(&[("Add", "a"), ("Edit", "i"), ("Remove", "xx"), ("Todos", "tab"), ("Keybindings", "?")]),
        Bar::Notification { message, level, .. } => {
            let color = match level {
                Level::Info => Color::Green,
                Level::Warning => Color::Yellow,
                Level::Error => Color::Red,
            };
            Line::styled(message.clone(), Style::new().fg(color))
        }
        Bar::Search { input, .. } => Line::from(vec![
            Span::styled("Search: ", Style::new().fg(SEARCHING_BORDER)),
            Span::raw(display(&input.draw().chars().skip(1).collect::<String>())),
        ]),
        Bar::Sort { options, selected, .. } => {
            let mut spans = vec![Span::styled("Sort by:", Style::new().fg(OPTIONS))];
            for (i, option) in options.iter().enumerate() {
                let style = if i == *selected { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                spans.push(Span::raw(" "));
                spans.push(Span::styled(format!(" {} ", option.name()), style));
            }
            Line::from(spans)
        }
        Bar::Confirm(confirm) if confirm.default_yes => options(&[("Confirm", "y/enter"), ("Cancel", "n/esc")]),
        Bar::Confirm(_) => options(&[("Confirm", "y"), ("Cancel", "n/enter")]),
    };
    buf.set_line(area.x, area.y, &line, area.width);

    let version = format!("fooit {}", env!("CARGO_PKG_VERSION"));
    let width = text_width(&version) as u16;
    if line.width() as u16 + width + 1 < area.width {
        buf.set_string(area.right() - width, area.y, version, Style::new());
    }
}

// ----------------------------------------------------------------------
// Help screen

/// The bindings with a description, keys for the same action joined on one line
fn help_bindings() -> Vec<(String, Action)> {
    let mut grouped: Vec<(String, Action)> = vec![];
    for (key, action) in crate::keys::KeyManager::with_defaults().bindings {
        if action.description().is_empty() {
            continue;
        }
        match grouped.iter_mut().find(|(_, a)| *a == action) {
            Some((keys, _)) => *keys = format!("{}, {}", keys, key),
            None => grouped.push((key, action)),
        }
    }
    grouped
}

/// Draws the keybindings popup and returns the clamped scroll offset
fn draw_help(buf: &mut Buffer, area: Rect, scroll: u16) -> u16 {
    let bindings = help_bindings();
    let key_w = bindings.iter().map(|(k, _)| text_width(k)).max().unwrap_or(1);
    let desc_w = bindings.iter().map(|(_, a)| text_width(a.description())).max().unwrap_or(1);

    let width = (key_w + 2 + desc_w + 2) as u16;
    let inner = popup(buf, area, width, bindings.len() as u16 + 2, "Keybindings");

    let max_scroll = (bindings.len() as u16).saturating_sub(inner.height);
    let scroll = scroll.min(max_scroll);
    for (i, (key, action)) in bindings.iter().skip(scroll as usize).take(inner.height as usize).enumerate() {
        let line = Line::from(vec![
            Span::styled(format!("{:<width$}", key, width = key_w + 2), Style::new().fg(OPTIONS)),
            Span::raw(action.description()),
        ]);
        buf.set_line(inner.x, inner.y + i as u16, &line, inner.width);
    }
    scroll
}
