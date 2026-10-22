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

The app opens on the first workspace's todos, already adding a new one; `escape`
drops it.

## Keys

| key | action |
| --- | --- |
| `tab` | switch between workspaces and todos (entering todos starts a new todo) |
| `j` / `k`, down / up | move down / up, wrapping around the ends |
| shift+down / shift+up | next / previous top-level item |
| `gg` / `G` | top / bottom |
| `i` `d` | edit description / due date |
| `a`, `enter` / `A` | add sibling / child |
| `z` / `Z` | toggle expansion of item / parent |
| `J` / `K` | shift down / up |
| `>>` / `<<`, right / left | indent / dedent |
| `xx` | remove (asks for confirmation) |
| `y` / `Y` | copy description / copy item |
| `p` / `P` | paste item below / above |
| `c` | toggle complete |
| `H` | show / hide completed todos |
| `X` | remove the workspace's completed todos (asks for confirmation) |
| `+` `=` / `-` `_` | urgency up / down |
| `/` | search |
| `ctrl+s` | sort siblings |
| `?` | help |
| `q`, `ctrl+q`, `ctrl+c` | quit |

While editing, `enter` saves (and on a description, starts a new sibling) and
`escape` discards, asking first if something changed. The up and down arrows (also
with shift) and `tab` save the edit and then move the cursor or switch lists.

Completed todos are hidden, once the cursor has left them and their children.

## Mouse

- Click a todo's status to toggle it, or the `▾` / `▸` in front of an item to collapse or
  expand it.
- Click a todo's description, due date or urgency to edit it, with the cursor where
  you clicked. Click a workspace to select it.
- Drag a row to move it along with its children. Moving sideways while dragging
  nests it deeper or shallower, as far as the rows around it allow.
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
