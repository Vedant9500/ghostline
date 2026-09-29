#!/usr/bin/env bash
# Functional test for the ghostline bash ghost snippet.
# Args: <snippet-path> <stub-ghostline-path>
# The stub implements: ghostline suggest --cwd DIR -- BUFFER -> suffix
set -u
snippet="$1"
stub="$2"
stubdir="$(dirname "$stub")"
export PATH="$stubdir:$PATH"
export TERM=xterm GHOSTLINE_GHOST=1
calls_log="$stubdir/calls.log"
: > "$calls_log"
ncalls() { wc -l < "$calls_log" | tr -d ' '; }
tmpout="$(mktemp)"
trap 'rm -f "$tmpout"' EXIT
# shellcheck disable=SC1090
source "$snippet"

fail=0
assert_eq() { # <name> <want> <got>
  if [ "$2" != "$3" ]; then echo "FAIL $1: want [$2] got [$3]"; fail=1; fi
}
assert_contains() { # <name> <needle> <haystack>
  case "$3" in *"$2"*) ;; *) echo "FAIL $1: missing [$2] in [$3]"; fail=1;; esac
}
# Paint is deferred past readline redisplay ((sleep 0.03; printf)&), so in
# non-interactive tests wait for the background job before reading $tmpout.
ghost_wait() { sleep 0.15; wait 2>/dev/null; true; }

# 1. ghost match paints dim suffix, records full candidate
READLINE_LINE="git ch"; READLINE_POINT=6
_GHOSTLINE_SUGGEST=""; _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""; _GHOSTLINE_LAST_FORK=0
: > "$tmpout"
_ghostline_ghost >"$tmpout" 2>&1
ghost_wait
out=$(cat "$tmpout")
assert_eq "ghost-full" "git checkout main" "$_GHOSTLINE_SUGGEST"
assert_contains "ghost-paint" "eckout main" "$out"
assert_contains "ghost-dim" $'\e[2;' "$out"

# 1b. same line again -> no repaint (flicker guard), state intact
: > "$tmpout"
_ghostline_ghost >"$tmpout" 2>&1
ghost_wait
out=$(cat "$tmpout")
assert_eq "ghost-norepaint" "" "$out"
assert_eq "ghost-state-kept" "git checkout main" "$_GHOSTLINE_SUGGEST"

# 1c. no per-char insert hooks: typing stays native (no whole-line refresh).
# ANY `bind -x` forces `\r\e[K\rP> ...` per keystroke; printable chars must
# use native self-insert. The old `_ghostline_k_*` / `_ghostline_insert`
# functions must not exist.
if declare -F _ghostline_insert >/dev/null 2>&1; then echo "FAIL no-insert-fn: _ghostline_insert still defined"; fail=1; fi
if declare -F _ghostline_k_101 >/dev/null 2>&1; then echo "FAIL no-perchar: _ghostline_k_101 still defined"; fail=1; fi
if declare -F _ghostline_k_92 >/dev/null 2>&1; then echo "FAIL no-perchar-bs: _ghostline_k_92 still defined"; fail=1; fi
# Native typing simulation: shell sets READLINE_LINE directly, no fork yet.
READLINE_LINE="git c"; READLINE_POINT=5
_GHOSTLINE_SUGGEST=""; _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""; _GHOSTLINE_LAST_FORK=0
: > "$calls_log"
READLINE_LINE="git ch"; READLINE_POINT=6  # native self-insert of 'h', no hook
assert_eq "native-nofork" "0" "$(ncalls)"
# Explicit preview fetches once, then shrinks fork-free along cache.
_ghostline_preview >"$tmpout" 2>&1
ghost_wait
assert_eq "preview-calls" "1" "$(ncalls)"
assert_eq "preview-state" "git checkout main" "$_GHOSTLINE_SUGGEST"
# Typing along cache + re-preview needs no re-fork (fast path).
READLINE_LINE="git che"; READLINE_POINT=7
_ghostline_ghost >"$tmpout" 2>&1
ghost_wait
assert_eq "fast-norefork" "1" "$(ncalls)"
assert_eq "fast-shrunk" "git checkout main" "$_GHOSTLINE_SUGGEST"
# diverge -> fork again
READLINE_LINE="git X"; READLINE_POINT=5
_GHOSTLINE_LAST_FORK=0
_ghostline_ghost >/dev/null 2>&1
ghost_wait
assert_eq "diverge-reforks" "2" "$(ncalls)"
assert_eq "diverge-cleared" "" "$_GHOSTLINE_SUGGEST"
# token boundary (trailing space) -> re-rank fork even along cache
_GHOSTLINE_SUGGEST="cargo test"; _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""; _GHOSTLINE_LAST_FORK=0
READLINE_LINE="cargo "; READLINE_POINT=6
_ghostline_ghost >/dev/null 2>&1
ghost_wait
assert_eq "space-reforks" "3" "$(ncalls)"

# 1e. slow-path forks throttled to ~10Hz (held-Backspace auto-repeat)
: > "$calls_log"
now_ms=$(( ${EPOCHREALTIME/./} / 1000 ))
_GHOSTLINE_LAST_FORK=$now_ms
_GHOSTLINE_SUGGEST=""; _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""
READLINE_LINE="zzz-diverge"; READLINE_POINT=11
_ghostline_ghost >/dev/null 2>&1
ghost_wait
assert_eq "throttle-skips" "0" "$(ncalls)"
_GHOSTLINE_LAST_FORK=$(( now_ms - 500 ))
_ghostline_ghost >/dev/null 2>&1
ghost_wait
assert_eq "throttle-fires-after-pause" "1" "$(ncalls)"

# 2. no history hit -> cleared (redisplay already clears paint; no extra K)
READLINE_LINE="zzz"; READLINE_POINT=3
_GHOSTLINE_LAST_FORK=0
_ghostline_ghost >"$tmpout" 2>&1
ghost_wait
out=$(cat "$tmpout")
assert_eq "ghost-miss" "" "$_GHOSTLINE_SUGGEST"

# 3. mid-line -> no ghost
READLINE_LINE="git ch"; READLINE_POINT=3
_GHOSTLINE_SUGGEST="stale"
_ghostline_ghost
assert_eq "ghost-midline" "" "$_GHOSTLINE_SUGGEST"

# 4. accept full at EOL
READLINE_LINE="git ch"; READLINE_POINT=6
_GHOSTLINE_SUGGEST="git checkout main"
_GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""; _GHOSTLINE_LAST_FORK=0
_ghostline_accept >/dev/null 2>&1
ghost_wait
assert_eq "accept-line" "git checkout main" "$READLINE_LINE"
assert_eq "accept-point" "17" "$READLINE_POINT"

# 4b. Right at EOL with no ghost -> preview fetch (not useless step)
READLINE_LINE="git ch"; READLINE_POINT=6
_GHOSTLINE_SUGGEST=""; _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""; _GHOSTLINE_LAST_FORK=0
: > "$calls_log"
_ghostline_accept >/dev/null 2>&1
ghost_wait
assert_eq "accept-eol-fetches" "1" "$(ncalls)"
assert_eq "accept-eol-state" "git checkout main" "$_GHOSTLINE_SUGGEST"
assert_eq "accept-eol-line-kept" "git ch" "$READLINE_LINE"

# 5. accept mid-line acts as forward-char (no clobber, no fork)
READLINE_LINE="abc"; READLINE_POINT=1
_GHOSTLINE_SUGGEST=""
: > "$calls_log"
_ghostline_accept >/dev/null 2>&1
ghost_wait
assert_eq "accept-mid-point" "2" "$READLINE_POINT"
assert_eq "accept-mid-line" "abc" "$READLINE_LINE"
assert_eq "accept-mid-nofork" "0" "$(ncalls)"

# 6. accept word twice completes the suggestion
READLINE_LINE="git ch"; READLINE_POINT=6
_GHOSTLINE_SUGGEST="git checkout main"
_GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""; _GHOSTLINE_LAST_FORK=0
_ghostline_accept_word >/dev/null 2>&1
ghost_wait
assert_eq "word1" "git checkout" "$READLINE_LINE"
_ghostline_accept_word >/dev/null 2>&1
ghost_wait
assert_eq "word2" "git checkout main" "$READLINE_LINE"

# 7. dismiss clears state (redisplay clears paint; no extra K needed)
_GHOSTLINE_SUGGEST="git checkout main"
_GHOSTLINE_PAINTED="x"; _GHOSTLINE_PAINTED_LINE="y"
_ghostline_dismiss >/dev/null 2>&1
assert_eq "dismiss" "" "$_GHOSTLINE_SUGGEST"
assert_eq "dismiss-painted" "" "$_GHOSTLINE_PAINTED"

# 8b. backspace deletes + refreshes ghost for the shorter prefix
READLINE_LINE="git cha"; READLINE_POINT=7
_GHOSTLINE_SUGGEST=""; _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""; _GHOSTLINE_LAST_FORK=0
: > "$calls_log"
_ghostline_bspace >/dev/null 2>&1
ghost_wait
assert_eq "bspace-line" "git ch" "$READLINE_LINE"
assert_eq "bspace-refresh" "git checkout main" "$_GHOSTLINE_SUGGEST"

# 9. kill-word eats one word
READLINE_LINE="git checkout main"; READLINE_POINT=17
_ghostline_kill_word >/dev/null 2>&1
ghost_wait
assert_eq "killword" "git checkout " "$READLINE_LINE"

# 10. kill-line to start
READLINE_LINE="git ch"; READLINE_POINT=6
_ghostline_kill_line >/dev/null 2>&1
ghost_wait
assert_eq "killline" "" "$READLINE_LINE"
assert_eq "killline-point" "0" "$READLINE_POINT"

# 11. GHOSTLINE_GHOST=0 -> handler no-op
GHOSTLINE_GHOST=0
READLINE_LINE="git ch"; READLINE_POINT=6
_GHOSTLINE_SUGGEST=""
_ghostline_ghost
assert_eq "disabled" "" "$_GHOSTLINE_SUGGEST"
GHOSTLINE_GHOST=1

# 12. overlong buffer -> no-op (no fork)
long=$(printf 'x%.0s' $(seq 1 201))
READLINE_LINE="$long"; READLINE_POINT=201
_GHOSTLINE_SUGGEST=""
: > "$calls_log"
_ghostline_ghost
assert_eq "toolong" "" "$_GHOSTLINE_SUGGEST"
assert_eq "toolong-nofork" "0" "$(ncalls)"

# 13. (bind installation is verified separately under bash -i; bind -p is
# empty in non-interactive shells)

if [ "$fail" -eq 0 ]; then echo "bash-ghost: all ok"; fi
exit "$fail"
