# GHOSTLINE

GHOSTLINE is a local-first shell history tool: one small Rust binary (`ghostline`) plus shell hooks for bash, zsh, and fish. It records commands with their working directory, exit status, session, host, and shell in a local SQLite database, then uses that history for prefix ghost-text suggestions, typo repair, and search. The three jobs share one store and one binary, and the default path makes no network calls.

It is for people who want faster history recall and inline correction without a sync account, a resident model, or a language runtime in the prompt. This is v0.1: the bash ghost is the most complete experience; zsh and fish currently provide logging hooks.

## Features

### Implemented today

**History capture**

- `ghostline log` records one row per command: command, cwd, exit code, timestamp, hostname, session, shell, and sensitive flag. Commands are whitespace-normalized, capped at 8 KiB, and self-commands (`ghostline ...`) are skipped.
- Hooks come from `ghostline init bash|zsh|fish`. Bash uses `PROMPT_COMMAND` plus `history 1`; zsh uses `preexec`/`precmd`; fish uses `fish_postexec`.
- Storage is SQLite in WAL mode with a `history` table, time/cwd/session/prefix indexes, and an FTS5 virtual table with insert/update/delete triggers. The database, its WAL/SHM sidecars, and the `ghostline` data directory are private (`0600`/`0700`).
- An optional JSONL ingest queue (`log --queue` or `GHOSTLINE_QUEUE=1`) is drained by `ghostline daemon` in batches of 500 rows.
- `ghostline import` reads a bash-style `HISTFILE`, including `#<unix-seconds>` timestamps.

**Ghost suggestions (bash)**

- History suggestions are prefix matches ranked by exact-cwd affinity, frequency, and recency. Only successful, non-sensitive commands are candidates, and candidates containing control characters are dropped.
- When no history matches and the buffer is one token, lookup falls back to executable names on `PATH` plus shell builtins.
- The default bash hook paints an on-demand dim suffix. `Ctrl-G` previews, `Right`/`Ctrl-F`/`End` accept at end of line, `Alt-Right`/`Alt-F` accept one word, and `Ctrl-]` dismisses.
- The optional loadable bash builtin (`builtin/ghostline_autosuggest.c`) hooks readline's redisplay and paints the suffix automatically on every keypress without a full-line refresh.
- zsh and fish hooks log history only; they do not paint a ghost yet.

**Typo repair**

- `ghostline fix` suggests up to three corrections: swapped program order, unknown command name (checked against `PATH`, builtins, and history), `git` subcommand or branch typos, `cd`/path case and edit-distance path typos, missing `mkdir -p`, flag typos seen in history, and a narrow missing-`sudo` heuristic.
- `fix --last` uses the most recent non-zero-exit command. Repair only prints candidates; nothing is auto-executed.

**Search and diagnostics**

- `ghostline search` uses FTS5 trigram phrase matching for queries of at least three characters, with a `LIKE` fallback for shorter queries. Filters cover cwd (including subdirectories), exit status (`N` or `!N`), session, host, and timestamp. Output is tab-separated: timestamp (Unix ns), exit code, cwd, command.
- No fzf/skim binding or `Ctrl-R` hijack is installed.
- `ghostline bench` reports log/search/suggest/hook percentiles, inserts per second, BUSY count, WAL and DB sizes, binary size, RSS, and schema presence. `--gate` exits non-zero when a budget is exceeded or a query plan loses its FTS/index path; `--json` emits a machine-readable report.

### Roadmap / not yet implemented

- Fuzzy history matching, fzf/skim integration, opt-in `Ctrl-R` search, and zsh/fish ghost painting.
- tldr templates, `explain`, NL-to-command, local models, and cloud fallback.
- Destructive-command tiers, undo guard, typed confirmation flows, `stats`, `forget`, `why`, `replay`, sanitized export, team cheatsheets, and multi-machine sync.
- Next-command prediction, project-aware habit ranking, the alias advisor, daemon service units, socket transport, and an installed `gl` alias.

See [docs/09-roadmap.md](docs/09-roadmap.md) for the planned slices and parked work.

## Install

### From a checkout

```bash
cd /path/to/ghostline
./install.sh
```

`install.sh` builds a release binary, installs it to `~/.local/bin` (override with `--prefix DIR` or `GHOSTLINE_PREFIX`), installs the optional bash builtin when possible, and appends an idempotent `ghostline init` block to the rc files of installed shells. Re-run it to update. `./install.sh --uninstall` removes the rc blocks and keeps the binary unless `--prefix` is also supplied.

### Manual build

```bash
cargo build --release
mkdir -p ~/.local/bin
cp target/release/ghostline ~/.local/bin/
```

Add a shell hook as shown in Quick start. The optional native auto-ghost is built separately and must sit next to the binary:

```bash
make -C builtin
cp builtin/ghostline_autosuggest.so ~/.local/bin/
```

The builtin requires `cc` and bash development headers; the Makefile expects `/usr/include/bash` and `/usr/include/bash/include`. If it is absent or fails to build, the on-demand shell ghost still works. Logging supports bash, zsh, and fish. The flicker-free automatic ghost is bash-only today and requires bash 4 or newer.

## Quick start

Wire the hook once:

```bash
eval "$(ghostline init bash)"   # bash
eval "$(ghostline init zsh)"    # zsh
ghostline init fish | source    # fish
```

Hooks log automatically. Direct CLI examples:

```bash
ghostline log --shell bash --cwd "$PWD" --exit 0 --session demo "cargo test"
ghostline search cargo --limit 5
ghostline suggest --cwd "$PWD" -- "git ch"        # suffix, e.g. "eckout main"
ghostline suggest --all --cwd "$PWD" -- "git ch"  # full candidates
ghostline fix "gti status"
ghostline import --from ~/.bash_history
```

Queue mode:

```bash
export GHOSTLINE_QUEUE=1
ghostline daemon --once
ghostline daemon
```

With the shell ghost, `Ctrl-G` previews; `Right`/`Ctrl-F`/`End` accept at end of line; `Alt-Right`/`Alt-F` accept a word; `Ctrl-]` dismisses. With the native builtin, `Tab` accepts a visible ghost and otherwise completes.

## CLI reference

| Command | Purpose | Key flags |
|---|---|---|
| `ghostline init SHELL` | Print the hook for `bash`, `zsh`, or `fish` | none |
| `ghostline log [flags] <command...>` | Log one command | `--shell S`, `--cwd DIR`, `--exit CODE`, `--session ID`, `--db PATH`, `--queue`, `--queue-dir D` |
| `ghostline daemon [--once]` | Drain the queue once or in a loop | `--once`, `--interval-ms N`, `--queue-dir D`, `--db PATH` |
| `ghostline import [--from HISTFILE]` | Bulk-import a bash history file | `--from HISTFILE` |
| `ghostline fix [--last] [command...]` | Print `did you mean` candidates; exit 0 when found | `--last`, `--exit CODE`, `--cwd DIR`, `--db PATH`, `-q` |
| `ghostline search [flags] [query]` | Search history and print TSV rows | `--limit N`, `--dir DIR`, `--exit N` or `!N`, `--session S`, `--host H`, `--since UNIX_SECS`, `--db PATH` |
| `ghostline suggest [flags] <buffer>` | Print the next ghost suffix, or full commands with `--all` | `--cwd DIR`, `--limit N`, `--all`, `--db PATH` |
| `ghostline bench [flags]` | Run measurements and optional gate | `--n N`, `--reps R`, `--concurrency C`, `--json`, `--db PATH`, `--gate`, `--max-log-ms X`, `--max-search-ms X`, `--max-suggest-ms X`, `--max-hook-ms X`, `--min-inserts-per-s X` |

`ghostline --help` and `--version` are also available. Search defaults to 20 rows and suggest to 5; `search --dir` matches the directory and every directory below it.

## How it works

```text
 shell hook (bash / zsh / fish)
    |
    +-- ghostline log -------------------> SQLite WAL (default)
    |       `-- GHOSTLINE_QUEUE=1 -------> JSONL queue --> ghostline daemon --> SQLite
    |
    +-- ghostline suggest <--------------- SQLite (read)

 optional bash builtin: readline redisplay --> ghostline suggest
```

The CLI is a small Rust binary with hand-rolled argument parsing and bundled SQLite. The schema stores a UUIDv7 id, command, cwd, exit code, timestamps, host, session, and shell, plus context columns (`tty`, `ssh_ctx`, `tmux`, `container`, `sudo_user`, `git_repo`, `git_branch`) that the current hooks do not populate.

The queue is one append-only JSONL file. The daemon renames it to claim it, skips malformed lines, inserts 500-row chunks, and checkpoints WAL. The bash hook asks `suggest` for a suffix; the native builtin caches the candidate and forks only on divergence. Redaction runs before rows are persisted.

## Performance

Targets from [docs/08-performance-budgets.md](docs/08-performance-budgets.md):

| Path | Documented target |
|---|---|
| Pre-exec capture (shell) | ≤ 0.10 ms |
| Post hook shell code | ≤ 0.30 ms |
| Fork + exec `ghostline log` | ~0.8–1.5 ms |
| Prompt render delta | ≤ 2 ms total, p99 < 5 ms |
| Search, 20 results, 500k rows | ≤ 50 ms (FTS target) |
| Daemon batch, 500 rows | ≤ 100 ms background, 0 ms foreground |

`bench --gate` defaults enforced in `src/cmd_bench.rs`, all overridable:

| Check | Default |
|---|---|
| Log p99 | ≤ 5 ms |
| Search p50 | ≤ 50 ms |
| Suggest p50 | ≤ 50 ms |
| Hook p99 | ≤ 100 ms |
| Throughput | ≥ 2000 inserts/s |
| Schema objects | `history`, `history_fts`, `idx_cmd`, `idx_time`, `idx_cwd`, `idx_session` |

```bash
ghostline bench --gate
ghostline bench --n 50000 --reps 20 --json --gate
```

Search uses trigram FTS for three-character-and-longer queries and a `LIKE` fallback for shorter ones.

## Privacy and security

- Local-first: no sockets, telemetry, accounts, auto-update, or network calls in the default path.
- The database defaults to `$XDG_DATA_HOME/ghostline/history.db` or `~/.local/share/ghostline/history.db`; the database and its WAL/SHM sidecars are chmod `0600`, and the data directory is chmod `0700`. The queue file defaults to `$XDG_CACHE_HOME/ghostline/queue/history.jsonl` or `~/.cache/ghostline/queue/history.jsonl`.
- Redaction runs before persistence for `log` and `import`. Rules cover common token formats (OpenAI, Anthropic, GitHub, AWS, Google, Slack, Stripe), JWTs, PEM private keys, database URLs with passwords, `curl -u`, and password/token/key/value patterns. Matches are replaced and the row is marked `sensitive=1`.
- `GHOSTLINE_IGNORE` excludes matching commands from `log`; `GHOSTLINE_DISABLED` pauses logging and ghost painting. Suggestions never execute; `fix` only prints candidates.
- Redaction is best-effort, not a guarantee. Sensitive rows are excluded from suggestions; the database and its WAL/SHM sidecars are chmodded `0600`, and the data directory is chmodded `0700`.

## Configuration

| Variable | Used by | Effect |
|---|---|---|
| `GHOSTLINE_DISABLED` | Hooks and bash ghost | Any non-empty value pauses logging and painting |
| `GHOSTLINE_IGNORE` | `log` | Glob list (`*`, `?`) separated by commas, colons, or whitespace; matching commands are not stored |
| `GHOSTLINE_QUEUE` | `log` | `1` appends to the JSONL queue instead of writing SQLite directly |
| `GHOSTLINE_GHOST` | Bash snippet and native builtin | `0` disables the ghost |
| `GHOSTLINE_GHOST_STYLE` | Native builtin | SGR parameters; default `2;38;5;244`; only digits and `;` accepted |
| `GHOSTLINE_PROMPT_LEN` | Bash snippet | Override visible prompt width for the wrap guard |
| `GHOSTLINE_BIN` | Native builtin | Absolute path to the binary; otherwise found by walking `PATH` |
| `GHOSTLINE_SESSION` | Hooks and `log` | Session id; the init snippet creates one when unset |
| `GHOSTLINE_SHELL` | `log` | Fallback shell name when `--shell` is omitted |
| `GHOSTLINE_PREFIX` | `install.sh` | Install directory; default `~/.local/bin` |
| `XDG_DATA_HOME` | Database path | Base directory for `ghostline/history.db` |
| `XDG_CACHE_HOME` | Queue path | Base directory for `ghostline/queue` |

## Building and testing

```bash
cargo build
cargo build --release
cargo test
cargo clippy --all-targets
```

- `cargo test` runs unit and integration tests. The pty end-to-end tests in `tests/pty.rs` use `python3` when available and skip otherwise.
- The native builtin test builds `builtin/ghostline_autosuggest.c` with `cc`; it skips when the compiler or bash headers are missing. `make -C builtin` builds it independently.
- CI in `.github/workflows/perf.yml` runs `cargo test`, a release build, and `ghostline bench --gate` on pushes and pull requests, with a larger seed in the weekly Ubuntu/macOS matrix.

## Project layout

```text
src/                 Rust CLI
  main.rs            entry point and usage text
  cmd_*.rs           init, log, daemon, import, fix, search, suggest, bench
  db.rs, queue.rs    SQLite schema/query and JSONL queue
  redact.rs, fix.rs  redaction and repair rules
  complete.rs        PATH/builtin fallback
builtin/             bash loadable builtin + Makefile
tests/               shell, pty, and pty scripts
docs/                design notes and specs
install.sh           build/install/wire shells
Cargo.toml
```

## Status and limitations

GHOSTLINE is v0.1. Logging, queue/daemon, import, prefix suggestion, repair, search, and benchmark paths work end-to-end, but the project is narrower than the full design in `docs/03-features.md`.

Known rough edges in the current source:

- The bash logger polls `history 1`, so very rapid prompts can miss or merge entries; zsh and fish use native pre/post-exec hooks.
- `duration_ns` is stored as `0`, and the tty/SSH/tmux/container/git context columns are not populated by the shipped hooks.
- `search` prints stored command text verbatim, including any control bytes logged before redaction.
- The `gl` alias mentioned in older design notes is not installed.

The roadmap tracks deferred work: [docs/09-roadmap.md](docs/09-roadmap.md).

## Documentation

- [docs/README.md](docs/README.md) — documentation index and project pitch
- [docs/01-vision-and-principles.md](docs/01-vision-and-principles.md) — problem, principles, non-goals
- [docs/03-features.md](docs/03-features.md) — full feature specification (includes future work)
- [docs/04-ux-spec.md](docs/04-ux-spec.md) — ghost text, keybindings, and flows
- [docs/06-architecture.md](docs/06-architecture.md) — storage, schema, shell integration, daemon design
- [docs/07-privacy-safety-security.md](docs/07-privacy-safety-security.md) — redaction and safety model
- [docs/08-performance-budgets.md](docs/08-performance-budgets.md) — budgets and benchmarking method
- [docs/09-roadmap.md](docs/09-roadmap.md) — v1/v1.1/v2 slices and parked ideas
