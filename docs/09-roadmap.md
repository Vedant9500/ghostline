# 09 — Roadmap

## v1 — lean slice (ship fast, feel instant)

No daemon, no sync, no model download. Offline rules + tldr bundle only.

1. `ghostline init {bash,zsh,fish}` + `ghostline log` (sync SQLite WAL, redaction, `busy_timeout`).
2. Ghost suggestion: history-prefix + dir-aware frecency rank + tldr-template fallback.
3. `thefuck`-lite top rules: cd-case, path ≤2, cmd typo, git subcmd, flag typo, sudo-hint, mkdir-p.
4. `ghostline search` (FTS5 trigram + `--dir/--host/--session/--exit/--since`) + fzf bind (opt-in) + `ghostline import --from histfile`.
5. `ghostline stats`, `ghostline explain`, `ghostline forget`, `ghostline tldr --update`.
6. `ghostline bench` + CI perf gate (`08-performance-budgets.md`).
7. NL-rules baseline (top-100 intents → tldr slot-fill), no model yet.

Exit criteria: hook <2ms median, cold log <5ms, search <50ms @500k, redaction corpus green, identical ghost UX on 3 shells.

## v1.1 — daemon + local NL

- Queue-append + `ghostline daemon` (batch 500/1s, FTS off hot path, systemd/launchd agent, socket-prefer with file fallback).
- NL-local: bundled `tldr.en.zip` + small GGUF (`Qwen3.5-0.8B q4` default, `Qwen2.5-Coder-1.5B` opt-in) via llama.cpp resident server; SQLite NL cache; `--explain`.
- Alias advisor, per-dir habits (F7), next-cmd Markov (F8), weekly stats digest.
- `ghostline import --from zoxide`, smart-cd resolve, flag tutor.

## v2 — sharing + cloud (opt-in)

- Team `*.cheat` repos (navi-compat), sanitized export, session replay.
- Cloud fallback `--cloud` (multi-provider, redaction-gated), chat/REPL refine.
- E2EE multi-machine sync (schema already UUIDv7 + Lamport-ready).
- Larger model option (3–4B), LoRA on user-accepted pairs, semantic cache.
- Full gitleaks integration, audit log, second-model risk review.

## Backlog (from brainstorm, unordered)

F14 smart-cd · F15 flag tutor · F16 undo-guard · F17 session replay · F18 sanitized export · F19 team cheats · F20 cloud · F21 hotkey widget · F22 sync · F23 failure coach (`ghostline why`) · F24 time-of-day stats.

## What NOT to build

Prompt theme, terminal emulator, Windows-native pwsh, hosted accounts/telemetry, auto-execution.
