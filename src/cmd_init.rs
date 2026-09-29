/// Shell snippets printed by `ghostline init <bash|zsh|fish>`.
/// Idempotent, append-safe, honor GHOSTLINE_DISABLED. Binary does ignore/redact.

pub fn snippet(shell: &str) -> Result<&'static str, String> {
    match shell {
        "bash" => Ok(BASH),
        "zsh" => Ok(ZSH),
        "fish" => Ok(FISH),
        other => Err(format!(
            "unknown shell '{other}' (want bash|zsh|fish). Usage: eval \"$(ghostline init bash)\""
        )),
    }
}

const HEADER: &str = "# ghostline: history logger (idempotent; safe to eval twice)\n";

const BASH: &str = r#"# ghostline: history logger (idempotent; safe to eval twice)
if [ -z "${_GHOSTLINE_INITED:-}" ]; then
  _GHOSTLINE_INITED=1
  export GHOSTLINE_SESSION="${GHOSTLINE_SESSION:-$$@$(hostname 2>/dev/null)}"
  _ghostline_post() {
    local ec=$?
    _GHOSTLINE_SUGGEST=""
    _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""
    if [ -n "${GHOSTLINE_DISABLED:-}" ]; then return $ec; fi
    local cmd
    cmd=$(HISTTIMEFORMAT= history 1 2>/dev/null | sed -E 's/^\s*[0-9]+\s+//')
    case "$cmd" in ghostline*|_ghostline_post*|"") return $ec;; esac
    (command ghostline log --shell bash --exit "$ec" --cwd "$PWD" --session "$GHOSTLINE_SESSION" "$cmd" >/dev/null 2>&1 &)
    # NOTE: subshell-wrap (not bare `&` / `& disown`): the parent shell never
    # tracks the job, so NEITHER "[1] pid" NOR "[1]+ Done" sprays the next
    # prompt mid-typing (that spray also flickers readline). Verified: `& disown`
    # still prints the "[1] pid" startup line; `( ... & )` prints nothing.
    # NOTE: history-1 polling is lossy under rapid prompts; install bash-preexec for exact preexec capture.
    return $ec
  }
  case "${PROMPT_COMMAND:-}" in *_ghostline_post*) ;; *)
    if [ -z "${PROMPT_COMMAND:-}" ]; then PROMPT_COMMAND="_ghostline_post"
    else PROMPT_COMMAND="_ghostline_post; ${PROMPT_COMMAND}"; fi ;;
  esac
fi

# --- ghostline ghost: inline faint suggestion (bash>=4). GHOSTLINE_GHOST=0 disables. ---
if [ -z "${_GHOSTLINE_GHOST_INITED:-}" ] && [ "${GHOSTLINE_GHOST:-1}" != "0" ] && [ "${BASH_VERSINFO[0]:-0}" -ge 4 ]; then
  _GHOSTLINE_GHOST_INITED=1
  _GHOSTLINE_SUGGEST=""
  _GHOSTLINE_PAINTED=""
  _GHOSTLINE_PAINTED_LINE=""

  # Recompute ghost for current line; paints dim suffix WITHOUT touching READLINE_LINE.
  # Typing stays native (no per-char bind -x): ANY bind -x forces readline to
  # do a full-line redisplay (`\r\e[K\rP> ...`) per keystroke vs 1 byte for
  # native self-insert (measured 2x bytes empty, ~6x with ghost). Per-char
  # auto-ghost therefore flickers by construction in bash (no POSTDISPLAY).
  # On-demand preview (Ctrl-G / Right at EOL) + native typing preserves a
  # matching ghost tail with zero repaint (typed char overwrites same ghost
  # char); diverge/backspace refresh via the edit bindings below.
  # Paint is deferred past readline's redisplay via double-subshell
  # ((sleep; printf)&) so it lands AFTER `P> line` at the cursor (correct
  # column) with no `[1]` job spam. Sync paint before redisplay gets
  # overwritten by `P> line` (misaligned by prompt width, then invisible).
  _ghostline_ghost() {
    [ "${GHOSTLINE_GHOST:-1}" = "0" ] && return 0
    [ -n "${GHOSTLINE_DISABLED:-}" ] && return 0
    case "${TERM:-}" in dumb|"") return 0;; esac
    [ -n "${COMP_LINE:-}" ] && return 0
    local line="$READLINE_LINE" point="$READLINE_POINT"
    [ "$point" -ne "${#line}" ] && { _GHOSTLINE_SUGGEST=""; return 0; }
    [ -z "$line" ] && { _GHOSTLINE_SUGGEST=""; return 0; }
    [ "${#line}" -gt 200 ] && { _GHOSTLINE_SUGGEST=""; return 0; }
    local suf
    if [ -n "$_GHOSTLINE_SUGGEST" ] && [[ "$_GHOSTLINE_SUGGEST" == "$line"* ]] \
       && [ "${line: -1}" != " " ]; then
      suf="${_GHOSTLINE_SUGGEST#$line}"
    else
      # Slow path: throttle forks to ~10Hz (held Backspace auto-repeat).
      # No erase-before-fork: readline's own `\r\e[K` after -x already clears
      # stale ghost; an extra `\e[K` at col 0 before redisplay blanks the
      # whole line (flash). Stale stays ~1ms during the fork, then the
      # deferred paint overwrites it.
      if [ -n "${EPOCHREALTIME:-}" ]; then
        local now_ms=$(( ${EPOCHREALTIME/./} / 1000 ))
        if [ $(( now_ms - ${_GHOSTLINE_LAST_FORK:-0} )) -lt 100 ]; then
          return 0
        fi
        _GHOSTLINE_LAST_FORK=$now_ms
      fi
      suf=$(command ghostline suggest --cwd "$PWD" -- "$line" 2>/dev/null)
      _GHOSTLINE_SUGGEST="$line$suf"
    fi
    if [ -z "$suf" ]; then
      _GHOSTLINE_SUGGEST=""
      return 0
    fi
    if [ "$suf" = "${_GHOSTLINE_PAINTED:-}" ] && [ "$line" = "${_GHOSTLINE_PAINTED_LINE:-}" ]; then
      return 0  # already on screen; repainting would flicker
    fi
    _GHOSTLINE_PAINTED="$suf"
    _GHOSTLINE_PAINTED_LINE="$line"
    # Deferred past redisplay (see header): correct column, no job spam.
    ((sleep 0.03; printf '\e[s\e[K\e[2;38;5;244m%s\e[m\e[u' "$suf") &)
  }

  _ghostline_preview() {  # Ctrl-G: explicit fetch (typing itself is native, no hook)
    _ghostline_ghost
  }

  _ghostline_has_usable() {
    [ -n "$_GHOSTLINE_SUGGEST" ] && [[ "$_GHOSTLINE_SUGGEST" == "$READLINE_LINE"* ]] \
      && [ "$READLINE_POINT" -eq "${#READLINE_LINE}" ]
  }

  _ghostline_accept() {  # Right / Ctrl-F: accept at EOL, fetch-preview when empty, else forward-char
    if _ghostline_has_usable; then
      READLINE_LINE="$_GHOSTLINE_SUGGEST"
      READLINE_POINT=${#READLINE_LINE}
      _ghostline_ghost
    elif [ "$READLINE_POINT" -eq "${#READLINE_LINE}" ] && [ -n "$READLINE_LINE" ]; then
      _ghostline_ghost  # at EOL with no ghost: preview instead of useless step
    else
      READLINE_POINT=$((READLINE_POINT + 1))
      [ "$READLINE_POINT" -gt "${#READLINE_LINE}" ] && READLINE_POINT=${#READLINE_LINE}
    fi
  }

  _ghostline_end() {  # End / Ctrl-E: accept full at EOL, else move to EOL + refresh
    if _ghostline_has_usable; then
      READLINE_LINE="$_GHOSTLINE_SUGGEST"
      READLINE_POINT=${#READLINE_LINE}
    else
      READLINE_POINT=${#READLINE_LINE}
    fi
    _ghostline_ghost
  }

  _ghostline_accept_word() {  # Alt-Right: accept one word of the ghost
    if _ghostline_has_usable; then
      local rem="${_GHOSTLINE_SUGGEST#$READLINE_LINE}"
      if [[ $rem =~ ^([[:space:]]*[^[:space:]]+) ]]; then
        READLINE_LINE+="${BASH_REMATCH[1]}"
        READLINE_POINT=${#READLINE_LINE}
        _ghostline_ghost
      fi
    else
      local rest="${READLINE_LINE:READLINE_POINT}"  # mid-line: emulate forward-word
      if [[ $rest =~ ^[^[:space:]]+[[:space:]]* ]]; then
        READLINE_POINT=$((READLINE_POINT + ${#BASH_REMATCH[0]}))
      else
        READLINE_POINT=${#READLINE_LINE}
      fi
    fi
  }

  _ghostline_dismiss() {  # Ctrl-]: drop suggestion (redisplay already clears paint)
    _GHOSTLINE_SUGGEST=""
    _GHOSTLINE_PAINTED=""; _GHOSTLINE_PAINTED_LINE=""
  }

  _ghostline_bspace() {  # Backspace/C-h: delete + refresh (else ghost goes stale)
    if [ "$READLINE_POINT" -gt 0 ]; then
      READLINE_LINE="${READLINE_LINE:0:READLINE_POINT-1}${READLINE_LINE:READLINE_POINT}"
      READLINE_POINT=$((READLINE_POINT - 1))
    fi
    _ghostline_ghost
  }

  _ghostline_kill_word() {  # Ctrl-W: emulate unix-word-rubout + refresh
    local b="${READLINE_LINE:0:READLINE_POINT}" after="${READLINE_LINE:READLINE_POINT}"
    if [[ $b =~ ^(.*[^[:space:]])[[:space:]]+$ ]]; then
      b="${BASH_REMATCH[1]}"
    elif [[ $b =~ ^(.*[[:space:]])[^[:space:]]+$ ]]; then
      b="${BASH_REMATCH[1]}"  # keep delimiter space, like unix-word-rubout
    else
      b=""
    fi
    READLINE_LINE="$b$after"
    READLINE_POINT=${#b}
    _ghostline_ghost
  }

  _ghostline_kill_line() {  # Ctrl-U: kill to start + refresh
    READLINE_LINE="${READLINE_LINE:READLINE_POINT}"
    READLINE_POINT=0
    _ghostline_ghost
  }
  _ghostline_kill_end() {  # Ctrl-K: kill to end + refresh
    READLINE_LINE="${READLINE_LINE:0:READLINE_POINT}"
    _ghostline_ghost
  }
  _ghostline_home() { READLINE_POINT=0; }  # Ctrl-A

  # NOTE: printable chars are intentionally NOT bound. ANY `bind -x` forces a
  # full-line redisplay (`\r\e[K\rP> ...`) per keystroke vs 1 byte for native
  # self-insert — that whole-line refresh per character IS the flicker.
  # A macro `"a": "a..."` is no escape: readline rescans it and recurses
  # ("maximum macro execution nesting level exceeded"). Typing stays native;
  # ghost is on-demand (Ctrl-G / Right at EOL) via _ghostline_ghost above.
  _ghostline_install_map() {
    local map="$1"
    bind -m "$map" -x '"\e[C": _ghostline_accept'
    bind -m "$map" -x '"\e[F": _ghostline_end'
    bind -m "$map" -x '"\eOF": _ghostline_end'
    bind -m "$map" -x '"\C-f": _ghostline_accept'
    bind -m "$map" -x '"\C-g": _ghostline_preview'
    bind -m "$map" -x '"\e\e[C": _ghostline_accept_word'
    bind -m "$map" -x '"\C-]": _ghostline_dismiss'
    bind -m "$map" -x '"\C-?": _ghostline_bspace'
    bind -m "$map" -x '"\C-h": _ghostline_bspace'
    bind -m "$map" -x '"\C-w": _ghostline_kill_word'
    bind -m "$map" -x '"\C-u": _ghostline_kill_line'
    bind -m "$map" -x '"\C-k": _ghostline_kill_end'
    bind -m "$map" -x '"\C-a": _ghostline_home'
    bind -m "$map" -x '"\C-e": _ghostline_end'
  }
  if set -o 2>/dev/null | grep -q '^vi[[:space:]]*on'; then
    _ghostline_install_map emacs
    _ghostline_install_map vi-insert
  else
    _ghostline_install_map emacs
  fi
  unset -f _ghostline_install_map
fi
"#;

const ZSH: &str = r#"# ghostline: history logger (idempotent; safe to eval twice)
if [ -z "${_GHOSTLINE_INITED:-}" ]; then
  _GHOSTLINE_INITED=1
  export GHOSTLINE_SESSION="${GHOSTLINE_SESSION:-$$@$(hostname 2>/dev/null)}"
  autoload -Uz add-zsh-hook 2>/dev/null
  if (( $+functions[add-zsh-hook] )); then
    _ghostline_preexec() { typeset -g _GHOSTLINE_CMD="$1"; typeset -g _GHOSTLINE_START="$EPOCHREALTIME"; }
    _ghostline_precmd() {
      local ec=$?
      if [ -n "${GHOSTLINE_DISABLED:-}" ]; then return $ec; fi
      local cmd="${_GHOSTLINE_CMD:-}"; unset _GHOSTLINE_CMD
      case "$cmd" in ghostline*|"") return $ec;; esac
      command ghostline log --shell zsh --exit "$ec" --cwd "$PWD" --session "$GHOSTLINE_SESSION" "$cmd" >/dev/null 2>&1 &!
      # NOTE: `&!` backgrounds AND disowns: no "[1]+ done" notice spraying the next prompt.
      return $ec
    }
    add-zsh-hook preexec _ghostline_preexec
    add-zsh-hook precmd _ghostline_precmd
  fi
fi
"#;

const FISH: &str = r#"# ghostline: history logger (idempotent; safe to eval twice)
if test -z "$_GHOSTLINE_INITED"
  set -g _GHOSTLINE_INITED 1
  if test -z "$GHOSTLINE_SESSION"
    set -gx GHOSTLINE_SESSION "$fish_pid"(hostname 2>/dev/null; or echo unknown)
  end
  function _ghostline_post --on-event fish_postexec -a cmd
    if test -n "$GHOSTLINE_DISABLED"; return $status; end
    switch "$cmd"
      case 'ghostline*' ''; return $status
    end
    command ghostline log --shell fish --exit $status --cwd $PWD --session $GHOSTLINE_SESSION "$cmd" >/dev/null 2>&1 &; disown
    # NOTE: `; disown` silences fish's background-job notice on the next prompt.
  end
end
"#;

#[allow(dead_code)]
pub fn header() -> &'static str {
    HEADER
}

#[cfg(test)]
mod tests {
    use super::snippet;

    /// Snippet must be syntax-clean AND pass the functional ghost scenarios
    /// (stub `ghostline suggest`, scripted READLINE_LINE/POINT).
    #[test]
    fn bash_snippet_functional() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("gl-bash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let snip = dir.join("snippet.bash");
        std::fs::write(&snip, snippet("bash").unwrap()).unwrap();

        let stub = dir.join("ghostline");
        std::fs::write(
            &stub,
            "#!/usr/bin/env bash\necho \"${@: -1}\" >> \"${GL_CALL_LOG:-/dev/null}\"\nbuf=\"${@: -1}\"\ncase \"$buf\" in\n  \"git ch\") printf 'eckout main' ;;\n  \"git checkout\") printf ' main' ;;\n  \"cargo \") printf 'test' ;;\n  *) printf '' ;;\nesac\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // 1. syntax check
        let syntax = std::process::Command::new("bash")
            .args(["-n", &snip.to_string_lossy()])
            .output()
            .expect("bash -n");
        assert!(
            syntax.status.success(),
            "bash -n failed: {}",
            String::from_utf8_lossy(&syntax.stderr)
        );

        // 2. functional scenarios
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let script = format!("{manifest}/tests/bash_ghost.sh");
        let snip_s = snip.to_string_lossy().into_owned();
        let stub_s = stub.to_string_lossy().into_owned();
        let out = std::process::Command::new("bash")
            .arg(&script)
            .arg(&snip_s)
            .arg(&stub_s)
            .env("GL_CALL_LOG", dir.join("calls.log"))
            .output()
            .expect("run bash_ghost.sh");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "bash_ghost.sh failed:\nstdout: {stdout}\nstderr: {stderr}"
        );
        assert!(stdout.contains("all ok"), "unexpected output: {stdout}");

        // 3. NO per-char -x bindings (needs line editing -> bash -i).
        // ANY `bind -x` forces a full-line redisplay (`\r\e[K\rP> ...`) per
        // keystroke vs 1 byte native self-insert — that whole-line refresh
        // per character IS the flicker. Printable chars must stay native.
        // -x bindings appear under `bind -X`.
        // Regression guards: no `_ghostline_k_*`, no `_ghostline_insert`,
        // but preview (`_ghostline_preview` on Ctrl-G) must be bound.
        use std::process::Stdio;
        // --norc + unset guard: the ambient ~/.bashrc may eval a stale
        // ghostline whose _GHOSTLINE_GHOST_INITED would skip our install.
        let out = std::process::Command::new("bash")
            .args(["--norc", "-i", "-c", &format!("unset _GHOSTLINE_GHOST_INITED; source {snip_s}; bind -m emacs -X")])
            .stdin(Stdio::null())
            .output()
            .expect("bash -i bind -X");
        let listing = String::from_utf8_lossy(&out.stdout);
        let nkeys = listing.lines().filter(|l| l.contains("_ghostline_k_")).count();
        assert!(
            nkeys == 0,
            "per-char bindings cause whole-line refresh flicker, must be 0 (got {nkeys}):\n{listing}"
        );
        assert!(
            !listing.contains("_ghostline_insert"),
            "insert fn forces full redisplay per char:\n{listing}"
        );
        assert!(
            listing.contains("_ghostline_preview"),
            "on-demand preview (Ctrl-G) missing:\n{listing}"
        );
        assert!(
            listing.contains("_ghostline_accept"),
            "accept binding missing:\n{listing}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn init_rejects_unknown_shell() {
        assert!(snippet("powershell").is_err());
        // Background logging must not leak job notices into the prompt:
        // bash uses subshell-wrap `( ... & )` (bare `&` prints "[1]+ Done";
        // `& disown` still prints the "[1] pid" startup line — both verified).
        assert!(snippet("bash").unwrap().contains("& )"), "bash logger must be silent");
        assert!(snippet("zsh").unwrap().contains("&!"), "zsh must disown logger");
        assert!(snippet("fish").unwrap().contains("disown"), "fish must disown logger");
        // unused import guard
        let _ = super::header().len() + snippet("zsh").unwrap().len();
    }
}
