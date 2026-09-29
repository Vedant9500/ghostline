# 03 — Features (Polished Spec)

## P0 — must have for v1

### F1. Universal command logging
- Log every command with: `cmd, cwd, exit_code, duration_ns, started_at/ended_at, hostname, session_id, shell, tty, ssh_ctx, tmux, container, sudo_user, git_repo/branch, sensitive flag`.
- Shells: `bash ≥3.2` (bash-preexec / 5.3 native), `zsh` (preexec/precmd), `fish` (fish_preexec/postexec + `$CMD_DURATION`).
- Rules: skip `ghostline` itself, completions, `ignorespace`-prefixed (` `), non-interactive (`$-` lacks `i`, `TERM=dumb` piped). Leave native `HISTFILE` on.
- Storage: local SQLite WAL (`~/.local/share/ghostline/history.db`, `chmod 600`). Schema sync-ready (UUIDv7). See `06-architecture.md`.

### F2. Inline ghost suggestion (faint font)
- As-you-type suffix ghost, dim-styled (`SGR 2` + gray `38;5;244`, never ANSI inside ZLE buffer — use `region_highlight` on zsh).
- Source order: (1) exact-prefix history in this dir → (2) fuzzy frecency-ranked → (3) tldr template if no history hit (e.g. typing `tar` shows `tar -xzf {{file}}` dimmed).
- Accept full on `→`/`End`/`Ctrl-F` at EOL only; partial word on `Alt-→`; dismiss on diverge/`Esc`; never execute on accept. Separate `accept+execute` bind.
- Guards: `BUFFER_MAX_SIZE` (~20–50 chars skip), `PENDING/KEYS_QUEUED` skip, multiline disables ghost for that line.
- See `04-ux-spec.md`.

### F3. tldr-inline
- Bundled `tldr.en.zip` (~2.3MB) offline. Update via `ghostline tldr --update`.
- When history has no hit, ghost shows most-common tldr example with `{{placeholders}}` highlighted; `Tab` cycles examples; typed args fill placeholders.
- `ghostline explain <cmd>` = reverse mode (what does this do, is it dangerous).

### F4. Instant mistake repair (thefuck-lite)
- Top-15 rules in Rust, <50ms: `sudo` on EACCES, `cd` case (`downloads`→`Downloads`), path Damerau ≤2, command-name typo (`gti`→`git`), git subcommand/branch, flag typo, swapped tokens, `mkdir -p` suggestion, extension stem.
- UX: one line after failure: `did you mean: git checkout main? [Enter run | e edit | d dismiss]`. Max 3 candidates. Never auto-run destructive.
- Learn dismissals: 2× dismiss suppresses pattern 24h.

### F5. `ghostline search` (history UI)
- FTS5 trigram + filters: `--dir`, `--host`, `--session`, `--exit 0/!0`, `--since`, `--limit`.
- fzf/skim binding (`Ctrl-R` opt-in, never hijack `↑` by default). `F2`/flag to delete entry. Import: `ghostline import --from histfile`.

### F6. Privacy + safety rails
- Redact before persist + before any cloud send; `sensitive=1` flag; `GHOSTLINE_IGNORE` glob deny-list; `ghostline redact --pattern`.
- Destructive tiers: BLOCKED (fork bomb, `rm -rf /`, `mkfs`, `dd of=/dev/*`, `curl|sh`) vs DANGEROUS (confirm, no bypass) vs CAUTION vs SAFE. Default insert-into-buffer, never execute. See `07-privacy-safety-security.md`.

## P1 — v1.1 (high value, small cost)

- **F7. Per-dir habits:** `P(cmd|dir)` TF-IDF boost; project-root detection (git root / `package.json` / `Cargo.toml` / `go.mod`); ghost prefers `npm test` vs `cargo test` per repo.
- **F8. Next-command prediction:** order-1 Markov `P(next|prev)` backoff; after `git add .` suggest `git commit`; after `mkdir x` suggest `cd x`.
- **F9. Alias advisor:** `ghostline stats` detects repeated long commands → `alias gco='git checkout'` suggestion; one-command install to rc file.
- **F10. Stats/insights:** `ghostline stats [--by dir|cmd|hour]` — top cmds, failure rate, slowest, time-of-day. Weekly digest opt-in.
- **F11. NL-rules baseline (ON HOLD for anything model-based):** rules (top-100 intents) + bundled tldr only. Local GGUF model is parked — breaks the lightweight budget. See `09-roadmap.md`.
- **F12. Daemon + queue:** `O_APPEND` queue + `ghostline daemon` batching (500 rows/1s), FTS indexing off hot path, systemd/launchd user agent.
- **F13. Delete/redact tooling:** `ghostline forget <pattern>`, `ghostline forget --last-n`, per-project `.ghostlineignore`.

## P2 — later / brainstormed extras

These came out of research as differentiators. Not v1, tracked here so they aren't lost:

- **F14. Smart `cd`:** case-insensitive + fuzzy dir resolve + zoxide import (`ghostline import --from zoxide`); `z`-style jump built in if zoxide absent.
- **F15. Flag tutor:** on flag error, show valid flags from completion spec + one-line meaning (from tldr/man), not just the spelling.
- **F16. Undo-guard:** pre-exec scan for `rm`, `git reset --hard`, `push --force`, `chmod -R` → typed-`yes` confirm + `--explain` preview; dry-run hint (`--dry-run` where supported).
- **F17. Session replay:** `ghostline replay <session>` re-prints a session's successful commands as a runnable script (onboarding / postmortem).
- **F18. Dotfile-safe export:** `ghostline export --sanitized` for sharing (secrets stripped, home → `~`).
- **F19. Team cheatsheets:** git-repo `*.cheat` sharing (navi-compatible), no server needed.
- **F20. Cloud fallback — ON HOLD:** parked with local models (breaks offline + latency budget). See `09-roadmap.md`.
- **F21. Voice/hotkey hooks:** documented shell hotkeys (`Ctrl-G` navi-style widget, `Alt-e` NL-prompt) without stealing core binds.
- **F22. Multi-machine sync (E2EE):** only after daemon + redaction are solid; UUIDv7 + Lamport clock already reserved.
- **F23. Failure coach:** `ghostline why` explains last non-zero exit (compiles git/stderr `did you mean`, EACCES → sudo hint, ENOENT walk).
- **F24. Time travel stats:** "what do I usually run Monday 9am in ~/infra?" — weak signal, only if data-rich.

## Explicitly out

- Prompt theme, font management, terminal emulator itself.
- Windows-native (WSL is fine, native pwsh deferred).
- Server-side analytics, accounts, telemetry.
- Auto-execution of suggestions (never).

## Acceptance mapping

| Brief ask | Feature |
|---|---|
| track all commands | F1 + F5 |
| tldr faint format | F2 + F3 |
| typos (`cd downloads`) | F4 |
| per-dir habits | F7 |
| next-command | F8 |
| NL | F11 (rules in v1, model in v1.1) |
