# 07 — Privacy, Safety, Security

## Privacy model: local-first

- Default: logging local-only, DB `chmod 600` (`~/.local/share/ghostline/history.db`), cache `~/.cache/ghostline/`, no sockets in default path, no telemetry, no auto-update phone-home.
- Opt-in only: NL cache, cloud fallback, multi-machine sync (E2EE when built). Everything clearable: `ghostline forget <pattern>`, `--last-n`, `--forget` (NL cache).
- Never store wholesale `env`, raw IPs, or full `/proc`. Allowlist (`SHELL,TERM,HOSTNAME,USER,TZ,LANG`) + hashed projections. `ssh_ctx` = hash, not IP.
- CLI args also leak via `ps`/`/proc`/auditd/scrollback — history redaction covers disk only. Document defense-in-depth: prefer env-var / secret-manager / interactive `-p` over CLI args.

## Secret redaction (at capture + before send)

Redact twice: (1) before persist/cache, (2) before any cloud send. If secret detected → mark `sensitive=1`, skip NL caching.

Core patterns (from gitleaks/Warp core; full integration later):

- `sk-[A-Za-z0-9]{48}` (OpenAI), `sk-ant-…{80,120}`, `ghp_|gho_|ghu_|ghs_|github_pat_`, `AKIA[A-Z0-9]{16}`, `AIza[0-9A-Za-z-_]{35}`, `xox[bpa]-…`, `rk_|sk_(live|test)_…`, JWT `ey[…]\.[…]\.[…]`, `-----BEGIN .*PRIVATE KEY-----`.
- Generic: `(?i)(password|passwd|pwd|secret|token|api[_-]?key|auth)[\s=:]+[^\s]+`, `--password\s+\S+`, `Bearer\s+[^\s]+`, `(postgres|mysql|mongodb|redis)://[^:@]+:[^@]+@`.
- Replace with `[REDACTED]` preserving key name. Custom `rules.conf` (`regex|||replacement`), allowlist/blocklist, per-project `.ghostlineignore`.
- Also suppress from ghost suggestions: never suggest `sensitive=1` rows or `git push --force` style footguns (fish/atuin leak lesson). `GHOSTLINE_IGNORE="*password* *secret* *token*"` mirrors `HISTIGNORE`.

Later: full gitleaks ruleset (200+), reversible tokenization, per-project `.wick.yaml`-style config, audit log.

## Destructive-command tiers (authoritative, fail-closed)

Tokenize with shell-aware lexer: respect quotes; unwrap `sudo/doas`, `env/timeout/nice`, `sh -c "…"`, `xargs`/`find -exec`, `|/&&/||/;/$( )` recursively. Token-boundary match (so `iptables -L` stays SAFE). Parse error → treat as CAUTION minimum. Never trust model `isDangerous=false`.

| Tier | Examples | Behavior |
|---|---|---|
| **BLOCKED** (refuse even with confirm) | fork bomb (`:(){` + `:|:`), `rm -rf /`, `mkfs`, `dd of=/dev/*`, `iptables -F/-X`, `kill -9 -1`, `curl…|sh`, `> /dev/sda` | refuse + log block for audit |
| **DANGEROUS** (always explicit confirm, no whitelist bypass) | `rm/rmdir/shred/unlink`, `find -delete/-exec rm`, `git reset --hard` / `clean -fd` / `push --force` / `branch -D`, `chmod -R 777`, `docker *prune` / `rm -v`, `rsync --delete`, `truncate`, `eval`, `bash -c`, any `sudo/doas` wrapping the above | typed `yes` or double confirm + `--explain` preview; non-interactive/piped = block, don't auto-run |
| **CAUTION** (confirm once / show explain) | `mv/cp -f`, `git rebase/merge/push`, `kubectl apply/delete`, `docker run --privileged`, `pkill/killall`, `chattr ±i` | show explain, single confirm |
| **SAFE** | `ls/cat/grep/find`, `git status/log/diff`, `mkdir/touch` | insert/run normally |

UX default: print + insert-into-buffer, never execute. NL path always offers `[E]xecute / [D]escribe / [A]bort`.

## Other safety rails

- Never auto-`cd`, auto-`sudo`, or auto-correct destructive semantics (space in `rm -rf / tmp/foo`, `chmod -R 777`) — explain only.
- `sudo -i` / EUID=0: write to `/root/…`, never chown user DB as root (TOCTOU/setuid-adjacent care; Rust memory safety helps, still validate paths).
- Crash reports: strip cmd/env; hashed only.
- Sync (future): E2EE, device_id hashed, encrypted at rest on server (atuin model), per-device revoke.
