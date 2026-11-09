# `src/db.rs` review — bugs, scale limits, and opportunities

**Date:** 2026-11-09  
**Commit reviewed:** `4dc35ef`  
**Scope:** all 270 lines of `src/db.rs`, plus every caller (`cmd_log`, `cmd_search`, `cmd_suggest`, `cmd_import`, `cmd_daemon`, `cmd_bench`, `cmd_fix`)  
**Method:** full read, caller tracing, `EXPLAIN QUERY PLAN`, timing on synthetic 100k/500k-row databases, and behavioral repros with the built binary. Synthetic data is deliberately skewed toward common prefixes; absolute numbers are illustrative, while plan shapes and ratios are the durable signal.

---

## Summary

| # | Severity | Finding | Lines |
|---|---|---|---|
| 1 | High | `--since` compares seconds against nanosecond timestamps → silent no-op | 160–164 |
| 2 | High | Case-insensitive SQL match + case-sensitive suffix strip → ghost disappears | `SUGGEST_SQL:209`, `cmd_suggest.rs:65` |
| 3 | High (privacy) | `sensitive=1` rows are still suggested, contrary to docs/07 | 202–241 |
| 4 | High (scale) | FTS index is maintained on every insert but never queried | 47–53, 140 |
| 5 | High (scale) | Suggest full-scans on every keystroke; 500k-row common prefix = 64 ms | 202–241 |
| 6 | Medium | LIKE `%`/`_` are not escaped in query or buffer | 140, 209 |
| 7 | Medium | `--dir` is exact equality while the doc says “cwd prefix” | 144–148 |
| 8 | Medium (security) | `search` prints stored control bytes raw → terminal escape injection | 169–186 |
| 9 | Medium | Magic `limit + 16` over-fetch can starve clean candidates | 232–240 |
| 10 | Medium | Negative limits become SQLite “no limit”; large values can overflow | 165, 234 |
| 11 | Medium (privacy) | WAL/SHM are `0644`; only the main DB is chmodded `0600` | 57–69 |
| 12 | Medium | No schema migrations (`PRAGMA user_version` is 0) | 19–54 |
| 13 | Medium | Bench gate never asserts `SEARCH` vs `SCAN` | 190–200, 244–251 |
| 14 | Low | `--exit` accepts only `0`/`!0`; other values silently ignored | 131–135 |
| 15 | Low | Empty `XDG_DATA_HOME` creates a relative DB in the CWD | 6–10 |
| 16 | Low | No FTS update trigger (latent desync if updates are added) | 48–53 |
| 17 | Low | `latest_with_prefix` is dead code and bypasses suggestion safety | 262–270 |
| 18 | Low | No `ANALYZE` / `PRAGMA optimize`; planner stats stay empty | — |
| 19 | Low | No deterministic tie-break in `search`/`suggest` ordering | 165, 211 |
| 20 | Low | Imports with no timestamps share one `started_at`, so ties are arbitrary | `cmd_import.rs:14` |

> **Status (2026-11-09):** findings 1–12 are fixed in the working tree, along with removal of the dead helper (#17). The remaining low findings (13–16 and 18–20) are open. See [Resolution log](#resolution-log-2026-11-09).

---

## Line-by-line map

| Lines | Item | Verdict |
|---|---|---|
| 5–17 | `default_db_path` | minor XDG empty-string bug |
| 19–54 | `SCHEMA` | FTS is dead weight; no migrations |
| 57–69 | `open` | permissions gap; schema on every open |
| 71–83 | `Entry` | fine |
| 85–119 | `insert` / `insert_batch` | fine; no dedupe; deferred transaction |
| 121–167 | `search_sql` | false FTS comment; LIKE scans; `--dir` exact; `--since` units |
| 169–186 | `search` | unclamped limit; no ranking |
| 188–200 | `search_plan` | gate blind spot |
| 202–212 | `SUGGEST_SQL` | scan; wildcard/case/sensitive bugs |
| 214–241 | `suggest` | over-fetch starvation; limit handling |
| 243–251 | `suggest_plan` | gate blind spot |
| 253–261 | `schema_objects` | fine |
| 262–270 | `latest_with_prefix` | dead code; unsafe if wired |

---

## What is already good

- Every query is parameterized; `exit_filter` is a fixed-string mapping. No SQL injection surface.
- `STRICT` table, WAL, `synchronous=NORMAL`, `busy_timeout`, and a cached prepared statement in `insert_batch`.
- `insert_batch` wraps a 500-row chunk in one transaction and returns a count.
- UUIDv7 IDs are time-ordered and merge-friendly.
- `suggest()` already drops control-character candidates before painting (the right idea, applied in only one path).
- `search_plan`/`suggest_plan` and `schema_objects` exist as observability hooks.
- Integration tests cover insert/search/suggest basics, failed-command exclusion, control filtering, and the daemon drain path.

---

## Bugs

### 1. `--since` is silently a no-op

`started_at` is stored in **nanoseconds** (`cmd_log.rs:7`, `cmd_import.rs:10`), but `search_sql` documents “since (unix secs)” (`db.rs:122`) and compares the raw value (`db.rs:160–164`). A normal epoch-seconds value is ~1.7e9 while stored rows are ~1.7e18, so every row passes.

Reproduction on a four-row DB:

```text
ghostline search --since 1700000000              -> 4 rows (should filter)
ghostline search --since 4102444800000000000     -> 0 rows
```

Only a nanosecond input works. Fix at the CLI boundary: parse seconds, `checked_mul(1_000_000_000)`, or detect magnitude (`< 10^12` means seconds) for compatibility. Keep nanoseconds as the storage unit.

### 2. Case-insensitive SQL match vs case-sensitive suffix strip

`SUGGEST_SQL` uses `cmd LIKE ?1 || '%'`, which is ASCII case-insensitive. `cmd_suggest.rs:65` uses exact `strip_prefix(&buffer)`. When the top-ranked candidate differs in case, the ghost is silently empty even if a valid lower-ranked candidate exists.

Reproduction with `Git status` ranked above `git stash`:

```text
ghostline suggest git        -> (empty)
ghostline suggest --all git  -> Git status
                                git stash
```

Fix options, in order of simplicity:

1. Prefer exact-case candidates first, then case-insensitive variants.
2. On a case-insensitive match, verify the head with `eq_ignore_ascii_case` and slice by byte length (handle non-ASCII char boundaries).
3. Make SQL matching exact with `GLOB` and keep case-insensitive matching as a secondary query.

### 3. Sensitive rows are still suggested

`docs/07-privacy-safety-security.md` says: “never suggest `sensitive=1` rows or `git push --force` style footguns.” `SUGGEST_SQL` has no `sensitive = 0` filter, and `suggest()` never checks the flag.

Reproduction:

```text
ghostline suggest super ->
supersecret --password [REDACTED]
```

The stored value is redacted, but the policy is suppression. Add `AND sensitive = 0` and a regression test. Destructive-command suppression is a separate policy feature that is also not implemented.

### 4. LIKE metacharacters are not escaped

Neither `search` nor `suggest` escapes `%` or `_`. Typing `%` makes the suggest pattern `%%` (match everything); `_it` matches `git`. In practice, the wrong candidates fail `strip_prefix` and the ghost vanishes, but the semantics are wrong and work is wasted.

Fix: `... LIKE ?1 ESCAPE '\'` and escape `\`, `%`, `_` in the bound prefix. For `search`, decide whether user wildcards are a feature; for `suggest`, they must be literal.

### 5. `--dir` is exact equality, but the doc says prefix

`db.rs:122` says “Filters: cwd prefix,” while `db.rs:144–148` uses `h.cwd = ?2`.

```text
search --dir /x marker   -> 0
search --dir /x/y marker -> 1
```

Either implement subtree semantics (`cwd = ?2 OR cwd LIKE ?2 || '/%'`, escaped) or change the comment and help text to “exact directory.” Exact cwd affinity in `suggest` is likely intentional.

### 6. Control bytes reach `ghostline search` output

`suggest()` filters `is_control()`, but `search()` returns stored bytes unchanged and `cmd_search.rs:69` prints them with `println!`. Importing an untrusted HISTFILE can therefore replay terminal escape sequences.

Reproduction: log `echo \x1b[7mINJECTED\x1b[0m`, then:

```text
ghostline search INJECTED | od -An -tx1 | head -3
...
65 63 68 6f 20 1b 5b 37 6d 49 4e 4a 45 43 54 45 44 1b 5b 30 6d
```

`1b 5b` is a raw ESC `[`. This can trigger cursor movement, inverse video, OSC clipboard writes, and Readline desync.

Fix: strip C0 controls at write time in both `cmd_log` and `cmd_import`; also escape non-printables on `search` output so existing DBs are safe.

### 7. Over-fetch can starve clean candidates

`suggest()` asks SQL for `limit + 16` rows, then filters control-tainted rows in Rust and takes `limit`. If the top 16+ matches are tainted, clean rows below are never seen.

Reproduction: 3 older clean `bad clean N` rows, then 25 newer control-tainted `bad\x1b[7mN` rows:

```text
rows matching bad prefix: 28
suggest --all bad -> badblocks    # PATH fallback, history candidates lost
```

Fix: sanitize at write time (best), or filter in SQL with a control predicate, or loop with keyset pagination until `limit` clean rows or exhaustion.

### 8. Negative and oversized limits

SQLite treats `LIMIT -1` as “no limit.”

```text
ghostline search --limit -1 -> all 4 rows
ghostline search --limit 0  -> 0 rows
```

`limit + 16` can also overflow a huge parsed value (debug panic; release wrap to negative, which means no limit). Clamp to a sane `0..=MAX` before building SQL, and use `saturating_add`.

### 9. `--exit` silently ignores anything except `0` / `!0`

`db.rs:131–135` maps only `"0"` and `"!0"`; all other values fall through to no filter. `ghostline search --exit 1` returns unfiltered results. Accept arbitrary numeric exit codes (bind a parameter) or reject unknown values with an error.

### 10. No schema migrations

`CREATE TABLE/INDEX IF NOT EXISTS` is idempotent but cannot evolve an existing database: new columns or indexes never appear. `PRAGMA user_version` is `0`. Before v1 ships, add a version ladder (baseline v1, then `ALTER TABLE` steps). This is also the right place to introduce the suggested covering index.

### 11. Missing FTS update trigger

There is an insert trigger and a delete trigger, but no `history_au`. No update path exists today, so it is latent; any future redaction/dedupe/update will desync `history_fts` silently.

### 12. Empty `XDG_DATA_HOME` creates a relative database

`var_os` returns `Some("")` when the variable is exported empty, and the XDG spec says empty means unset. Result:

```text
cd /tmp/somewhere
XDG_DATA_HOME= ghostline search x
# creates ./ghostline/history.db inside the current directory
```

Fix: `.filter(|s| !s.is_empty())`.

### 13. Dead, unsafe `latest_with_prefix`

Marked `#[allow(dead_code)]`, returns raw control-tainted commands, ignores `sensitive` and `exit_code`, and is inconsistent with `suggest()`’s safety rules. Delete it or route it through the same filters. Do not leave an unsafe helper beside the suggestion path.

---

## Performance measurements

Synthetic database: 500k rows, 100 cwds, 4 000 distinct long-tail commands, 30% matching the `git` prefix. Timings are warm-cache `sqlite3 .timer` query times unless marked “CLI”; CLI numbers include process start, open, schema, and query.

### A. FTS is write-only

`history_fts` plus triggers are created (`db.rs:47–53`), asserted by the bench gate, and fed on every insert — but no query reads it. `search_sql` uses `LIKE '%…%'`.

| Metric | With FTS | Without FTS |
|---|---:|---:|
| 200k-row bulk insert | **1.14 s** | **0.39 s** |
| DB file at 200k rows | **69 MB** | **40 MB** |
| FTS structures at 500k | **~85 MB** (`_data` 58.0 + `_content` 21.9 + `_docsize` 5.5) | 0 |

Search at 500k rows:

| Query | Time |
|---|---:|
| current no-match `LIKE '%…%'` | **94 ms** |
| FTS rare substring | **6.7 ms** |
| FTS miss | **0.4 ms** |
| FTS common token (`git`) | **119 ms** |
| current common token (recency index, early exit) | **0.17 ms** |

FTS is not a universal win: it helps misses and rare tokens, but can be slower than newest-first early exit for common tokens. The problem today is paying for it on every write without using it anywhere.

### B. Suggest full scan

`cmd LIKE ?1 || '%'` is an expression, so SQLite’s LIKE optimization does not apply, and there is no `cmd` index.

| Case | Current CLI | Rewritten query only | Rewrite |
|---|---:|---:|---|
| Common prefix `git` | **64 ms** | **21 ms** | bind `?1 = buffer + '%'`; covering index `(cmd COLLATE NOCASE, cwd, started_at, exit_code)`; `GROUP BY cmd COLLATE NOCASE` |
| Selective prefix `cmd-0399` | **27 ms** | **0.38 ms** | same |

Plan before: `SCAN h` + temp b-tree for GROUP BY + temp b-tree for ORDER BY.  
Plan after: `SEARCH h USING COVERING INDEX idx_sugg_cover (cmd>? AND cmd<?)` + temp b-tree only for the final ORDER BY.

The covering index measures 26.9 MB at 500k rows (before removing FTS). Net DB size still drops substantially if the unused FTS is removed. The 50 ms suggest budget is violated at 500k rows for common prefixes; the 10k-row CI run hides this.

### C. `open()` cost and the 5 s timeout

Measured `open + schema + query`: `--version` 1.29 ms, empty search 1.81 ms → roughly **0.5 ms** overhead on a 229 MB database. That is acceptable, but every `suggest` is a fresh process running full DDL under `busy_timeout=5000` (`db.rs:22, 62`). A lock/checkpoint contention can stall a keystroke for up to 5 s.

Add a read-only fast path for `search`/`suggest`: skip schema when `user_version` matches, open with `SQLITE_OPEN_READ_ONLY`, use a ~100 ms timeout. Keep the long timeout for `log` and `daemon`.

### D. Bench gate blind spot

`search_plan`/`suggest_plan` only prove that plans are non-empty; `check_gate` checks schema object *presence* (`cmd_bench.rs:261, 327`), not `SEARCH` vs `SCAN`. The gate passes with the current suggest full scan. Add plan assertions and a 500k-row query job; docs/08 already asks for a 500k seed, but CI only runs `--n 10000`.

---

## Golden opportunities

1. **Precomputed suggestion aggregate** — `cmd_stats(cmd, n, last_seen)` plus `cmd_dir_stats(cmd, cwd, n, last_seen)`, maintained by triggers or the daemon batch. Suggest becomes an index lookup independent of history size. More machinery than the covering index, but removes the last scan.
2. **Real frecency** — the `SUGGEST_SQL` comment claims “log(freq) and recency decay,” but the SQL is linear `5 * COUNT(*)` and recency only breaks ties. Today 100 runs elsewhere score 500 and beat 10 local runs at 250. Tune weights or add age buckets; add ranking tests (there are none).
3. **Search ranking** — `search()` is newest-first despite the “Frecency-ish” comment. Consider exact-match first, then bm25 (if FTS), then recency/dir boost. Also decide multi-token semantics: `LIKE '%foo bar%'` requires an exact phrase; FTS gives AND semantics.
4. **Dedupe imports** — re-importing the same HISTFILE duplicates every row. Add a dedupe key/index or an `--dedupe` pass. Related: timestamp-less imports share one `fallback_ns`, so ordering ties are arbitrary. Add a `rowid`/`id` tie-break or synthesize decreasing timestamps.
5. **Retention and vacuum** — nothing deletes old rows; the database grows forever. Add `ghostline forget`, age/size caps, and `PRAGMA auto_vacuum=INCREMENTAL` at creation or periodic `VACUUM`.
6. **`PRAGMA optimize` / `ANALYZE`** — never run; planner statistics stay empty. Run after imports and periodically from the daemon.
7. **Write-time sanitization** — strip C0 controls in `build_entry`/`parse_histfile`. This fixes search injection and over-fetch starvation, and removes the magic `+16`.
8. **UUID storage** — `id TEXT` (36 bytes) costs ~9 MB of index at 500k rows. A 16-byte BLOB UUID saves roughly 10 MB; the ID is internal.
9. **Read-only hot path** — short timeout, no DDL, no chmod, works on read-only mounts, and avoids creating an empty DB just because `search` ran.
10. **Directory privacy** — create `~/.local/share/ghostline` as `0700`. That protects `history.db-wal` and `-shm` (currently `0644` while the main DB is `0600`) without chasing sidecar files. Surface `set_permissions` failures.
11. **External-content FTS** — if FTS stays, `content='history', content_rowid='rowid'` removes the duplicated ~22 MB; tuning `detail` may shrink the 58 MB trigram index.
12. **`BEGIN IMMEDIATE` for batches** — `unchecked_transaction` is deferred; under multi-writer contention, lock upgrades can burn the 5 s timeout. Use `TransactionBehavior::Immediate` for `insert_batch`.

---

## Suggested patch order

1. **Correctness and privacy**
   - `--since` units
   - case-consistent ghost suffix
   - `sensitive = 0`
   - LIKE escaping
   - limit clamping
   - `--dir` semantics or docs
   - control sanitization at write + `search` output
2. **Scale**
   - bind `buffer || '%'`
   - covering suggest index + collated GROUP BY
   - clamp/limit in SQL
   - decide FTS fate (wire into search or drop) and update bench/docs
3. **Hardening**
   - `user_version` migrations
   - `0700` data directory
   - read-only fast open with short timeout
   - FTS update trigger
   - remove dead code
4. **Product**
   - real frecency
   - search ranking
   - import dedupe and tie-breaks
   - retention
   - `PRAGMA optimize`

---

## Reproduction appendix

Build and seed a database:

```bash
cargo build --release
BIN=target/release/ghostline
```

`--since` and limits:

```bash
$BIN log --db /tmp/t.db --shell bash --cwd /x --exit 0 --session s 'git status'
$BIN search --db /tmp/t.db --since 1700000000 | wc -l      # all rows: bug
$BIN search --db /tmp/t.db --limit -1 | wc -l              # all rows: bug
```

Case-sensitive suffix:

```bash
for i in 1 2 3 4 5; do
  $BIN log --db /tmp/t.db --shell bash --cwd /x --exit 0 --session s 'Git status'
done
$BIN log --db /tmp/t.db --shell bash --cwd /x --exit 0 --session s 'git stash'
$BIN suggest --db /tmp/t.db --cwd /x git                    # empty: bug
```

Sensitive suggestion:

```bash
$BIN log --db /tmp/t.db --shell bash --cwd /x --exit 0 --session s 'supersecret --password hunter2'
$BIN suggest --db /tmp/t.db --cwd /x super                  # suggests redacted row: policy bug
```

Terminal injection:

```bash
$BIN log --db /tmp/t.db --shell bash --cwd /x --exit 0 --session s "echo $'\e'[7mINJECTED$'\e'[0m"
$BIN search --db /tmp/t.db INJECTED | od -An -tx1 | head -3 # raw 1b 5b bytes
```

Query plans:

```sql
EXPLAIN QUERY PLAN
SELECT cmd,
       (100 * MAX(CASE WHEN cwd = ?2 THEN 1 ELSE 0 END))
     + (10  * SUM(CASE WHEN cwd = ?2 THEN 1 ELSE 0 END))
     + (5   * COUNT(*)) AS w,
       max(started_at) AS last_seen
FROM history h
WHERE cmd LIKE ?1 || '%' AND cmd != ?1 AND exit_code = 0
GROUP BY cmd
ORDER BY w DESC, last_seen DESC
LIMIT ?3;
-- before: SCAN h + temp b-trees
-- after:  SEARCH h USING COVERING INDEX ... + one temp b-tree
```

Query plans and SQL timings were measured with the `sqlite3` 3.54.1 CLI on a warm-cache WAL database; behavioral repros and CLI end-to-end timings used the repo's bundled `rusqlite` build. Absolute latency will vary by machine and SQLite version; the scan-vs-index plan difference and the write-cost ratio are the reproducible parts.

---

## Resolution log (2026-11-09)

| Finding | Status | Change |
|---|---|---|
| 1. `--since` units | Fixed | `cmd_search::since_to_ns` converts Unix seconds to nanoseconds (nanosecond inputs pass through); invalid/negative values now error. |
| 2. Case-mismatched ghost | Fixed | `cmd_suggest::ghost_suffix` compares the head case-insensitively, slices by byte length, skips empty/exact matches, and falls through to the next ranked candidate. |
| 3. Sensitive suggestions | Fixed | `SUGGEST_SQL` now requires `sensitive = 0`; regression test added. |
| 4. Unused FTS | Fixed | `search` uses an FTS5 trigram phrase (`MATCH ?`) for queries of 3+ characters, with a LIKE fallback for 1–2 character queries; special characters are quoted literally. |
| 5. Suggest full scan | Fixed | Bound `buffer || '%'` as one parameter, added the partial covering index `idx_cmd`, grouped by `cmd COLLATE NOCASE`, clamped limits, and escaped LIKE wildcards. |
| 6. LIKE wildcards | Fixed (search + suggest) | `escape_like` + `ESCAPE '\'`; wildcards in queries/buffers are literal. |
| 7. `--dir` exact vs prefix | Fixed | `--dir` now matches the directory and its descendants. The query uses an exact equality plus a `[dir/, dir0)` range, so `idx_cwd` still serves it via a multi-index OR. Trailing slashes and `/` are normalized. |
| 8. Control bytes in search output | Fixed | `redact::strip_controls` removes C0/C1/DEL at log and import time, and `search` sanitizes `cwd`/`cmd` on output so pre-existing databases are safe too. |
| 9. Suggest over-fetch starvation | Fixed | Control-tainted rows are still filtered in Rust, but the fetch size now doubles until enough clean candidates are found or the prefix is exhausted. Write-time sanitization keeps the first query sufficient for new data. |
| 10. Negative/oversized limits | Fixed | `db::search` clamps `limit` to `0..=10_000` before binding, so `--limit -1` no longer means "no limit" and huge values cannot overflow the suggest over-fetch. |
| 11. WAL/SHM permissions | Fixed | `db::open` creates missing data directories `0700` and chmods the DB plus `-wal`/`-shm` sidecars `0600`; `insert_batch` re-applies after writes because SQLite recreates sidecars lazily. |
| 12. No schema migrations | Fixed | `PRAGMAS` are split from the baseline DDL and `PRAGMA user_version` tracks `SCHEMA_VERSION = 1`. `open()` applies the baseline only on upgrade and rejects databases written by a newer version. |
| 17. Dead helper | Fixed | `latest_with_prefix` and its unused `OptionalExtension` import removed. |

Measured on the synthetic 500k-row database:

| Query | Before | After |
|---|---:|---:|
| `suggest` common prefix (`git`) | 64 ms | 24 ms |
| `suggest` selective prefix (`cmd-0399`) | 27 ms | 2.7 ms |
| `search` no match | 94 ms | 2.4 ms |
| `search` rare substring | ~17 ms | 6.0 ms |
| `search` common substring | 0.17 ms (early exit) | ~37 ms (FTS) |
| `search` short (1–2 char) miss | 94 ms | ~112 ms (LIKE fallback, unchanged) |

The FTS path trades the old early-exit win on very common substrings for large wins on misses and rare terms; both common and miss cases stay within the 50 ms search budget at 500k rows. Short queries cannot use trigrams and retain the old LIKE cost.

Tests added: `since_to_ns` unit tests, `ghost_suffix` case/boundary tests, sensitive suppression, FTS/LIKE search behavior, wildcard literals, a plan assertion that suggest uses `idx_cmd`, `--dir` subtree/trailing-slash/root behavior, negative limits, suggest starvation, control stripping at log/import/search time, schema migration/version rejection, and Unix `0600`/`0700` permissions. `cargo test` is green (61 unit + 3 pty).

For finding 9, a SQL-level `cmd NOT GLOB '*[control]*'` filter was measured but added ~30 ms to a common-prefix suggest at 500k rows (about 20 ms -> 53 ms). Adaptive over-fetch plus write-time sanitization keeps the common case at ~25 ms while still guaranteeing clean candidates from legacy databases.
