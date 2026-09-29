#!/usr/bin/env python3
"""Pty end-to-end for ghostline bash ghost: real keypresses, not function calls.

Catches what scripted tests cannot: per-char whole-line refresh flicker
(`\\r\\e[K\\rP> ...` per keystroke from `bind -x` printable hooks).

Args: <snippet> <ghostline-bin> <workdir>
Asserts:
  1. typing a known prefix does NOT whole-line-refresh per char (native
     self-insert, no `bind -x` printable hooks) and no macro errors
  2. Ctrl-G previews the dim ghost on demand (deferred past redisplay)
  3. Right-arrow accepts the ghost, Enter runs the accepted command
"""
import os
import pty
import select
import subprocess
import sys
import time

snippet, bindir, workdir = sys.argv[1], sys.argv[2], sys.argv[3]
gl = os.path.join(bindir, "ghostline")
# Seed the DEFAULT db location (XDG) so `suggest` finds it without flags.
xdg = os.path.join(workdir, "xdg")
db = os.path.join(xdg, "ghostline", "history.db")
fails = []


def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name, detail)
    if not cond:
        fails.append(name)


# seed history with a harmless command
subprocess.run(
    [gl, "log", "--db", db, "--shell", "bash", "--exit", "0",
     "--cwd", workdir, "--session", "pty", "echo hello-world-xyz"],
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
send(f"source {snippet}\n".encode(), 0.8)
send(f"cd {workdir}\n".encode(), 0.8)
out = b""  # drop startup noise
for ch in b"echo hello":
    send(bytes([ch]), 0.25)
typing_phase = bytes(out)

check("no macro nesting error", b"nesting level" not in out)
# Flicker guard: native typing must not full-redisplay per char. Old per-char
# `bind -x` produced one `\r\e[K` per keystroke (10 chars -> ~10). On-demand
# keeps typing native: expect ~0 during the typing phase.
n_redraw = typing_phase.count(b"\r\x1b[K")
check("typing has no whole-line refresh", n_redraw <= 1, f"redraws={n_redraw}")
check("no auto-ghost before preview", b"-world-xyz" not in typing_phase)

send(b"\x07", 1.0)  # Ctrl-G: on-demand preview (deferred past redisplay)
check("ghost painted dim on preview", b"\x1b[2;" in out and b"-world-xyz" in out)
check("no job spam on preview", b"[1]" not in out)

send(b"\x1b[C", 0.6)  # Right arrow: accept full ghost
send(b"\r", 1.0)  # Enter: run accepted command
check("accepted command ran", b"hello-world-xyz" in out)

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
