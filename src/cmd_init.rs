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
    if [ -n "${GHOSTLINE_DISABLED:-}" ]; then return $ec; fi
    local cmd
    cmd=$(HISTTIMEFORMAT= history 1 2>/dev/null | sed -E 's/^\s*[0-9]+\s+//')
    case "$cmd" in ghostline*|_ghostline_post*|"") return $ec;; esac
    command ghostline log --shell bash --exit "$ec" --cwd "$PWD" --session "$GHOSTLINE_SESSION" "$cmd" >/dev/null 2>&1 &
    # NOTE: history-1 polling is lossy under rapid prompts; install bash-preexec for exact preexec capture.
    return $ec
  }
  case "${PROMPT_COMMAND:-}" in *_ghostline_post*) ;; *)
    if [ -z "${PROMPT_COMMAND:-}" ]; then PROMPT_COMMAND="_ghostline_post"
    else PROMPT_COMMAND="_ghostline_post; ${PROMPT_COMMAND}"; fi ;;
  esac
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
      command ghostline log --shell zsh --exit "$ec" --cwd "$PWD" --session "$GHOSTLINE_SESSION" "$cmd" >/dev/null 2>&1 &
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
    set -gx GHOSTLINE_SESSION (echo (fish_pid)(hostname 2>/dev/null))
  end
  function _ghostline_post --on-event fish_postexec -a cmd
    if test -n "$GHOSTLINE_DISABLED"; return $status; end
    switch "$cmd"
      case 'ghostline*' ''; return $status
    end
    command ghostline log --shell fish --exit $status --cwd $PWD --session $GHOSTLINE_SESSION "$cmd" >/dev/null 2>&1 &
  end
end
"#;

#[allow(dead_code)]
pub fn header() -> &'static str {
    HEADER
}
