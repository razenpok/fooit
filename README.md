# fooit

A Rust port of [dooit](https://github.com/dooit-org/dooit) (3.3.4 plus the local
indent/dedent and editing changes), built with [ratatui](https://ratatui.rs).

It uses the same SQLite schema and the same database file as dooit, so both can be
used on the same todos:

- macOS: `~/Library/Application Support/dooit/dooit.db`
- Linux: `$XDG_DATA_HOME/dooit/dooit.db` (usually `~/.local/share/dooit/dooit.db`)

Dates and recurrences are stored in the exact format SQLAlchemy uses, and the
ORM hooks (sibling ordering, completing parents when all children are done,
resetting children, recurrence pushing the due date, urgency clamped to 1-4)
are reimplemented, so data round-trips between the two apps.

## Usage

```
fooit [--db PATH] [-v] [config-loc]
fooit buy milk
```

Anything that isn't an option or command is added as a todo at the bottom of the
topmost workspace.

## Keys (dooit's defaults)

| key | action |
| --- | --- |
| `tab` | switch between workspaces and todos (entering todos starts a new todo) |
| `j` / `k`, down / up | move down / up |
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
| `H` | show / hide completed todos (hidden on start) |
| `X` | remove the completed todos of the workspace (asks for confirmation) |
| `+` `=` / `-` `_` | urgency up / down |
| `/` | search |
| `ctrl+s` | sort siblings |
| `?` | help |
| `q`, `ctrl+q`, `ctrl+c` | quit |

With the mouse, clicking a todo's status toggles its completion, clicking a row
highlights it, the wheel moves the cursor, and dragging a row moves it along with its
children. Where it lands follows the pointer, and moving sideways while dragging
nests it deeper or shallower, as far as the rows around it allow.

While editing, `enter` saves (and on a description, starts a new sibling) and
`escape` discards, asking first if something changed. The up and down arrows (also
with shift) and `tab` save the edit and then move the cursor or switch lists.

## Differences from dooit

- dooit's Python config files (`config.py`) are not supported; fooit always uses
  dooit's default keys, layout, theme and status bar.
- `dooit migrate` (v2 YAML import) is not ported; use dooit for that one-off step.
- Copied items can be pasted into any workspace, and are pasted next to the
  highlighted item rather than next to the original.
- Due dates accept a practical subset of what `dateutil` parses, plus `today`,
  `tomorrow` and `yesterday`.
- Arrow keys move the cursor in addition to `j`/`k`.
- Items start expanded, so children are shown when the app opens; `z` collapses them.

## Development

```
cargo test
cargo build --release
```
