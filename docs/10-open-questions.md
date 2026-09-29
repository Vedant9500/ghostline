# 10 — Open Questions & Variables to Validate

## Decisions needed before code

1. **Binary name + repo — DECIDED: `ghostline`** (alias `gl`). crates.io `ghostline` returned 404 (available) at naming time; `dowse` rejected (taken by a Windows file-search crate with `dowse` binary + dowse/douse confusion). Still to do: reserve crate + GitHub org/repo, check Homebrew/apt, decide `gl` alias coexistence.
2. **MSRV + musl target:** Rust version floor? `x86_64-unknown-linux-musl` + `aarch64-unknown-linux-musl` + macOS universal — confirm `rusqlite/bundled` builds clean on all three in CI.
3. **Arg parser for hot path:** hand-rolled vs `lexopt`/`argh` — measure `clap_derive` cold-start tax before deciding; hot `log` subcommand must stay dependency-free.
4. **tldr license surface:** pages are `CC-BY-SA-4.0` — bundling `tldr.en.zip` requires attribution + share-alike notice. Decide: bundle zip vs download-on-first-run vs git-submodule. Include `ATTRIBUTION.md` regardless.
5. **Ctrl-R stance:** never hijack `↑`; but do we bind `Ctrl-R` to `ghostline search` by default in `init` snippet? Recommendation: opt-in flag (`ghostline init --bind-ctrl-r`), default off.
6. **Ghost accept keys:** `→` vs `Tab` conflict (Tab = completion). Recommendation: `→`/`End`/`Ctrl-F` accept, `Tab` cycles tldr examples or completes — validate with fish/zsh users.
7. **Model default — PARKED:** local models on hold (memory/compute budget). v1/v1.1 ship NL-rules baseline only. Revisit with fresh research per `09-roadmap.md`.
8. **Sync scope:** E2EE server vs git-repo sync vs none. Schema reserves UUIDv7/Lamport — but don't promise server dates.

## Variables to be aware of (validate in test matrix)

`SHELL` vs running shell · `TERM`/dumb · TTY null (cron/pipe) · `SSH_CONNECTION` multiplexing · `TMUX`/`STY`/WezTerm/Kitty panes · `SUDO_USER`/EUID=0 HOME illusion · containers (ephemeral hostname, overlayfs sqlite) · nix/snap (read-only TMPDIR, respect `XDG_*`) · sensitive env bleed · `HISTFILE`/`HISTCONTROL` interplay · `HOME` on NFS (sqlite latency) · clock skew (use monotonic for duration) · `TERM_PROGRAM` quirks (VSCode/xterm.js/Windows Terminal pre-2020 dim).

Full matrix in `06-architecture.md` §Environment + `08-performance-budgets.md` §Benchmarking.

## Research to redo quarterly

- Small-model pareto (size vs NL2SH pass rate vs CPU latency) — moves fast.
- tldr page counts / client churn (tealdeer vs tlrc).
- fish 4.x / bash 5.3 hook changes, Windows Terminal SGR2 coverage.
- gitleaks rule updates for redaction corpus.
