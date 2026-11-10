# 08 — Performance Budgets

## Budgets (Linux, NVMe, p50)

```
preexec capture (date/printf only):            ≤0.10ms
post hook shell code (exit snapshot + env):    ≤0.30ms
fork+exec ghostline-log (static musl):             ~0.8–1.5ms
  arg parse + redact regex (cached):           ~0.20ms
  sqlite open + 1 INSERT WAL/NORMAL:           ~0.3–1.0ms
  close+exit:                                  ~0.10ms
prompt render delta vs baseline:               ≤2ms total (p99 <5ms)
query 20 results, FTS, 500k rows:              ≤50ms (rare terms and misses; common
                                               ranked tokens cost more)
short search (1–2 chars), 500k rows:           ≤50ms (both miss/rare and broad)
suggest, 500k rows / 4k distinct commands:     ≤50ms p50
read-path lock wait (`search`/`suggest`):      ≤100ms busy timeout
daemon batch 500 rows:                         ≤100ms background, 0ms foreground
```

Human bar: **<10ms feels instant; >10ms sluggish** (zsh-bench). Typing gap ~40ms (fast) to ~167ms (avg) — sync history ghost (<1–5ms, no fork) fits between keystrokes; async/completion ghost <50ms OK, >100ms noticed, 200ms = frustration. History ghost: zero debounce. LLM ghost: 750–1000ms debounce (avoid "garden rake" accidental accepts).

If `HOME` on NFS/spinning disk: budget doubles → auto-fallback queue-append (target arch `06-architecture.md` model C).

## Rules that protect the budget

- Never block `self-insert` on fork/exec; check `PENDING/KEYS_QUEUED`; drop stale async; size-guard buffers.
- The logger is one indexed INSERT; FTS and aggregate triggers update in the same WAL transaction, so they stay in sync without a daemon dependency. Keep the trigger work measured: it is part of the log p99 budget.
- `ghostline-log`: no `tokio`, no `clap_derive`, no `openssl` in hot path; `synchronous=NORMAL`, cached prepared stmt, `busy_timeout`.
- Never block prompt on network. Sync out-of-band, jittered, killable <100ms.

## Benchmarking (reproducible)

1. **Micro startup:** `hyperfine --warmup 50 'ghostline log --help' '/bin/true'`; `strace -c` for open/connect storms. Target <5ms mean, <50 syscalls. CI gate: fail if +20% vs main.
2. **Hook overhead:** baseline vs instrumented:
   ```bash
   hyperfine --shell bash 'eval "$PROMPT_COMMAND"'
   # zsh: zsh -i -c 'time (repeat 200 precmd)'
   # fish: fish -c 'time for i in (seq 200); fish_postexec; end'
   ```
   Median prompt-render delta must be <2ms.
3. **Write throughput:** `ghostline bench --n 10000 --concurrency 8` → `inserts/s`, `SQLITE_BUSY` count, WAL growth; compare `NORMAL vs FULL`, queue vs direct.
4. **Query:** `ghostline bench --n 500000 --reps 20 --gate` seeds its own synthetic corpus (500k rows, 4,000 distinct `cmd-N` commands, 7 common cwds plus one rare marker row), then measures rare search, common search, short miss/rare search, broad 1-char search, and suggest. `--db PATH` keeps the database for `EXPLAIN QUERY PLAN` / `hyperfine` follow-ups. The gate fails on a lost `history_fts`, `idx_cmd_all`, `idx_time`, or `cmd_stats` path, not just on latency. CI runs the same command in `bench-500k` on schedule/dispatch and uploads the JSON artifact; PRs run `--n 10000` and nightly `--n 50000`.
5. **E2E prompt:** `expect`/tmux harness: 100 cmds (`true`, `false`, `sleep 0.01`, 4KB line, unicode, multiline) × bash/zsh/fish × tmux/ssh/docker; assert row counts, exit codes, durations within 20% of `/usr/bin/time`.
6. **Matrix nightly:** `{x86_64,aarch64}×{linux-musl,macOS}×{bash4,bash5.3,zsh5.9,fish3/4}×{local,NFS,container}`; track binary size + cold RSS (`/usr/bin/time -v`).
7. **Non-vacuous gate:** temporarily `sleep 0.02` in logger or force `synchronous=FULL` + FTS-in-hook, confirm bench FAILS, then revert — proves the test catches its bug.

`ghostline bench --json` emits `{hook_ms_p50/p99, log_ms_p50/p99, search_ms_p50/p99, search_common_ms_p50/p99, search_short_ms_p50/p99, search_short_miss_ms_p50/p99, search_short_common_ms_p50/p99, suggest_ms_p50/p99, inserts_per_s, busy_count, wal_bytes, db_bytes, binary_bytes, rss_kb, indexes_ok, search_plan_ok, search_short_plan_ok, search_short_recent_plan_ok, suggest_plan_ok}` for dashboarding.

Measured with the release build on the 500k corpus (Linux NVMe, warm cache, `--reps 20`):

| Metric | p50 | p99 |
|---|---:|---:|
| log | 0.061 ms | 0.198 ms |
| search, rare term | 3.08 ms | 3.48 ms |
| search, common term | 33.0 ms | 34.1 ms |
| search, short rare (`ra`) | 0.32 ms | 0.46 ms |
| search, short miss (`zz`) | 0.15 ms | 0.15 ms |
| search, short broad (1 char) | 0.54 ms | 0.58 ms |
| suggest | 0.58 ms | 0.71 ms |
| hook (fork+exec) | 2.25 ms | 2.74 ms |

Absolute numbers vary by machine; the plans and the 500k gate are the reproducible parts.
