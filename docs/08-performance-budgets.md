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
query 20 results, FTS, 500k rows:              ≤50ms
daemon batch 500 rows:                         ≤100ms background, 0ms foreground
```

Human bar: **<10ms feels instant; >10ms sluggish** (zsh-bench). Typing gap ~40ms (fast) to ~167ms (avg) — sync history ghost (<1–5ms, no fork) fits between keystrokes; async/completion ghost <50ms OK, >100ms noticed, 200ms = frustration. History ghost: zero debounce. LLM ghost: 750–1000ms debounce (avoid "garden rake" accidental accepts).

If `HOME` on NFS/spinning disk: budget doubles → auto-fallback queue-append (target arch `06-architecture.md` model C).

## Rules that protect the budget

- Never block `self-insert` on fork/exec; check `PENDING/KEYS_QUEUED`; drop stale async; size-guard buffers.
- No FTS update in hook (defer to daemon / `ghostline vacuum` cron).
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
4. **Query:** seed 500k (`ghostline seed`), `hyperfine 'ghostline search k8s --limit 20'`; `EXPLAIN QUERY PLAN` in CI to catch missing-index regressions.
5. **E2E prompt:** `expect`/tmux harness: 100 cmds (`true`, `false`, `sleep 0.01`, 4KB line, unicode, multiline) × bash/zsh/fish × tmux/ssh/docker; assert row counts, exit codes, durations within 20% of `/usr/bin/time`.
6. **Matrix nightly:** `{x86_64,aarch64}×{linux-musl,macOS}×{bash4,bash5.3,zsh5.9,fish3/4}×{local,NFS,container}`; track binary size + cold RSS (`/usr/bin/time -v`).
7. **Non-vacuous gate:** temporarily `sleep 0.02` in logger or force `synchronous=FULL` + FTS-in-hook, confirm bench FAILS, then revert — proves the test catches its bug.

`ghostline bench --json` emits `{hook_ms_p50/p99, log_ms_p50/p99, inserts_per_s, busy_count, wal_bytes, binary_bytes, rss_kb}` for dashboarding.
