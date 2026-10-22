# fooit

A terminal todo manager written in Rust with [ratatui](https://ratatui.rs). It
started as a port of [dooit](https://github.com/dooit-org/dooit) 3.3.4 and keeps
its keys and its database. It looks like [lazygit](https://github.com/jesseduffield/lazygit),
in the terminal's own colors.

fooit keeps its todos in its own SQLite database:

- macOS: `~/Library/Application Support/fooit/fooit.db`
- Linux: `$XDG_DATA_HOME/fooit/fooit.db` (usually `~/.local/share/fooit/fooit.db`)

On its first start, when that file does not exist yet, fooit imports dooit's database
(`dooit/dooit.db` in the same folder) if there is one, and leaves it as it was. From
then on the two apps keep their own todos. dooit's rules (completing a parent when all
its children are done, resetting children, recurrence pushing the due date, urgency
kept to 1-4) still apply.

## Usage

```
fooit [--db PATH] [-c KEYS] [-v] [config-loc | keys | share | sync]
fooit buy milk
```

Anything that isn't an option or command is added as a todo at the bottom of the
topmost workspace.

A new todo's text can set its urgency with `!2` to `!4` anywhere, and its due date with
its last words: `today`, `tomorrow`, a weekday (`fri`, `friday`), a date (`oct 30`,
`2026-11-02`) and/or a time (`10:30`, `5pm`), optionally after `on` or `at`, or a time
from now (`1d 5h`, `in 30m`). So
`every 3d`, `every day` or `every 2 weeks` makes it repeat, due today unless it says
otherwise. While you type, the bottom line shows what will be read. `fooit pay rent fri !3` adds "pay rent", due Friday, urgency 3. `#name` anywhere
sends it to the bottom of the workspace with that name, or the first one whose name
starts with it (case and spaces don't matter): `fooit call bank #tiara`. Editing an existing
todo leaves its text as typed.

The app opens on the first workspace's todos, already adding a new one; `escape`
drops it.

**Today**, at the top of the workspace list, lists the open todos due today or earlier
from every workspace, soonest first, each with the workspace it is in. Its row shows
how many are overdue (in red, like `!2`) and how many are due later today.
**Upcoming**, under it, lists what is due in the next seven days, by day. They can be
completed and edited there; adding and arranging happens in their workspace.

## Keys

| key | action |
| --- | --- |
| `tab` | switch between workspaces and todos (entering todos starts a new todo) |
| `j` / `k` | move down / up a row |
| down / up | next / previous item on the same level |
| | past the first or last item of a level (the workspaces, a workspace's todos, a todo's subtasks; the whole list for `j`/`k`), a new item starts at its end; from that new item, down goes round to the first and up back to the last |
| right, `l` / left, `h` | into the subtasks (or child workspaces) / to the parent; from a workspace without child workspaces, or a top-level todo, like `tab` |
| `g` / `G` | top / bottom |
| `i`, space / `d` / `r` | edit description / due date (a date, or from now: `1d`, `1d 5h`, `30m`) / recurrence (`1d`, `2w`, `12h`) |
| `a`, `enter` / `A` | add sibling / child |
| backspace | edit the description, deleting its last character |
| `z` / `Z` | toggle expansion of item / parent |
| `J` / `K` | shift down / up |
| `m` | move mode (see below) |
| `v` | mark the item (and go to the next on its level); `c`, `s`, `+`/`-`, `m` and `x` then act on all marked; `escape` unmarks |
| `>>`, `.` / `<<`, `,` | indent / dedent |
| `x` | remove (asks for confirmation; enter or `y` confirms) |
| `y` / `Y` | copy description / copy item |
| `p` / `P` | paste item below / above |
| `c` | toggle complete |
| `s`, `3s` | snooze: push the due date a day (or three) later, from today when overdue |
| `H` | show / hide completed todos |
| `X` | remove the workspace's completed todos (asks for confirmation) |
| `+` `=` / `-` `_` | urgency up / down |
| `u` / `ctrl+r` | undo / redo |
| `/` | search the list |
| `ctrl+f` | search every workspace; enter on a result goes to it |
| `ctrl+s` | sort siblings |
| `?` | help |
| `q`, `ctrl+q`, `ctrl+c` | quit |

While editing, `enter` saves (and on a description, starts a new sibling) and
`escape` discards, asking first if something changed. The up and down arrows save the
edit and then move the cursor. `tab` / `shift+tab` nest the item under the one above
it / move it out of its parent while you keep typing, also for a new item. With nothing
typed yet, left and right leave the edit and move on as they do outside it.

In move mode (`m`) up and down move the item among its siblings and right and left
nest it or move it out. For a todo, `tab` (or left from the top level) goes to the
workspace list, where up and down move it to the bottom of the highlighted workspace,
and `tab` or right goes back. `enter` keeps
it where it is and `escape` puts it back where it was.

Completed todos are hidden, once the cursor has left them and their children.

## Rebinding keys

Keys can be rebound in a keys file, at the path `fooit config-loc` prints (or the one
given with `-c`). Each line binds an action to a key or a list of keys, in place of its
defaults; `fooit keys` prints every action with its current keys to start from:

```
remove_node = "d"
next_sibling = ["<down>", "J"]
edit_due = "D"
```

Keys are written as in the help screen: `a`, `gg`, `<ctrl+s>`, `<down>`. fooit won't
start with a key bound twice, or one that is the start of another (`x` next to `xx`).

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

## Sync and sharing

Workspaces can be synced between machines and shared with other people through a
small server you run on Cloudflare (see [worker/](worker/README.md)). Everything is
encrypted on your machine before it is sent; the server only keeps the order of changes.

Tell fooit where the server is, in `config.toml` next to the keys file (see
`fooit config-loc`):

```toml
[sync]
url = "https://fooit-sync.you.workers.dev"
token = "the OWNER_TOKEN you gave the Worker"
```

or with `FOOIT_SYNC_URL` and `FOOIT_SYNC_TOKEN`, which win over the file. The token
is only needed to start syncing new workspaces and shares; machines that only import
shares need neither setting.

A **share** is a link that gives access to a set of workspaces. A workspace can be in
any number of shares, so different people can get different workspaces:

```
fooit share new dev general dev       # prints the link for general + dev
fooit share new art general art       # and one for general + art
fooit share new me general dev art    # all three, for your own machines
```

On another machine, or for another person:

```
fooit share import 'https://fooit-sync.you.workers.dev/#…' --name from-me
```

brings in every workspace in the share, and any added to it later. Anyone with a link
can read and change its workspaces, so give it out like a password.

To move a share to your own other machine without copying the link, make a pairing
code; it works once, for ten minutes:

```
fooit share pair me                   # prints a code like k7fq-m2xa-9p4d
fooit share import k7fq-m2xa-9p4d     # on the other machine (add --server URL if it has no config)
```

| command | |
| --- | --- |
| `fooit share new NAME WORKSPACE...` | make a share and print its link |
| `fooit share add SHARE WORKSPACE...` | put workspaces in a share |
| `fooit share remove SHARE WORKSPACE...` | take them out: the share's people keep what they have, but get nothing new |
| `fooit share link SHARE` | print a share's link again |
| `fooit share pair SHARE` | a code for another machine to import the share with, once, within ten minutes |
| `fooit share list` | the shares here and their workspaces |
| `fooit share import LINK-OR-CODE [--name NAME] [--server URL]` | add a share from its link or a pairing code |
| `fooit share leave SHARE` | forget a share here; its workspaces stay, and stop syncing unless another share has them |
| `fooit share rename SHARE NAME` | rename a share, here only |
| `fooit sync` | sync now |

While the app runs it syncs a couple of seconds after each change and every 30
seconds otherwise, marks synced workspaces with `⇅`, and shows sync problems in the
bar. `fooit buy milk` syncs right after adding.

How it works: each synced workspace is a log on the server, holding a base (the
whole workspace at some point) and the batches of changes pushed after it. A machine
pulls the new batches, merges them with what changed locally since its last sync, and
pushes the rest. When two machines change the same thing, the change that reached the
server first wins. Batches older than 30 days are folded into the base; a machine that
was away for longer starts again from the base, keeping its own changes. A share is a
log too, listing the secrets of its workspaces. Taking a workspace out of a share
moves it to a new secret, which the other shares that hold it pass on.

Deleting a synced workspace deletes it for everyone it is shared with.

## Differences from dooit

- dooit's Python config files (`config.py`) are not supported (keys can be rebound in
  fooit's own keys file), and the layout and colors follow lazygit instead of dooit.
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

To test syncing against the Worker itself, run it with `npx wrangler dev` in `worker/`
(with `OWNER_TOKEN` in `worker/.dev.vars`), then

```
FOOIT_TEST_SYNC_URL=http://127.0.0.1:8787 FOOIT_TEST_SYNC_TOKEN=... cargo test worker -- --ignored
```
