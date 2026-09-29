# 06 — Architecture

## Decision: Rust + SQLite WAL

| Axis | Rust (chosen) | Go | Zig | C |
|---|---|---|---|---|
| Cold start | 2–4ms static-musl | 3–8ms (runtime+GC) | 1–2ms | 1ms |
| Static size (sqlite bundled, stripped) | 1.5–4MB | 6–15MB | 0.5–1.5MB | 0.2–0.8MB |
| Static cross-compile | excellent (musl, rusqlite/bundled) | excellent but `mattn/go-sqlite3` needs CGO → pure-Go slower | best (`zig cc -target …-musl`) | painful vendoring |
| Storage libs | `rusqlite` mature (WAL, cached stmts, FTS5) | `modernc.org/sqlite` good, ~20% slower | hand-rolled wrapper | direct, footguns |
| Hot-path CLI parsing | keep `ghostline-log` free of `clap_derive`/`tokio`/`openssl` | `cobra` +2–3ms | manual (fastest) | manual |

Verdict: **Rust.** Split binary: `ghostline-log` (tiny, `no-default-features`, hand-rolled args, no TLS in hot path) + `ghostline` (full: search/daemon/sync). If <1MB absolutist: Zig second choice (cost: write SQLite+FTS wrapper). Avoid Go for hook path (runtime + CGO friction); avoid C except ~50-line shims.

Pragmas: `journal_mode=WAL; synchronous=NORMAL; temp_store=MEMORY; busy_timeout=5000`. Logger does one `INSERT` with cached prepared statement, no FTS-in-hook. `rustls` over `openssl` if TLS ever in logger path.

## Storage

Primary: **SQLite WAL single file** `~/.local/share/ghostline/history.db`. 0.1–0.5ms single-row (NORMAL + prepared + single txn), 20–50k/s batched, 500k-row FTS query <20ms, 1 writer + N readers, crash-safe. Precedent: atuin, zsh-histdb, browsers.

Rejected for v1 primary: append-JSONL alone (fast `O_APPEND` 0.05ms but O(n) scan, needs sidecar index + compaction), sled/redb/heed (KV-only, manual FTS, crash/maturity + mapsize pain). Hybrid accepted: JSONL as **ingest queue** (`~/.cache/ghostline/queue/$SESSION.jsonl`) tailed by daemon → batch INSERT (500/1s) → CHECKPOINT + FTS.

### Schema (canonical)

```sql
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA busy_timeout=5000;
PRAGMA foreign_keys=ON;

CREATE TABLE history(
  id TEXT PRIMARY KEY,          -- UUIDv7 (time-ordered, merge-friendly)
  cmd TEXT NOT NULL,            -- redacted
  cwd TEXT NOT NULL,
  exit_code INTEGER NOT NULL,
  duration_ns INTEGER NOT NULL, -- monotonic delta, not wall subtraction
  started_at INTEGER NOT NULL,  -- unix ns UTC
  ended_at   INTEGER NOT NULL,
  hostname TEXT NOT NULL,
  session_id TEXT NOT NULL,     -- per shell process
  shell TEXT NOT NULL,          -- bash|zsh|fish + version
  tty TEXT,
  ssh_ctx TEXT,                 -- hash of SSH_CONNECTION, never raw IP
  tmux TEXT,
  container TEXT,               -- docker id / k8s pod
  sudo_user TEXT,
  git_repo TEXT,                -- hook-provided, no git call in hot path
  git_branch TEXT,
  nix_ctx TEXT,
  sensitive INTEGER NOT NULL DEFAULT 0,
  created_seq INTEGER           -- lamport clock for sync tiebreak
) STRICT;
CREATE INDEX idx_time ON history(started_at DESC);
CREATE INDEX idx_cwd ON history(cwd, started_at DESC);
CREATE INDEX idx_session ON history(session_id, started_at);
CREATE INDEX idx_host_session ON history(hostname, session_id);

CREATE VIRTUAL TABLE history_fts USING fts5(cmd, cwd, tokenize='trigram');

CREATE TABLE sessions(
  session_id TEXT PRIMARY KEY,
  hostname TEXT NOT NULL, pid INTEGER NOT NULL,
  shell TEXT NOT NULL, started_at INTEGER NOT NULL,
  tty TEXT, ssh_ctx TEXT, tmux TEXT, env_hash TEXT
);
CREATE TABLE sync_state(
  device_id TEXT PRIMARY KEY, last_seq INTEGER, last_pushed_at INTEGER
);
```

Notes: cap `cmd` ~8KB (overflow → blob table or `truncated=1`); never store `env` wholesale (hashed allowlist only); UUIDv7 + `(started_at, device_id)` = merge order without server.

## Shell integration

| Shell | Pre-exec | Pre-prompt | Duration/exit | Caveats |
|---|---|---|---|---|
| bash | `bash-preexec` DEBUG trap (or 5.3+ native preexec); fallback histfile-poll is lossy, avoid | `PROMPT_COMMAND+=( _ghostline_post )` (append, never overwrite; handle string vs array) | snapshot `$?` first line; `$EPOCHREALTIME` delta | DEBUG fires in subshells/completions → skip `[[ $BASH_COMMAND == _ghostline* ]]`; `extdebug` care |
| zsh | `add-zsh-hook preexec` (`$1` cmd) | `add-zsh-hook precmd` | `local ec=$?`; `$EPOCHREALTIME` via `zsh/datetime` | ordering vs starship/p10k theme; keep native HISTFILE on; multiline quoting |
| fish | `fish_preexec` event (`$argv[1]`) | `fish_postexec` (`$status`, `$CMD_DURATION`) | native ms + status — cheapest | no fire on empty/Ctrl-C variants (`status=130`); don't wrap `fish_prompt` |

Hook payload via env (not argv, avoids `ps` leak); `command ghostline` to bypass aliases; `%q` quoting, never `eval "$cmd"`; `GHOSTLINE_DISABLED=1` escape hatch; `ghostline init bash|zsh|fish` prints version-pinned idempotent snippet.

```bash
_GHOSTLINE_CMD="..." _GHOSTLINE_CWD="$PWD" _GHOSTLINE_EXIT=$? \
  _GHOSTLINE_START=... _GHOSTLINE_END=... _GHOSTLINE_SESSION=$GHOSTLINE_SESSION \
  command ghostline log --shell bash &
```

## Daemon vs sync write

| Model | p50 prompt | Loss | Complexity | When |
|---|---|---|---|---|
| A. Sync direct SQLite | 0.5–2ms | ~0 | lowest | **v1 default** (≤10 shells/host; `busy_timeout`, single-stmt txn; no FTS-in-hook) |
| B. Fire-and-forget `&` | 0.2–0.5ms fork | 1 row on SIGKILL | low (setsid/nohup, zombie care) | cheap upgrade if p99>5ms (HDD/NFS) |
| C. Queue-append + daemon tail | 0.05–0.3ms | 1s batch window | medium (systemd/launchd agent, inotify/kqueue, backpressure) | **target** (sync, FTS, >50 shells) |
| D. Datagram socket to daemon | 0.1–0.4ms | daemon-down → fallback C | highest (0600 perms, macOS path limits) | scale-out / enterprise |

Ship **A now, design for C**: hook prefers `GHOSTLINE_SOCK` (nonblock datagram, drop-on-full) → else queue-append → else sync SQLite. Never block prompt on network; sync out-of-band (`--background`, jittered 5min, killable <100ms). Move `CHECKPOINT FULL`/imports to daemon.

## Environment variables matrix (things that will bite)

| Var / file | Why | Handling |
|---|---|---|
| `$SHELL,$0,$BASH_VERSION,$ZSH_VERSION,$FISH_VERSION` | hook choice, `shell` tag | capture at init + per-session; `$SHELL` ≠ running shell |
| `$TERM,$TERM_PROGRAM`, `dumb` | disable fancy when dumb | skip non-interactive; record term in session hash only |
| TTY | interactive vs cron/pipe | `tty(1)` once/session, cache; null allowed |
| `$SSH_CONNECTION/_CLIENT/_TTY` | shared HOME multiplexing | hash → `ssh_ctx`; source hostname in session seed |
| `$TMUX/_PANE,$STY,$WEZTERM_PANE,$KITTY_WINDOW_ID` | concurrent writers | record pane/window; grouping only |
| `$SUDO_USER,$SUDO_COMMAND,$UID/$EUID` | HOME illusion as root | log `sudo_user`; EUID=0 → `/root/…` DB, never chown user DB |
| Containers (`/.dockerenv`, cgroup `/kubepods/`, `$KUBERNETES_*`) | ephemeral hostname, overlayfs-slow sqlite | tag container id; `pod/node` hostname; warn on overlay, suggest `$HOST_HOME` bind; relax fsync in containers |
| nix (`IN_NIX_SHELL`), snap (`SNAP_*`) | no FHS, read-only TMPDIR | musl static; respect `XDG_DATA/CACHE_HOME`, `$XDG_RUNTIME_DIR→$TMPDIR→~/.cache`; test `nix-shell --pure` + snap |
| Sensitive env (`AWS_*,GH_TOKEN,OPENAI_API_KEY,*PASSWORD/*SECRET,DATABASE_URL`) | bleed via /proc, crash reports, sync | never capture env wholesale; allowlist + hashed rest; redact cmd at capture |
| `HISTFILE/HISTCONTROL/HISTSIZE/HISTIGNORE/SHARE_HISTORY` | double-log, ignorespace/erasedups mismatch, truncation races | mirror ignorespace:erasedups in `GHOSTLINE_IGNORE`/DEDUP; document `disable-up-arrow` opt-in only |
| `XDG_*,HOME,HOSTNAME,$TZ`, clock | identity + ordering | `device_id=hash(HOSTNAME+machine-id)`; UTC ns; Lamport tiebreak; monotonic duration |
