#!/usr/bin/env bash
# ghostline install/update: builds release binary, installs it, and wires the
# shell hook into every installed shell's rc file (covers all terminals —
# terminals just run your shell). Idempotent: safe to re-run for updates.
# Must be EXECUTED (./install.sh), not sourced: the `exit` calls below would
# otherwise exit your interactive shell and the terminal closes.
if [ "${BASH_SOURCE[0]:-}" != "${0}" ]; then
  echo "error: run it, don't source it: ./install.sh (sourcing runs exit in your shell)" >&2
  return 1 2>/dev/null || exit 1
fi
set -euo pipefail

PREFIX="${GHOSTLINE_PREFIX:-$HOME/.local/bin}"
UNINSTALL=0
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
  echo "Usage: ./install.sh [--prefix DIR] [--uninstall]"
  echo "  --prefix DIR   install binary to DIR (default: \$HOME/.local/bin)"
  echo "  --uninstall    remove ghostline rc blocks (keeps binary unless --prefix given with it)"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX="${2:?}"; shift 2;;
    --uninstall) UNINSTALL=1; shift;;
    -h|--help) usage; exit 0;;
    *) echo "unknown flag: $1" >&2; usage; exit 2;;
  esac
done

have() { command -v "$1" >/dev/null 2>&1; }

remove_block() { # <rcfile> — delete lines between ghostline markers
  local rc="$1"
  [ -f "$rc" ] || return 0
  cp "$rc" "$rc.ghostline-bak"
  sed -i '/^# >>> ghostline >>>$/,/^# <<< ghostline <<<$/d' "$rc"
}

add_block() { # <rcfile> <shell> — append eval block once
  local rc="$1" shell="$2"
  [ -f "$rc" ] || return 0
  if grep -q '^# >>> ghostline >>>$' "$rc" 2>/dev/null; then
    echo "  already wired: $rc"
    return 0
  fi
  cp "$rc" "$rc.ghostline-bak"
  {
    echo ""
    echo "# >>> ghostline >>>"
    echo "if command -v ghostline >/dev/null 2>&1; then eval \"\$(ghostline init $shell)\"; fi"
    echo "# <<< ghostline <<<"
  } >> "$rc"
  echo "  wired: $rc (backup: $rc.ghostline-bak)"
}

if [ "$UNINSTALL" -eq 1 ]; then
  echo "Removing ghostline shell blocks..."
  remove_block "$HOME/.bashrc"
  remove_block "$HOME/.zshrc"
  remove_block "$HOME/.config/fish/config.fish"
  echo "Done. (Binary at $PREFIX/ghostline left in place; delete manually if wanted.)"
  exit 0
fi

if ! have cargo; then
  echo "error: cargo not found — install Rust first (https://rustup.rs)" >&2
  exit 1
fi

echo "Building ghostline (release)..."
cargo build --release --manifest-path "$REPO/Cargo.toml"

echo "Building bash native auto-ghost (optional, needs cc + bash headers)..."
if have cc && [ -f "$REPO/builtin/ghostline_autosuggest.c" ] && [ -f /usr/include/bash/builtins.h ]; then
  if cc -shared -fPIC -o "$REPO/target/release/ghostline_autosuggest.so" "$REPO/builtin/ghostline_autosuggest.c" -I/usr/include/bash -I/usr/include/bash/include 2>/dev/null; then
    echo "Built: target/release/ghostline_autosuggest.so (auto-ghost, no flicker)"
  else
    echo "NOTE: native auto-ghost build failed — falling back to on-demand shell ghost (Ctrl-G)."
  fi
else
  echo "NOTE: cc/bash headers missing — skipping native auto-ghost (on-demand shell ghost still works)."
fi

mkdir -p "$PREFIX"
cp "$REPO/target/release/ghostline" "$PREFIX/ghostline"
chmod +x "$PREFIX/ghostline"
if [ -f "$REPO/target/release/ghostline_autosuggest.so" ]; then
  cp "$REPO/target/release/ghostline_autosuggest.so" "$PREFIX/ghostline_autosuggest.so"
  echo "Installed: $PREFIX/ghostline_autosuggest.so (auto-ghost)"
fi
echo "Installed: $PREFIX/ghostline ($("$PREFIX/ghostline" --version))"

case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) echo "NOTE: $PREFIX is not on PATH. Add this to your rc file:"; echo "  export PATH=\"$PREFIX:\$PATH\"";;
esac

echo "Wiring shells..."
if have bash; then
  touch "$HOME/.bashrc"
  add_block "$HOME/.bashrc" bash
fi
if have zsh; then
  touch "$HOME/.zshrc"
  add_block "$HOME/.zshrc" zsh
fi
if have fish; then
  mkdir -p "$HOME/.config/fish"
  touch "$HOME/.config/fish/config.fish"
  # fish uses a different init syntax
  if grep -q '^# >>> ghostline >>>$' "$HOME/.config/fish/config.fish" 2>/dev/null; then
    echo "  already wired: $HOME/.config/fish/config.fish"
  else
    cp "$HOME/.config/fish/config.fish" "$HOME/.config/fish/config.fish.ghostline-bak"
    {
      echo ""
      echo "# >>> ghostline >>>"
      echo "if command -v ghostline >/dev/null 2>&1; ghostline init fish | source; end"
      echo "# <<< ghostline <<<"
    } >> "$HOME/.config/fish/config.fish"
    echo "  wired: $HOME/.config/fish/config.fish"
  fi
fi

echo ""
echo "Done. Open a new terminal (any of them — Alacritty, Kitty, etc.) and type a known command prefix."
echo "Tab accepts the dim ghost when visible (else normal completion); Right-arrow / Ctrl-F accepts, Alt-Right / Alt-F accepts a word, Ctrl-] dismisses."
echo "Typing stays native (no per-char refresh flicker). Disable ghost any time: export GHOSTLINE_GHOST=0. Pause logging: export GHOSTLINE_DISABLED=1."
