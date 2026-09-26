# nuhist

A small nushell history + hints utility: one daemon per login session serves
fish-style inline hints to every nushell session, with cwd-aware matching and
one synchronized history.

```
 nushell session A ─┐
 nushell session B ─┼──> nuhist daemon ──> nushell history.sqlite3
 nushell session C ─┘        (hints)            (shared, WAL)
```

## What it does

- **One daemon for all sessions.** The first `nuhist` call spawns it; all later
  sessions reuse it. A lock file guarantees a single daemon even when several
  sessions race to start one, and stale sockets from a crashed daemon are
  cleaned up automatically.
- **Synchronized history.** Hints are read straight from nushell's SQLite
  history, so a command typed in *any* session is visible to every other
  session immediately. This works even with `history.isolation = true`, which
  only limits what nushell's built-in hinter shows per session.
- **cwd-scoped suggestions.** Matching mirrors nushell's built-in cwd-aware
  hinter: the most recent entry starting with the current line that was run in
  the current directory wins; if there is none, the most recent entry from any
  directory is used.
- **fish-style file suggestions.** When history has no match, the last token on
  the line is completed from the filesystem: `cat REA` → `DME.md`,
  `cd sr` → `c/`, `nvim src/ma` → `in.rs`. Directories get a trailing slash,
  hidden files need a leading dot, and unterminated quotes are closed.
- **Never in the way.** Hinting is silent: if the daemon or history database is
  unavailable, the client prints nothing and exits successfully.

## Requirements

- A Unix system (unix domain sockets).
- Nushell with an external hinter closure (`$env.config.hinter.closure`),
  added in nushell 0.112.0.
- `history.file_format = "sqlite"` (the nushell default).

## Install

```sh
cargo install --path .
```

Then add this to `config.nu`:

```nu
$env.config.hinter.closure = {|ctx|
    if ($ctx.line | is-empty) {
        null
    } else {
        let hint = (^nuhist hint --line $ctx.line --cwd $ctx.cwd | str trim --right)

        if ($hint | is-empty) {
            null
        } else {
            $hint
        }
    }
}
```

Only trim the right side: a hint may legitimately start with a space (typing
`git` can suggest ` push origin main`), and trimming the left would turn
`git push` into `gitpush`.

## Hints

For a line like `cargo `, the daemon looks for the newest history entry that
starts with `cargo `:

1. entries recorded while the cwd was the one nushell passes in `ctx.cwd`,
2. otherwise entries from any directory.

Matching is literal and case sensitive — `%` and `_` are ordinary characters,
unlike SQL `LIKE`. An exact hit returns an empty hint instead of falling
through to an older, longer command, just like nushell's built-in hinter.

## File suggestions

History is consulted first, so files only show up when nothing in history
matches — `git ` still suggests `push origin main` even if the directory has a
`Cargo.toml`. (A history match that only adds whitespace, as some old entries
do, is ignored rather than showing an invisible hint.) Then the last token on
the line is completed:

| Line              | Hint         | Because                          |
| ----------------- | ------------ | -------------------------------- |
| `cat REA`         | `DME.md`     | bare name in argument position   |
| `nvim src/ma`     | `in.rs`      | path with a separator            |
| `cd sr`           | `c/`         | directories get a slash          |
| `cd ~/no`         | `tes.txt`    | `~` expands to home              |
| `cat "My Doc`     | `uments/"`   | open quotes are closed           |
| `cat .hid`        | `den`        | hidden files need a dot prefix   |
| `cat README.md`   | *(none)*     | exact names are already complete |

Names that would need escaping are skipped, because a hinter can only append
to the line (it cannot insert an opening quote before what you typed). If you
quote the path yourself, the closing quote is appended for you.

`NUHIST_FILE_HINTS` controls how eager this is:

| Value            | Behavior                                                        |
| ---------------- | --------------------------------------------------------------- |
| `off`            | never suggest files                                             |
| `path`           | only path-like tokens (`.`, `..`, `~`, `/`, or a `/` inside)    |
| `args` (default) | path-like tokens anywhere, plus bare names in argument position |
| `all`            | bare names everywhere, including command position               |

The mode is read when the daemon starts; run `nuhist stop` after changing it
(that happens automatically on the next login). `nuhist status` prints the
active mode.

## CLI

```
nuhist hint [--line <LINE>] [--cwd <CWD>]   print the completion suffix for LINE
nuhist <LINE>                               shorthand for `nuhist hint --line <LINE>`
nuhist daemon [--history <PATH>] [--socket <PATH>] [--idle-timeout <SECS>] [--file-hints <MODE>]
nuhist status                               show daemon status
nuhist stop                                 stop the daemon
```

`nuhist status` works even when the daemon is not running and points at the
history database and log file.

## Configuration

| Variable           | Meaning                                          | Default                                      |
| ------------------ | ------------------------------------------------ | -------------------------------------------- |
| `NUHIST_HISTORY`   | nushell history database                         | `$XDG_CONFIG_HOME/nushell/history.sqlite3`   |
| `NUHIST_SOCKET`    | daemon unix socket                               | `$XDG_RUNTIME_DIR/nuhist.sock`               |
| `NUHIST_FILE_HINTS`| file suggestion mode: `off`, `path`, `args`, `all` | `args`                                     |
| `NU_HISTORY_PATH`  | legacy alias for `NUHIST_HISTORY`                |                                              |

The daemon logs to `$XDG_STATE_HOME/nuhist/nuhist.log` (usually
`~/.local/state/nuhist/nuhist.log`).

The daemon exits together with your login session. If you prefer it to stop
after an idle period, start it with `--idle-timeout` (seconds); the next hint
will start it again automatically.

## Development

```sh
cargo test
cargo build --release
```
