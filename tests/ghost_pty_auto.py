#!/usr/bin/env python3
"""Pty end-to-end for NATIVE auto-ghost (loadable readline hook).

Real keypresses through real bash+readline with ghostline_autosuggest.so.
Asserts auto-ghost without whole-line-refresh flicker:
  1. typing a known prefix auto-paints dim ghost, no Ctrl-G needed
  2. typing causes ~0 full-line redraws (\\r\\e[K), no [1] job spam, no macro errors
  3. Right-arrow accepts, Enter runs the accepted command
  4. fast burst (2nd key inside the old 100ms throttle window) still
     re-ranks: no stale tail interleaved with typed text (`coear` bug)

Args: <snippet> <ghostline-bin> <workdir> <so-path>
Skips (exit 0, prints SKIP) when the .so is missing.
"""
import os
import pty
import select
import subprocess
import sys
import time

snippet, bindir, workdir, so = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
if not os.path.isfile(so):
    print("SKIP no .so")
    sys.exit(0)
gl = os.path.join(bindir, "ghostline")
xdg = os.path.join(workdir, "xdg")
db = os.path.join(xdg, "ghostline", "history.db")
fails = []


def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name, detail)
    if not cond:
        fails.append(name)


subprocess.run(
    [gl, "log", "--db", db, "--shell", "bash", "--exit", "0",
     "--cwd", workdir, "--session", "pty", "echo hello-world-xyz"],
    check=True, capture_output=True,
)
# Markers for the burst test: history-only (deterministic, no PATH
# dependence). Seeded codex FIRST so `c` ghosts the clear marker (newer
# last_seen wins ties); then `co` diverges from the cached clear entry and
# must re-rank via a fresh fork — the old 100ms throttle skipped that fork
# and left the stale tail (`coear` bug).
for cmd in ["codex-pty-marker-xyz", "clear-pty-marker-xyz"]:
    subprocess.run(
        [gl, "log", "--db", db, "--shell", "bash", "--exit", "0",
         "--cwd", workdir, "--session", "pty", cmd],
        check=True, capture_output=True,
    )

env = dict(
    os.environ,
    TERM="xterm",
    PS1="P> ",
    PATH=bindir + ":" + os.environ.get("PATH", ""),
    XDG_DATA_HOME=xdg,
    GHOSTLINE_SESSION="pty",
)
os.makedirs(workdir, exist_ok=True)

pid, fd = pty.fork()
if pid == 0:
    os.execvpe("bash", ["bash", "--noprofile", "--norc", "-i"], env)

out = b""


def drain(t=0.6):
    global out
    end = time.time() + t
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try:
                out += os.read(fd, 65536)
            except OSError:
                break


def send(data, wait=0.6):
    os.write(fd, data)
    drain(wait)


drain(1.0)
# Pure-C mode: the snippet auto-loads install/ghostline_autosuggest.so
# itself (same side-by-side layout as a real user install). No manual
# enable here — that would mix hook + shell-fallback states.
send(f"source {snippet}\n".encode(), 0.8)
send(f"cd {workdir}\n".encode(), 0.8)
out = b""
for ch in b"echo hello":
    send(bytes([ch]), 0.25)

check("no macro nesting error", b"nesting level" not in out)
check("auto ghost painted dim", b"\x1b[2;" in out and b"-world-xyz" in out)
n_redraw = out.count(b"\r\x1b[K")
check("no whole-line refresh while typing", n_redraw <= 1, f"redraws={n_redraw}")
check("no job spam", b"[1]" not in out)

send(b"\x1b[C", 0.6)
send(b"\r", 1.0)
check("accepted command ran", b"hello-world-xyz" in out)

# Burst: fire `o` the moment `c`'s ghost paint lands (tight 10ms poll, no
# wall-time gap guess). Gap from `c`'s fork is ~ms — deep inside the old
# 100ms throttle window. Ghost must re-rank to the codex marker, not leave
# the clear tail interleaved (`co` + stale `lear` rendered as `coear`).
send(b"\x03", 0.5)  # fresh prompt
out = b""
os.write(fd, b"c")
t_end = time.time() + 3.0
while time.time() < t_end:
    r, _, _ = select.select([fd], [], [], 0.01)
    if r:
        try:
            out += os.read(fd, 65536)
        except OSError:
            break
    if b"lear-pty-marker-xyz" in out:
        break
os.write(fd, b"o")
drain(0.6)
check("burst re-ranks ghost", b"dex-pty-marker-xyz" in out and b"\x1b[2;" in out)

try:
    os.write(fd, b"\x03")
except OSError:
    pass
drain(0.3)
try:
    os.close(fd)
except OSError:
    pass
_, _ = os.waitpid(pid, 0)

print("RESULT:", "FAIL" if fails else "OK", fails)
sys.exit(1 if fails else 0)
