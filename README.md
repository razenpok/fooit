# fooit

A terminal todo manager written in Rust with [ratatui](https://ratatui.rs). It
started as a port of [dooit](https://github.com/dooit-org/dooit) 3.3.4 and keeps
its keys and its database. It looks like [lazygit](https://github.com/jesseduffield/lazygit),
in the terminal's own colors.

fooit uses the same SQLite schema and the same database file as dooit, so both can
be used on the same todos:

- macOS: `~/Library/Application Support/dooit/dooit.db`
- Linux: `$XDG_DATA_HOME/dooit/dooit.db` (usually `~/.local/share/dooit/dooit.db`)

Dates and recurrences are stored in the format SQLAlchemy uses, and dooit's ORM
hooks (sibling ordering, completing parents when all children are done, resetting
children, recurrence pushing the due date, urgency clamped to 1-4) are
reimplemented, so data round-trips between the two apps.

## Usage

```
fooit [--db PATH] [-v] [config-loc]
fooit buy milk
```

Anything that isn't an option or command is added as a todo at the bottom of the
topmost workspace.

A new todo's text can set its urgency with `!2` to `!4` anywhere, and its due date with
its last words: `today`, `tomorrow`, a weekday (`fri`, `friday`), a date (`oct 30`,
`2026-11-02`) and/or a time (`10:30`, `5pm`), optionally after `on` or `at`. So
`fooit pay rent fri !3` adds "pay rent", due Friday, urgency 3. Editing an existing
todo leaves its text as typed.

The app opens on the first workspace's todos, already adding a new one; `escape`
drops it.

## Keys

| key | action |
| --- | --- |
| `tab` | switch between workspaces and todos (entering todos starts a new todo) |
| `j` / `k` | move down / up a row, wrapping around the ends |
| down / up | next / previous item on the same level, wrapping around it |
| right, `l` / left, `h` | into the subtasks / to the parent |
| `gg` / `G` | top / bottom |
| `i` `d` `r` | edit description / due date / recurrence (`1d`, `2w`, `12h`) |
| `a`, `enter` / `A` | add sibling / child |
| `z` / `Z` | toggle expansion of item / parent |
| `J` / `K` | shift down / up |
| `m` | move mode (see below) |
| `>>`, `.` / `<<`, `,` | indent / dedent |
| `xx` | remove (asks for confirmation) |
| `y` / `Y` | copy description / copy item |
| `p` / `P` | paste item below / above |
| `c` | toggle complete |
| `H` | show / hide completed todos |
| `X` | remove the workspace's completed todos (asks for confirmation) |
| `+` `=` / `-` `_` | urgency up / down |
| `u` / `ctrl+r` | undo / redo |
| `/` | search |
| `ctrl+s` | sort siblings |
| `?` | help |
| `q`, `ctrl+q`, `ctrl+c` | quit |

While editing, `enter` saves (and on a description, starts a new sibling) and
`escape` discards, asking first if something changed. The up and down arrows save the
edit and then move the cursor. `tab` / `shift+tab` nest the item under the one above
it / move it out of its parent while you keep typing, also for a new item.

In move mode (`m`) up and down move the item among its siblings and right and left
nest it or move it out. For a todo, `tab` (or left from the top level) goes to the
workspace list, where up and down move it to the bottom of the highlighted workspace,
and `tab` or right goes back. `enter` keeps
it where it is and `escape` puts it back where it was.

Completed todos are hidden, once the cursor has left them and their children.

## Mouse

- Click a todo's status to toggle it, or the `▾` / `▸` in front of an item to collapse or
  expand it.
- Click a todo's description, due date or urgency to edit it, with the cursor where
  you clicked. Click a workspace to select it.
- Drag a row to move it along with its children. Moving sideways while dragging
  nests it deeper or shallower, as far as the rows around it allow. Holding it at the
  top or bottom edge of the list scrolls the list.
- Drop a todo on another workspace to move it to the bottom of that workspace.
- The wheel moves the cursor.

While editing, the mouse is left to the terminal, so text can be selected.

## Differences from dooit

- dooit's Python config files (`config.py`) are not supported, and the layout and
  colors follow lazygit instead of dooit.
- `dooit migrate` (v2 YAML import) is not ported; use dooit for that one-off step.
- Items start expanded and completed todos start hidden.
- Copied items can be pasted into any workspace, and are pasted next to the
  highlighted item rather than next to the original.
- Due dates accept a practical subset of what `dateutil` parses, plus `today`,
  `tomorrow` and `yesterday`.
- The extra keys and the mouse support above.

## Development

```
cargo test
cargo build --release
```
