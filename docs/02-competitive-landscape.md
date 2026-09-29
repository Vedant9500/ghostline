# 02 — Competitive Landscape

Researched: atuin, mcfly, zsh-autosuggestions, fish built-in, fzf history, zoxide/autojump, thefuck (+successors), tldr clients (tealdeer/tlrc), navi, cheat.sh, howdoi, carapace/cobra completions, starship/p10k.

## Summary per tool

### atuin — closest reference
- Rust + SQLite, context (cwd/exit/duration/session/host), full-screen `Ctrl-R`, filter modes, `stats`, optional E2EE sync/self-host.
- Strengths: durability vs corrupt `history.txt`, multiline-safe, sync across machines.
- Weaknesses: typing lag reports at 6k+ records, up-arrow hijack annoyance, server-setup friction, plaintext local DB, no inline ghost.
- Take: reuse schema ideas (session/host/cwd/exit/duration) and UUID-ordering for future sync. Avoid sync weight in v1.

### mcfly — ranking inspiration
- Rust + SQLite, tiny NN rank: frecency + cwd + prior cmd + exit status. `F2` delete, `%` wildcard.
- Strengths: dir-aware ranking, failed-cmd demotion, real-time light.
- Weaknesses: semi-unmaintained, DB-migration panics, splits multiline, opaque ranking, no sync/stats.
- Take: copy dir/exit-aware signals, but make ranking transparent + tunable (documented weights).

### zsh-autosuggestions — ghost-text pattern to copy
- Pure shell, `POSTDISPLAY` suffix + `region_highlight`, async via `zpty`.
- Strengths: unobtrusive, `→` accept.
- Weaknesses: prefix-only, no fuzzy/context, zsh-only, 100ms–5s blowups under Oh-My-Zsh without `USE_ASYNC=1`, `BUFFER_MAX_SIZE=20`.
- Take: copy widget-wrapping pattern and guards (`PENDING/KEYS_QUEUED`, size cap). Extend to fuzzy + cross-shell.

### fish autosuggestions — gold-standard feel
- Native C++/Rust, history+completions+paths, red=invalid highlight, ~0ms.
- Strengths: just works, high hit rate.
- Weaknesses: prefix-only up/down, suggests secrets/`git push --force`, non-POSIX blocks adoption.
- Take: match its latency and partial-accept keys; fix its privacy gap (suppress secrets/force-push).

### fzf history — universal fallback
- Go binary, `<10ms` at 100k lines, `--scheme=history`.
- Strengths: universal, composable, backend for navi/zoxide-`zi`.
- Weaknesses: dumb (no cwd/exit/duration), stale distro builds, manual keybinds.
- Take: lean on fzf/skim for `ghostline search` UI instead of building a TUI.

### zoxide / autojump — dir jumping only
- zoxide (Rust, ~5–20ms) frecency `z foo`; autojump (Python, ~50–100ms).
- Strengths: replaces `cd` pain, `zoxide import` from others.
- Weaknesses: dirs only, not commands; ambiguous matches need fzf.
- Take: don't compete; interoperate (boost `z`-style dirs in ranking, detect project roots).

### thefuck — UX to steal, perf to avoid
- Rule-based `fuck` corrector (~150 rules; ~15 cover 80%: sudo, git, cd-case, mkdir-p).
- Strengths: magical demo, composes fix + re-execution.
- Weaknesses: dead upstream (no release since Jan 2022), 500ms–2s Python tax, rule conflicts.
- Take: build `thefuck`-lite in Rust: top-15 rules, <50ms, no auto-exec on danger.

### tldr / tealdeer / tlrc + navi + cheat.sh + howdoi
- tldr: example-first pages, cached markdown (~few MB), `<50ms` offline. 3 clients confuse; coverage gaps.
- navi: interactive cheatsheets with `<arg>` interpolation via fzf, ingests tldr/cheat.sh. Needs fzf, authoring cost.
- cheat.sh: `curl cht.sh/tar`, broadest but 300ms–2s network, offline-dead, privacy leak.
- howdoi: SO scraper, brittle, stale.
- Take: bundle `tldr.en.zip` (~2.3MB) offline; use pages as ghost-template source + NL slot-filling. `navi`-style `<arg>` interpolation is the interaction to copy for tldr-inline.

### carapace / cobra + starship / p10k
- carapace: rich completions, 500+ cmds, 10+ shells, cached, ms-level. Large install, spec maintenance.
- starship (~40ms clean, 100ms–2s in huge repos) vs p10k (<10ms cached, now maintenance mode).
- Take: reuse fish/zsh completion specs as flag vocab for typo repair; never add prompt fork tax; document precmd ordering vs starship/p10k.

## Feature-gap table

| Capability | atuin | mcfly | zsh/fish suggest | fzf | zoxide | thefuck | tldr/navi | Gap for GHOSTLINE |
|---|---|---|---|---|---|---|---|---|
| Inline as-you-type | no | no | yes | no | no | no | no | **cross-shell inline, <5ms, no daemon** |
| Fuzzy mid-string | yes | partial | no | yes | yes | n/a | partial | combine fuzzy+inline in one |
| cwd/session/exit-aware rank | yes | yes | no | no | dirs only | exit only | no | transparent + explainable |
| Secret redaction / local-only | no | no | no | n/a | n/a | n/a | n/a | **deny-list + redaction = privacy edge** |
| Zero-deps fast startup | mid | yes | yes | yes | yes | no | yes/no | single static binary, no prompt fork |
| History-miss → cheat fallback | no | no | no | no | no | no | no | **history miss → local tldr snippet inline** |
| Fast fix-last-cmd | no | no | no | no | no | yes/slow | no | instant rule subset, no Python |
| Multiline + duration + stats | yes | no | no | no | no | no | no | keep atuin wins w/o sync server |

## Positioning

Own: **one binary = inline + fuzzy + dir/exit-aware without sync-server weight; privacy-first; identical UX on bash+zsh+fish; history-miss → local tldr; fast thefuck-lite.**
