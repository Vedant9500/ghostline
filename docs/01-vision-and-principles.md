# 01 — Vision and Principles

## Problem

1. Shell history is dumb. `Ctrl-R` is reverse-chronological, not contextual. It doesn't know you run `cargo test` in `~/proj/rust` and `npm test` in `~/proj/web`.
2. Knowledge is split: your history knows *what you did*, `tldr` knows *how commands work*, `thefuck` knows *how you mess up*. No one tool connects all three at typing time.
3. Existing smart tools each tax you: atuin needs sync-server thinking, mcfly is unmaintained + opaque, zsh-autosuggestions is zsh-only + prefix-only, thefuck costs 500ms–2s of Python, fish is great but non-POSIX and leaks secrets into suggestions.
4. Learning cost is high: users Google `how to untar`, copy-paste, forget, repeat.

## Vision

One binary, three jobs, zero perceived latency:

- **Remember everything** (with context: cwd, exit, duration, session, host).
- **Suggest before you finish** (faint ghost text: history → dir-aware rank → tldr template).
- **Repair after you fail** (one-line `did you mean`, never auto-executes danger).

Plus an offline NL escape hatch for "I don't know the structure" moments.

## Original idea (preserved)

From the initial brief:

- [x] Lightweight, near-instant tracker of all commands run.
- [x] Bundled `tldr` so unknown structure shows as faint format preview.
- [x] Pattern/typo tolerance: `cd downloads` → `cd Downloads`.
- [x] Smarter over time: per-dir habits, next-command prediction (`git add` → `git commit`).
- [x] NL understanding ("free up port 8080").

These are all kept as P0. See `03-features.md`.

## Design principles

1. **Prompt is sacred.** Budget: <2ms p50, <5ms p99 added render cost. Never fork/exec in `self-insert`. Never block on network. See `08-performance-budgets.md`.
2. **Local-first, offline by default.** Zero sockets in default path. Cloud LLM only on explicit `--cloud` flag.
3. **Single static binary.** `curl | sh` install, `x86_64 + aarch64` Linux/macOS. No Python/Node runtime. Target <4MB (Rust musl).
4. **Shell-agnostic UX.** Identical ghost-text feel on `bash ≥3.2`, `zsh`, `fish`. No zsh-only or fish-only tricks in core promise.
5. **Explainable, not black-box.** Ranking weights are documented and tunable (see `05-intelligence-engine.md`), unlike mcfly's NN.
6. **Never execute for you.** Accept ≠ run. Destructive commands always need explicit confirm. No silent `sudo` prepend.
7. **Privacy by construction.** Redact before persist, `chmod 600` DB, `GHOSTLINE_IGNORE` deny-list, no raw IP/env sync.
8. **Leave native history on.** Don't break `↑`, `HISTFILE`, or existing `Ctrl-R`. Mirror `HISTCONTROL=ignorespace:erasedups`. Opt-in hijack only.

## Non-goals (v1)

- No hosted sync server. Schema is sync-ready (UUIDv7) but server is deferred.
- No prompt theme / statusline. Cooperate with starship/p10k, don't replace.
- No Windows PowerShell native support in v1 (design shouldn't preclude it).
- No cloud account, telemetry, or auto-update phoning home.
- No full terminal UI. `search` leans on fzf/skim; inline ghost is the UI.

## Success criteria

- Hook overhead <2ms median on NVMe (`08-performance-budgets.md` harness).
- Suggestion hit acceptance rate >30% after 1 week of use (fish reports ~80% as stretch target).
- Typo-repair precision: top-1 correct >85% on `D(t,c) ≤ 2` class.
- Cold `ghostline log` <5ms, `ghostline search --limit 20` on 500k rows <50ms.
- Zero secret-leak reports in redaction test corpus.
