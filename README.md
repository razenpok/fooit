# fooit

A Rust port of [dooit](https://github.com/dooit-org/dooit) 3.3.4, built with
[ratatui](https://ratatui.rs).

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
```

## Keys (dooit's defaults)

| key | action |
| --- | --- |
| `tab` | switch between workspaces and todos |
| `j` / `k`, arrows | move down / up |
| `gg` / `G` | top / bottom |
| `i` `d` | edit description / due date |
| `a` / `A` | add sibling / child |
| `z` / `Z` | toggle expansion of item / parent |
| `J` / `K` | shift down / up |
| `xx` | remove (asks for confirmation) |
| `y` / `Y` | copy description / copy item |
| `p` / `P` | paste item below / above |
| `c` | toggle complete |
| `+` `=` / `-` `_` | urgency up / down |
| `/` | search |
| `ctrl+s` | sort siblings |
| `?` | help |
| `ctrl+q`, `ctrl+c` | quit |

While editing, `enter` or `escape` saves.

## Differences from dooit

- dooit's Python config files (`config.py`) are not supported; fooit always uses
  dooit's default keys, layout, theme and status bar.
- `dooit migrate` (v2 YAML import) is not ported; use dooit for that one-off step.
- Copied items can be pasted into any workspace, and are pasted next to the
  highlighted item rather than next to the original.
- Due dates accept a practical subset of what `dateutil` parses, plus `today`,
  `tomorrow` and `yesterday`.
- Arrow keys move the cursor in addition to `j`/`k`.

## Development

```
cargo test
cargo build --release
```
