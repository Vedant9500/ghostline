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

# 1. ghost match paints dim suffix, records full candidate
READLINE_LINE="git ch"; READLINE_POINT=6
_ghostline_ghost >"$tmpout"
out=$(cat "$tmpout")
assert_eq "ghost-full" "git checkout main" "$_GHOSTLINE_SUGGEST"
assert_contains "ghost-paint" "eckout main" "$out"
assert_contains "ghost-dim" $'\e[2;' "$out"

# 1b. same line again -> no repaint (flicker guard), state intact
_ghostline_ghost >"$tmpout"
out=$(cat "$tmpout")
assert_eq "ghost-norepaint" "" "$out"
assert_eq "ghost-state-kept" "git checkout main" "$_GHOSTLINE_SUGGEST"

# 1d. fast path: typing along the cache forks nothing
READLINE_LINE="git ch"; READLINE_POINT=6
_GHOSTLINE_SUGGEST=""; _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""; _GHOSTLINE_LAST_FORK=0
: > "$calls_log"
_ghostline_ghost >/dev/null
assert_eq "fast-prime-calls" "1" "$(ncalls)"
assert_eq "fast-prime-state" "git checkout main" "$_GHOSTLINE_SUGGEST"
READLINE_LINE="git ch"; READLINE_POINT=6
_ghostline_k_101 >/dev/null
assert_eq "fast-typed-e" "git che" "$READLINE_LINE"
assert_eq "fast-norefork" "1" "$(ncalls)"
assert_eq "fast-shrunk" "git checkout main" "$_GHOSTLINE_SUGGEST"
_ghostline_k_99 >/dev/null
assert_eq "fast-typed-c" "git chec" "$READLINE_LINE"
assert_eq "fast-norefork2" "1" "$(ncalls)"
# diverge -> fork again
READLINE_LINE="git X"; READLINE_POINT=5
_GHOSTLINE_LAST_FORK=0
_ghostline_ghost >/dev/null
assert_eq "diverge-reforks" "2" "$(ncalls)"
assert_eq "diverge-cleared" "" "$_GHOSTLINE_SUGGEST"
# token boundary (trailing space) -> re-rank fork even along cache
_GHOSTLINE_SUGGEST="cargo test"; _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""; _GHOSTLINE_LAST_FORK=0
READLINE_LINE="cargo "; READLINE_POINT=6
_ghostline_ghost >/dev/null
assert_eq "space-reforks" "3" "$(ncalls)"

# 1e. slow-path forks throttled to ~10Hz during bursts (ble.sh parity)
: > "$calls_log"
now_ms=$(( ${EPOCHREALTIME/./} / 1000 ))
_GHOSTLINE_LAST_FORK=$now_ms
_GHOSTLINE_SUGGEST=""; _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""
READLINE_LINE="zzz-diverge"; READLINE_POINT=11
_ghostline_ghost >/dev/null
assert_eq "throttle-skips" "0" "$(ncalls)"
_GHOSTLINE_LAST_FORK=$(( now_ms - 500 ))
_ghostline_ghost >/dev/null
assert_eq "throttle-fires-after-pause" "1" "$(ncalls)"

# 2. no history hit -> cleared + erase-to-EOL
READLINE_LINE="zzz"; READLINE_POINT=3
_ghostline_ghost >"$tmpout"
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
_ghostline_accept >/dev/null
assert_eq "accept-line" "git checkout main" "$READLINE_LINE"
assert_eq "accept-point" "17" "$READLINE_POINT"

# 5. accept mid-line acts as forward-char (no clobber)
READLINE_LINE="abc"; READLINE_POINT=1
_GHOSTLINE_SUGGEST=""
_ghostline_accept >/dev/null
assert_eq "accept-mid-point" "2" "$READLINE_POINT"
assert_eq "accept-mid-line" "abc" "$READLINE_LINE"

# 6. accept word twice completes the suggestion
READLINE_LINE="git ch"; READLINE_POINT=6
_GHOSTLINE_SUGGEST="git checkout main"
_ghostline_accept_word >/dev/null
assert_eq "word1" "git checkout" "$READLINE_LINE"
_ghostline_accept_word >/dev/null
assert_eq "word2" "git checkout main" "$READLINE_LINE"

# 7. dismiss clears
_GHOSTLINE_SUGGEST="git checkout main"
_ghostline_dismiss >/dev/null
assert_eq "dismiss" "" "$_GHOSTLINE_SUGGEST"

# 8a. per-char insert fn types the char AND refreshes ghost
READLINE_LINE="git c"; READLINE_POINT=5
_GHOSTLINE_LAST_FORK=0
_ghostline_k_104 >/dev/null
assert_eq "keyfn-line" "git ch" "$READLINE_LINE"
assert_eq "keyfn-refresh" "git checkout main" "$_GHOSTLINE_SUGGEST"

# 8b. backspace deletes + refreshes ghost for the shorter prefix
READLINE_LINE="git c"; READLINE_POINT=5
_ghostline_k_104 >/dev/null
assert_eq "keyfn-line" "git ch" "$READLINE_LINE"
assert_eq "keyfn-refresh" "git checkout main" "$_GHOSTLINE_SUGGEST"
READLINE_LINE="git cha"; READLINE_POINT=7
_ghostline_bspace >/dev/null
assert_eq "bspace-line" "git ch" "$READLINE_LINE"
assert_eq "bspace-refresh" "git checkout main" "$_GHOSTLINE_SUGGEST"

# 9. kill-word eats one word
READLINE_LINE="git checkout main"; READLINE_POINT=17
_ghostline_kill_word >/dev/null
assert_eq "killword" "git checkout " "$READLINE_LINE"

# 10. kill-line to start
READLINE_LINE="git ch"; READLINE_POINT=6
_ghostline_kill_line >/dev/null
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
_ghostline_ghost
assert_eq "toolong" "" "$_GHOSTLINE_SUGGEST"

# 13. (macro installation is verified separately under bash -i; bind -p is
# empty in non-interactive shells)

if [ "$fail" -eq 0 ]; then echo "bash-ghost: all ok"; fi
exit "$fail"
