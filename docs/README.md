# GHOSTLINE — Docs Index

> Name: **GHOSTLINE** (`ghostline`, alias `gl`) — ghost text on your line.
> Goal: really lightweight, near-instant CLI that tracks everything you run, suggests what you meant, and teaches you the right flags inline.

## Map

| File | What it covers |
|------|----------------|
| [01-vision-and-principles](01-vision-and-principles.md) | Problem, vision, non-goals, design principles |
| [02-competitive-landscape](02-competitive-landscape.md) | atuin, mcfly, fish, fzf, zoxide, thefuck, tldr/navi, carapace, prompts — gaps to own |
| [03-features](03-features.md) | Full feature spec: core + brainstormed extras, polished idea |
| [04-ux-spec](04-ux-spec.md) | Ghost text, faint styling, keybindings, tldr-inline, correction flows |
| [05-intelligence-engine](05-intelligence-engine.md) | Typo taxonomy, algorithms, ranking formula, per-dir + next-cmd learning, NL engine |
| [06-architecture](06-architecture.md) | Rust + SQLite WAL design, schema, shell hooks, daemon path, env variables matrix |
| [07-privacy-safety-security](07-privacy-safety-security.md) | Secret redaction, destructive-command tiers, local-first privacy |
| [08-performance-budgets](08-performance-budgets.md) | Latency budgets, benchmarking method, CI gates |
| [09-roadmap](09-roadmap.md) | v1 / v1.1 / v2 slices, deferred ideas |
| [10-open-questions](10-open-questions.md) | Decisions still open, variables to validate |

## One-paragraph pitch

GHOSTLINE is a single static binary + 3-line shell hook (`bash`/`zsh`/`fish`) that logs every command with context (cwd, exit code, duration, session, host) into local SQLite, then uses that data to do three things other tools keep separate: (1) **inline faint suggestion** as you type (history + dir-aware + tldr template fallback), (2) **instant mistake repair** (`cd downloads` → `Downloads`, `gti` → `git`, missing `sudo`/`mkdir -p`), (3) **NL fallback** (`ghostline "free port 8080"` → `lsof -ti:8080 | xargs kill`) fully offline by default. No daemon in v1, no network in default path, prompt overhead <2ms.

## Status

Pre-implementation research. No code yet. These docs are the spec to build from.
See [09-roadmap](09-roadmap.md) for the lean v1 slice.
