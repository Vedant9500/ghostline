# 05 — Intelligence Engine

## 1. Mistake taxonomy

| # | Class | Example | Signal | Fix |
|---|---|---|---|---|
| 1 | Case error | `cd downloads` vs `~/Downloads` | ENOENT + case-insensitive match in target dir | scandir + `lower→real` map |
| 2 | Path typo ≤2 edits | `cd Documnets` | ENOENT + Damerau ≤2 to sibling | BK-tree over parent `ls` |
| 3 | Command typo | `gti status`, `dokcer ps` | exit 127 | edit distance over `$PATH` + builtins + aliases + history verbs |
| 4 | Git subcmd/branch | `git chekout mian` | exit 1 + stderr `did you mean` | BK-tree over `git help -a` + branches + refs |
| 5 | Flag typo | `ls --colr` | exit 2 `unrecognized option` | flag vocab from `--help`/completions; handle `-` vs `--` |
| 6 | Swapped tokens | `checkout git main` | token[0] unknown, token[1] known cmd | bigram swap check |
| 7 | Missing sudo | `apt install x` → EACCES | `Permission denied` + cmd in {apt, systemctl, docker, mkdir /etc…} | prepend `sudo` (confirm) |
| 8 | Missing `mkdir -p` | `touch a/b/c.txt` | ENOENT on intermediate | walk components, suggest `mkdir -p $(dirname)` |
| 9 | Missing ext/prefix | `python script` vs `script.py` | ENOENT + stem match | stem + extension expansion |
| 10 | Dangerous semantics | `rm -rf / tmp/foo` (space) | high-risk | never auto-correct; explain + confirm |
| 11 | Keyboard adjacency | `gerp` → `grep` | Damerau-1 + QWERTY adjacency | weighted substitution cost |

80% of value from ~15 rules (thefuck corpus lesson): sudo, git, cd-case, mkdir-p, alt-space, quotes.

## 2. Algorithms

- **Damerau-Levenshtein (true DL, not OSA):** insert/delete/substitute + transposition (`gti`→`git`). ~15–30% of typos are transpositions. Weighted: insert/delete 1.0, substitute 1.0 (0.3 if QWERTY-adjacent, 0.1 if case-only), transposition 0.7.
- **SymSpell (symmetric delete):** precompute deletes to distance 2 → O(1) lookup. For bounded vocabs: `$PATH` (~2k), git subcmds (~150), per-cmd flags (~50). Not for full FS walk.
- **BK-trees:** metric tree, radius 1–2 prunes 90%+. For per-cwd filenames (10–10k), branches, history (5k). Build lazily per `cwd`, TTL ~2s, query <1ms at 10k.
- **Tries / Levenshtein automata:** flag prefix (`--ver`→`--version`) + as-you-type fuzzy prefix (Schulz & Mihov traversal). Flag vocab from fish completions (cleanest source).
- **Threshold:** reject if `D_w > max(2, len(t)/3)` (git's proportional rule — avoids `rm`→`vim` absurdity).

## 3. Case-insensitive resolve on case-sensitive FS

1. On ENOENT, split path, find first failing component.
2. Single `scandir(parent)` (no recursive walk), map `lower(name)→[real]`.
3. Exact-lower hit → suggest real casing (ties → frecency). Else fuzzy Damerau ≤2 case-folded.
4. Reconstruct, verify `exists`. Never auto-`cd`; `did you mean: cd Downloads? [Enter]`.
5. Cache `dir_mtime→listing` 1–2s. <5ms for <5k entries. Respect user `nocaseglob` setting; display correct case to teach habit.

## 4. Ranking formula (v1)

For candidate `c` given typed `t`, dir `d`:

```
score(c|t,d) = 0.45·Sim + 0.25·Freq + 0.15·Rec + 0.10·DirAff + 0.05·Succ
```

- `Sim = 1 − D_w(t,c)/max(len t, len c)`, require `Sim>0.5`.
- `Freq = log(1+N(c,d))/log(1+N_max(d))`, backoff `0.7·dir + 0.3·global` when sparse.
- `Rec = exp(−λ·Δt_hours)`, `λ=0.05` (~14h half-life). Mozilla buckets (1h/1d/1w) also fine.
- `DirAff = N(c,d)/N(c,all)` (1.0 if only used here) — TF-IDF intuition.
- `Succ = (s+2)/(n+4)` Laplace-smoothed success rate; veto if `Succ<0.2 && n>5`.
- Display iff `score>0.45`; max 3 sorted.

Example: `cd downloads` → `Downloads` (Sim .96) scores ~0.88 vs `download-old` ~0.35. Clear winner.

Tuning: log accept/reject, fit logistic regression weekly to personalize. Keep weights documented (anti-mcfly-opacity).

## 5. Learning: per-user, per-dir, next-cmd

Store: `history(cmd, cwd, exit, duration, ts, host)` + `corrections(typed, accepted, rejected, cwd, ts)`.

- **Personal alias map:** `gti`→`git` accepted 5× → ephemeral weight 0.9 for that user only.
- **Per-dir verb prior:** `npm test` in `~/web`, `cargo test` in `~/rust`, `make` in `~/c`. Condition on `cwd` (or git root / marker file).
- **Confusion matrix:** per-user substitution counts; `cost(a→b) = −log P(a typed | b intended)`. Start QWERTY-generic, adapt after ~50 corrections. Decay 0.95^weeks after 30d unless repeated.
- **Next-cmd Markov order-1:** `P(next|prev)` per cwd-cluster (`git add .`→`git commit`, `vim`→`pytest`). Order-2 with backoff only (`mkdir x→cd x→touch`). Serves as tie-breaker + proactive ghost.
- **Frecency (Mozilla-adapted):** `freq × recency_boost` — beats pure frequency (atuin/zoxide lesson).

## 6. NL engine (local-first)

Order: exact cache → fuzzy cache → rules (top-100 intents) → local model → (opt-in) cloud.

- **Rules baseline:** keyword/regex → fill tldr `{{slot}}`. Covers `untar`, `find large files`, `port in use`, `git undo`. <5ms, deterministic. Constrain first token to utility list.
- **Local model (v1.1):** `Qwen3.5-0.8B q4_k_m` (~400MB, <500ms Apple Silicon / 1–2s CPU) or `Qwen2.5-Coder-1.5B Q4_K_M` (~941MB, ~1s, 0.62 pass). Alternatives: `Gemma3-270M`. Stack: llama.cpp / llama-server + GGUF, resident daemon to avoid 2–4s cold start. Prompt: `Output exactly one line: single POSIX command. No prose, no fences.` `temperature=0`, 64-token cap, inject `$OSTYPE/$SHELL`. Datasets: `NL2SH` (40k train / 600 test), tldr `description↔command` pairs.
- **Cache:** SQLite `~/.cache/ghostline/nl.db`, key `normalize(nl)+os+shell`, `chmod 600`, never cache secret-flagged inputs, user-clearable (`--forget`).
- **Cloud fallback (opt-in only):** `--cloud --provider ollama/openai/phind`, sgpt/aichat pattern: OS/shell-aware prompt, stdin context, `--explain` reverse, insert-not-execute, redact-before-send.
- **Safety (authoritative, fail-closed):** tokenize shell-aware (quotes, `sudo/env/timeout`, `|/&&/;/$( )` recursion, token-boundary not substring). Tiers in `07-privacy-safety-security.md`. Non-interactive/piped = block dangerous, never auto-run. Never trust model `isDangerous=false`.
