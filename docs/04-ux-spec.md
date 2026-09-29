# 04 — UX Spec (Ghost Text, Keys, Flows)

## Ghost-text mechanics by shell

### zsh (ZLE — canonical)
- Vars: `BUFFER`, `CURSOR`, `POSTDISPLAY` (rendered after cursor, not in buffer), `region_highlight`.
- Suggest iff `BUFFER` is prefix: `POSTDISPLAY="${suggestion#$BUFFER}"`.
- Style: `region_highlight+=("$#BUFFER $(( $#BUFFER + $#POSTDISPLAY )) fg=244,dim")`. Never embed ANSI in `POSTDISPLAY` (prints literally on zsh 5.9 macOS).
- Widget wrap: save `orig_buffer/orig_postdisplay`, `unset POSTDISPLAY`, call original, skip fetch if `PENDING>0 || KEYS_QUEUED_COUNT>0` (zsh ≥5.4).
- Fast-path: if typing forward into suggestion (`BUFFER == orig_buffer*` and remainder matches), shrink `POSTDISPLAY` without re-query.
- Bash implements the same fast path fork-free (shrink cached suffix locally; re-rank only on diverge, empty cache, or token boundary). Rationale: measured ~6x terminal byte traffic repainting per key, and the fork latency shows a stale-ghost frame that reads as flicker.
- Strategies: `history` (reverse scan) sync <2ms; `completion` async via `zpty` only. History ghost must have zero debounce.
- Guards: `BUFFER_MAX_SIZE=20–50` skips paste/large buffers; `MANUAL_REBIND` for perf; single-owner `POSTDISPLAY` (clashes with Deja-style plugins).

### bash (readline has no POSTDISPLAY)
- Light hack (v1): `bind` macro per printable char → `self-insert + Ctrl-_` → handler reads `$READLINE_LINE/$READLINE_POINT`, reverse-matches, then `save-cursor (\e[s) + print dim suffix + restore-cursor (\e[u)`. Accept keys mutate `READLINE_LINE/POINT` directly.
- Full editor (deferred): `ble.sh` replaces readline, `auto_complete` face. Correct but heavy; 64k history = measurable lag. Don't take this dependency in v1.
- Never put ANSI in line buffer; paint only on redisplay. Clear on `precmd` + `Ctrl-L` repaint to avoid resize/interrupt ghosts.

### fish (reference)
- Native sync per-keystroke recompute (C++/Rust, no fork). Sets the feel bar.
- GHOSTLINE on fish uses `fish_preexec/postexec` for logging (cheap: native `$CMD_DURATION` + `$status`) and a prompt-adjacent ghost hook. Avoid wrapping `fish_prompt` (double-render cost). Respect `fish_autosuggestion_enabled`, `fish_bind_mode`.

### Copilot/IDE conventions (for NL-rewrite case)
- History ghost = suffix-append. NL→cmd rewrite = full replacement: show `⇥ full-cmd` indicator, `Tab` *replaces* buffer (snapshot `_BUFFER_AT_SUGGESTION`, spinner via `POSTDISPLAY`, cancellable poll).

## Styling (faint font)

- Use both dim + gray: `\e[2;38;5;244m … \e[m` (reset `22` clears bold+dim — no independent reset).
- Compatibility (2026 terminfo): 11/12 pass (92%); only `vt100.js` fails. Windows Terminal supports SGR2 since 2020; pre-2020 ignores dim.
- Pitfalls: `fg=8` invisible if Bright-Black == Background (common iTerm2/Solarized bug) → prefer `fg=244`/`fg=60` + `dim`. Light themes: SGR2 weak → explicit gray over pure dim. WezTerm renders dim as thin weight unless `font_rules intensity=Half` + HSV dim. 8-color terms fall back to `0–7`.
- tldr placeholders `{{file}}` get underline/bold accent distinct from dim base.

## Keybindings

| Action | Bind (all shells) | Notes |
|---|---|---|
| Accept full | `→` / `End` / `Ctrl-F` at EOL only | Mid-line `→` must move cursor, not accept |
| Accept word | `Alt-→` / `Alt-F` | Token variant `Ctrl-→` |
| Cycle candidates | `Ctrl-N` / `Ctrl-P` or `↑/↓` in fzf widget | Show `[N/M]` counter |
| Accept + execute | Explicit separate bind (e.g. `Ctrl-Enter` / documented `autosuggest-execute`) | Never on plain accept |
| Dismiss | `Esc`, keep typing (diverge clears instantly) | bash also `Ctrl-]` |
| NL prompt | `Alt-e` / `Ctrl-G` widget | Opens `ghostline "<nl>"` picker, inserts (not runs) |
| Delete history entry | `F2` / `Ctrl-X` in search widget | Like mcfly |

- Only accept when `CURSOR == $#BUFFER` (EOL); vim `vicmd` cursor max is `$#BUFFER-1` (off-by-one) — branch on `KEYMAP`.
- Diverge-to-dismiss: if new `BUFFER` not prefix of suggestion, clear instantly. Drop stale async results via `{term,suggestion}` pairing.
- Never steal `↑` (history) or `Ctrl-R` default; GHOSTLINE search is opt-in bind.

## Flows

### A. History ghost (happy path)
1. User types `git ch` in `~/proj`.
2. Ghost (dim): `eckout main` (dir-aware top hit, exit-0 only).
3. `→` accepts → `git checkout main` in buffer, not executed.

### B. History miss → tldr template
1. User types `tar` (never used here).
2. Ghost (dim): ` -xzf {{file.tar.gz}}` + hint `[tldr tar · Tab: next example]`.
3. `Tab` cycles `tar` examples; typing fills `{{…}}`.

### C. Failure → repair
1. `cd downloads` → `cd: no such file or directory`.
2. Next line (not ghost): `did you mean: cd Downloads? [Enter run | e edit | d dismiss]`.
3. `Enter` runs corrected; `e` puts it in buffer; `d` suppresses 24h after 2×.

### D. NL
1. `Alt-e` → `ghostline: free port 8080` → picker shows `lsof -ti:8080 | xargs kill` + `[E]xplain [Enter]insert [A]bort`.
2. `Enter` inserts into buffer. DANGEROUS tier needs typed `yes`.

## Edge cases

- Multiline: ghost only remainder of current logical line; disable if `BUFFER` contains `\n`.
- Bracketed paste: suppress ghost.
- SSH/slow/high-RTT: force async + larger debounce; no truecolor → gray-only fallback.
- Plugin conflicts: `zsh-syntax-highlighting` clobbers `region_highlight` (need memo feature zsh ≥5.9); `bash-preexec` + ble needs explicit integration; `HIST_IGNORE_ALL_DUPS` breaks history-order strategy — document.
- Resize/interrupt artifacts: always `unset POSTDISPLAY` before original widget; repaint on `Ctrl-L`.
