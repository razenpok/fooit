//! Rendering, matching dooit's default theme and layout

use crate::app::{indent, is_draft, layout, row_cells, App, Bar, Field, Focus, Level, Mode, Row, TreeRef, View};
use crate::db::Item;
use crate::keys::Action;
use chrono::Timelike;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph, Widget};
use ratatui::Frame;
use std::collections::HashMap;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const fn hex(v: u32) -> Color {
    Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const BG1: Color = hex(0x2E3440);
const BG2: Color = hex(0x3B4252);
const BG3: Color = hex(0x434C5E);
const FG1: Color = hex(0xD8DEE9);
const FG3: Color = hex(0xECEFF4);
const RED: Color = hex(0xBF616A);
const ORANGE: Color = hex(0xD08770);
const YELLOW: Color = hex(0xEBCB8B);
const GREEN: Color = hex(0xA3BE8C);
const BLUE: Color = hex(0x81A1C1);
const PURPLE: Color = hex(0xB48EAD);
const CYAN: Color = hex(0x8FBCBB);
const PRIMARY: Color = CYAN;
const SECONDARY: Color = BLUE;

// pre-blended translucent colors from dooit's stylesheet
const HIGHLIGHT_FOCUSED: Color = hex(0x4F6074); // $secondary 40% over $background1
const DISABLED: Color = hex(0x838994); // $foreground1 50%
const MUTED: Color = hex(0x8D929A); // $foreground3 50%
const ARROW: Color = hex(0x9A9FAA); // $foreground1 60% over $background2

/// textual's "blank" border
const BLANK: border::Set = border::Set {
    top_left: " ",
    top_right: " ",
    bottom_left: " ",
    bottom_right: " ",
    vertical_left: " ",
    vertical_right: " ",
    horizontal_top: " ",
    horizontal_bottom: " ",
};

/// textual's "tall" border
const TALL: border::Set = border::Set {
    top_left: "▊",
    top_right: "▎",
    bottom_left: "▊",
    bottom_right: "▎",
    vertical_left: "▊",
    vertical_right: "▎",
    horizontal_top: "▔",
    horizontal_bottom: "▁",
};

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
    Block::new().style(Style::new().bg(BG1).fg(FG3)).render(area, f.buffer_mut());

    if let Some(scroll) = app.help {
        app.help = Some(draw_help(f.buffer_mut(), area, scroll));
        return;
    }

    let [main, bar] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [left, right] = Layout::horizontal([Constraint::Fill(2), Constraint::Fill(8)]).areas(main);

    draw_tree(f.buffer_mut(), app, TreeRef::Workspaces, left);
    match app.current_ws() {
        Some(ws) => draw_tree(f.buffer_mut(), app, TreeRef::Todos(ws), right),
        None => draw_dashboard(f.buffer_mut(), right),
    }
    draw_bar(f.buffer_mut(), app, bar);
}

fn draw_dashboard(buf: &mut Buffer, area: Rect) {
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::new().fg(BG3))
        .style(Style::new().bg(BG1));
    let inner = block.inner(area);
    block.render(area, buf);

    let lines = ["Welcome to fooit!", "", "If you're stuck, press '?' for help."];
    let top = inner.y + inner.height.saturating_sub(lines.len() as u16) / 2;
    for (i, line) in lines.iter().enumerate() {
        let y = top + i as u16;
        if y >= inner.bottom() {
            break;
        }
        Paragraph::new(*line)
            .centered()
            .style(Style::new().fg(FG3))
            .render(Rect::new(inner.x, y, inner.width, 1), buf);
    }
}

// ----------------------------------------------------------------------
// Trees

struct Cell {
    lines: Vec<Vec<Span<'static>>>,
    width: usize,
    center: bool,
}

fn formatted(app: &App, item: Item, field: Field) -> Vec<Span<'static>> {
    let Item::Todo(id) = item else {
        return vec![Span::raw(display(app.db.description(item)))];
    };
    let bold = |text: String, color: Color| {
        Span::styled(text, Style::new().fg(color).add_modifier(Modifier::BOLD))
    };
    if is_draft(item) && field == Field::Status {
        return vec![bold("o".into(), YELLOW)];
    }
    let Some(todo) = app.db.todo(id) else { return vec![] };

    match field {
        Field::Description => vec![Span::raw(display(&todo.description))],
        Field::Status => vec![match todo.status() {
            "completed" => bold("x".into(), GREEN),
            "overdue" => bold("!".into(), RED),
            _ => bold("o".into(), YELLOW),
        }],
        Field::Due => match todo.due {
            None => vec![],
            Some(due) => {
                let mut text = due.format("%Y-%m-%d").to_string();
                if due.hour() != 0 {
                    text += &due.format(" (%H:%M)").to_string();
                }
                vec![Span::raw(text)]
            }
        },
        Field::Urgency => {
            let color = match todo.urgency {
                0 => return vec![],
                1 => GREEN,
                2 => YELLOW,
                3 => ORANGE,
                4 => RED,
                _ => PRIMARY,
            };
            vec![bold(format!("!{}", todo.urgency), color)]
        }
        Field::Effort => vec![Span::raw(todo.effort.to_string())],
        Field::Recurrence => match todo.recurrence {
            None => vec![],
            Some(r) => vec![Span::raw(crate::dates::recurrence_to_string(r))],
        },
    }
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| text_width(&s.content)).sum()
}

/// The text shown for a cell: the live input while editing, otherwise the formatted value
fn cell_content(app: &App, tree: TreeRef, item: Item, field: Field) -> (Vec<Span<'static>>, bool) {
    if let Some(edit) = app.tree(tree).and_then(|t| t.editing()) {
        if edit.item == item && edit.field == field {
            return (vec![Span::raw(display(&edit.input.render()))], false);
        }
    }

    let spans = formatted(app, item, field);
    if spans_width(&spans) == 0 {
        // empty values render as a dim, centered dash
        return (vec![Span::styled("-", Style::new().add_modifier(Modifier::DIM))], true);
    }
    (spans, false)
}

/// Width one row needs for a field when it is not being edited (empty values show a 1-wide dash)
fn plain_width(app: &App, item: Item, field: Field) -> usize {
    let rendered = spans_width(&formatted(app, item, field)).max(1);
    (text_width(&app.edit_default(item, field)) + 1).max(rendered)
}

/// Column widths shared by every row of a tree, like `ModelTree.get_column_width`. The per-row
/// widths are cached until the rows or the data change; only the row being edited is re-measured
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
                    Some((item, f, value)) if *item == row.item && f == field => {
                        let rendered = spans_width(&cell_content(app, tree, row.item, *field).0);
                        (text_width(value) + 1).max(rendered)
                    }
                    _ => *width,
                })
                .max()
                .unwrap_or(1);
            (*field, width)
        })
        .collect()
}

fn build_row(app: &App, tree: TreeRef, row: &Row, widths: &[(Field, usize)], total: usize) -> Vec<Line<'static>> {
    let description_width = row_cells(widths, row.depth, total)
        .iter()
        .find(|(f, _, _)| *f == Field::Description)
        .map_or(1, |(_, _, w)| *w);

    let mut cells = vec![];
    if row.depth > 0 {
        cells.push(Cell { lines: vec![vec![]], width: 2 * row.depth, center: false });
    }

    for (field, width) in widths {
        let (spans, center) = cell_content(app, tree, row.item, *field);
        if *field == Field::Description {
            let style = spans.first().map(|s| s.style).unwrap_or_default();
            let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
            let lines = wrap(&text, description_width)
                .into_iter()
                .map(|l| vec![Span::styled(l, style)])
                .collect();
            cells.push(Cell { lines, width: description_width, center });
        } else {
            let spans = spans
                .into_iter()
                .map(|s| Span::styled(truncate(&s.content, *width), s.style))
                .collect();
            cells.push(Cell { lines: vec![spans], width: *width, center });
        }
    }

    let height = cells.iter().map(|c| c.lines.len()).max().unwrap_or(1);
    (0..height)
        .map(|i| {
            let mut spans = vec![];
            for cell in &cells {
                spans.push(Span::raw(" "));
                let content = cell.lines.get(i).cloned().unwrap_or_default();
                let content_width = spans_width(&content);
                let pad = cell.width.saturating_sub(content_width);
                let (left, right) = if cell.center { (pad / 2, pad - pad / 2) } else { (0, pad) };
                spans.push(Span::raw(" ".repeat(left)));
                spans.extend(content);
                spans.push(Span::raw(" ".repeat(right)));
                spans.push(Span::raw(" "));
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

    let title = if tree.is_todos() { " Todos " } else { " Workspaces " };
    let (border_color, title_style) = if focused {
        (PRIMARY, Style::new().fg(BG1).bg(PRIMARY))
    } else {
        (BG3, Style::new().fg(FG1).bg(BG3))
    };

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::new().fg(border_color))
        .title(Span::styled(title, title_style))
        .padding(Padding::uniform(1))
        .style(Style::new().bg(BG1).fg(FG3));
    let inner = block.inner(area);
    block.render(area, buf);

    let rows = app.tree_mut(tree).rows.clone();
    app.tree_mut(tree).view = View { area: inner, ..View::default() };
    if rows.is_empty() {
        let y = inner.y + inner.height / 2;
        if inner.height > 0 {
            Paragraph::new("No items to display")
                .centered()
                .style(Style::new().fg(MUTED))
                .render(Rect::new(inner.x, y, inner.width, 1), buf);
        }
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
    if let Some(h) = highlighted {
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
        let mut style = Style::new().bg(BG1).fg(FG3);
        if dimmed {
            style = style.fg(DISABLED);
        }
        if highlighted == Some(index) {
            style = style.bg(if focused { HIGHLIGHT_FOCUSED } else { BG3 });
        }
        if onto == Some(row.item) {
            style = style.bg(HIGHLIGHT_FOCUSED).add_modifier(Modifier::UNDERLINED).underline_color(RED);
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
            if dimmed {
                for span in &mut line.spans {
                    span.style = span.style.fg(DISABLED);
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
        buf.set_style(rect, Style::new().add_modifier(Modifier::UNDERLINED).underline_color(RED));
    }

    let view = &mut app.tree_mut(tree).view;
    view.rows = shown;
    view.columns = widths;
}

// ----------------------------------------------------------------------
// Bars

fn draw_bar(buf: &mut Buffer, app: &App, area: Rect) {
    let base = Style::new().bg(BG2).fg(FG3);
    buf.set_style(area, base);

    match &app.bar {
        Bar::Status => {
            let mode_bg = match app.mode {
                Mode::Insert => SECONDARY,
                _ => PRIMARY,
            };
            let segment = |text: String, bg: Color| Span::styled(text, Style::new().fg(BG1).bg(bg));

            let mode = segment(format!(" {} ", app.mode.name()), mode_bg);
            let clock = segment(format!(" {} ", chrono::Local::now().format("%H:%M:%S")), SECONDARY);
            let user = segment(format!(" {} ", app.username), SECONDARY);

            buf.set_span(area.x, area.y, &mode, area.width);
            let right_width = (text_width(&clock.content) + 1 + text_width(&user.content)) as u16;
            let x = area.right().saturating_sub(right_width).max(area.x);
            let line = Line::from(vec![clock, Span::raw(" "), user]);
            buf.set_line(x, area.y, &line, area.right() - x);
        }
        Bar::Notification { message, level, .. } => {
            let bg = match level {
                Level::Info => PRIMARY,
                Level::Warning => YELLOW,
                Level::Error => RED,
            };
            let style = Style::new().bg(bg).fg(BG1);
            buf.set_style(area, style);
            buf.set_stringn(area.x + 1, area.y, message, area.width.saturating_sub(2) as usize, style);
        }
        Bar::Search { input, .. } => {
            buf.set_stringn(area.x, area.y, display(&input.draw()), area.width as usize, base);
        }
        Bar::Sort { options, selected, .. } => {
            let spans: Vec<Span> = options
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    let style = if i == *selected { Style::new().bg(SECONDARY) } else { Style::new() };
                    Span::styled(format!(" {} ", o.name()), style)
                })
                .collect();
            buf.set_line(area.x, area.y, &Line::from(spans), area.width);
        }
        Bar::Confirm(confirm) => {
            let style = Style::new().bg(RED).fg(BG1).add_modifier(Modifier::BOLD);
            buf.set_style(area, style);
            buf.set_stringn(area.x + 1, area.y, &confirm.message, area.width.saturating_sub(2) as usize, style);
        }
    }
}

// ----------------------------------------------------------------------
// Help screen

fn help_bindings() -> Vec<(String, Action)> {
    crate::keys::KeyManager::with_defaults().bindings
}

/// Draws the help screen and returns the clamped scroll offset
fn draw_help(buf: &mut Buffer, area: Rect, scroll: u16) -> u16 {
    let width = (area.width as u32 * 8 / 10) as u16;
    let x = area.x + (area.width - width) / 2;

    let bindings = help_bindings();
    let table_inner_width = width.saturating_sub(2 + 4) as usize;

    // rich's expanding grid: natural widths, extra space shared proportionally
    let key_w = bindings.iter().map(|(k, _)| text_width(k)).max().unwrap_or(1);
    let desc_w = bindings.iter().map(|(_, a)| text_width(a.description())).max().unwrap_or(1);
    let natural = key_w + 2 + desc_w + 2;
    let extra = table_inner_width.saturating_sub(natural);
    let total_cols = (key_w + 2 + desc_w).max(1);
    let key_col = key_w + extra * key_w / total_cols;
    let arrow_col = 2 + extra * 2 / total_cols;

    let mut table_lines: Vec<Line> = vec![Line::raw("")];
    for (key, action) in &bindings {
        table_lines.push(Line::from(vec![
            Span::styled(format!("{:<width$}", key, width = key_col), Style::new().fg(PRIMARY)),
            Span::raw(" "),
            Span::styled(format!("{:<width$}", "->", width = arrow_col), Style::new().fg(ARROW)),
            Span::raw(" "),
            Span::styled(
                action.description().to_string(),
                Style::new().fg(SECONDARY).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    table_lines.push(Line::raw(""));

    let header_h = 3u16;
    let table_h = table_lines.len() as u16 + 4;
    let outro: Vec<Line> = vec![
        Line::styled("     Thanks for using fooit <3", Style::new().fg(PURPLE)),
        Line::raw(""),
        Line::from(vec![
            Span::raw("     Use "),
            Span::styled(" escape ", Style::new().bg(PRIMARY).fg(BG1)),
            Span::raw(" to go back"),
        ]),
    ];
    let outro_h = outro.len() as u16;
    let total_h = 1 + header_h + 1 + table_h + 1 + outro_h + 1;

    let mut canvas = Buffer::empty(Rect::new(0, 0, area.width, total_h));
    canvas.set_style(canvas.area, Style::new().bg(BG1).fg(FG3));

    // header
    let header = Rect::new(x - area.x, 1, width, header_h);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_set(TALL)
        .border_style(Style::new().fg(BG3));
    let inner = block.inner(header);
    block.render(header, &mut canvas);
    Paragraph::new("Welcome to fooit!").centered().render(inner, &mut canvas);

    // key table
    let table = Rect::new(x - area.x, 1 + header_h + 1, width, table_h);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_set(BLANK)
        .border_style(Style::new().bg(BG2))
        .title(Line::styled(" Key Bindings ", Style::new().bg(PRIMARY).fg(BG1)).centered())
        .padding(Padding::new(2, 2, 1, 1))
        .style(Style::new().bg(BG2));
    let inner = block.inner(table);
    block.render(table, &mut canvas);
    for (i, line) in table_lines.iter().enumerate() {
        canvas.set_line(inner.x, inner.y + i as u16, line, inner.width);
    }

    // outro, centered as a block
    let outro_w = outro.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
    let outro_x = (x - area.x) + width.saturating_sub(outro_w) / 2;
    let outro_y = table.bottom() + 1;
    for (i, line) in outro.iter().enumerate() {
        canvas.set_line(outro_x, outro_y + i as u16, line, outro_w);
    }

    // blit the visible part
    let max_scroll = total_h.saturating_sub(area.height);
    let scroll = scroll.min(max_scroll);
    for row in 0..area.height.min(total_h - scroll) {
        for col in 0..area.width {
            if let (Some(src), Some(dst)) = (
                canvas.cell((col, row + scroll)),
                buf.cell_mut((area.x + col, area.y + row)),
            ) {
                *dst = src.clone();
            }
        }
    }
    scroll
}
