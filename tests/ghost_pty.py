#!/usr/bin/env python3
"""Pty end-to-end for ghostline bash ghost: real keypresses, not function calls.

Catches what scripted tests cannot: readline macro recursion
("maximum macro execution nesting level exceeded").

Args: <snippet> <ghostline-bin> <workdir>
Asserts:
  1. typing a known prefix paints the dim ghost, no macro errors
  2. Right-arrow accepts the ghost, Enter runs the accepted command
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

check("no macro nesting error", b"nesting level" not in out)
check("ghost painted dim", b"\x1b[2;" in out and b"-world-xyz" in out)

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
