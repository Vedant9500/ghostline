#!/usr/bin/env python3
"""Pty regression: ghost paint must never cross the right margin.

Typing a long line at a narrow width, every ghost paint's suffix must fit
in the space left on the last screen row (prompt + line + ghost stay in
the rows readline already tracks). Otherwise the wrapped ghost desyncs
readline's redisplay and long lines render garbled/truncated (e.g. a
leading `<`, i.e. no native wrap) instead of wrapping onto the next line.

Args: <snippet> <bindir/installdir> <workdir>
Asserts per keystroke k (1-based typed length):
  - every painted suffix is a prefix of the true remainder LONG[k:]
  - its display width <= avail_k = COLS - ((PROMPT + k) % COLS),
    and no paint at all when (PROMPT + k) % COLS == 0 (margin edge)
Fails loudly (not vacuously) when the width never took effect.
"""
import fcntl
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time

snippet, bindir, workdir = sys.argv[1], sys.argv[2], sys.argv[3]
COLS = 60
PROMPT = "P> "
LONG = "python3 src/ab/render.py --scene caustics_beach --backend unified --spp 4 --W 1920 --H 1920"
fails = []


def check(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name, detail)
    if not cond:
        fails.append(name)


gl = os.path.join(bindir, "ghostline")
xdg = os.path.join(workdir, "xdg_wrap")
db = os.path.join(xdg, "ghostline", "history.db")
subprocess.run(
    [gl, "log", "--db", db, "--shell", "bash", "--exit", "0",
     "--cwd", workdir, "--session", "pty", LONG],
    check=True, capture_output=True,
)

env = dict(
    os.environ,
    TERM="xterm",
    PS1=PROMPT,
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
        r, _, _ = select.select([fd], [], [], 0.15)
        if r:
            try:
                out += os.read(fd, 65536)
            except OSError:
                break


def send(data, wait=0.5):
    os.write(fd, data)
    drain(wait)


PAINT = re.compile(rb"\x1b\[s\x1b\[K\x1b\[[0-9;]*m(.*?)\x1b\[m\x1b\[u")

drain(1.0)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, COLS, 0, 0))
os.kill(pid, signal.SIGWINCH)
time.sleep(0.5)
send(f"source {snippet}\n".encode(), 0.8)
send(b"stty size; echo COLUMNS=$COLUMNS\n", 0.8)
check("width took effect", b"24 60" in out and b"COLUMNS=60" in out,
      repr(out[-200:]))
send(f"cd {workdir}\n".encode(), 0.5)

n_paints = 0
n_checked = 0
out = b""
for k, ch in enumerate(LONG.encode(), start=1):
    os.write(fd, bytes([ch]))
    drain(0.25)
    seg, out = bytes(out), b""
    for m in PAINT.finditer(seg):
        suf = m.group(1)
        n_paints += 1
        rem = LONG.encode()[k:]
        ok_prefix = rem.startswith(suf)
        used = len(PROMPT) + k
        r = used % COLS
        avail = 0 if r == 0 else COLS - r
        ok_width = len(suf) <= avail  # ASCII line: width == bytes
        n_checked += 1
        if not (ok_prefix and ok_width):
            check(f"paint fits at k={k}", False,
                  f"suf={suf!r} avail={avail} rem_head={rem[:avail+5]!r}")
            fails.append(f"k={k}")
    if (len(PROMPT) + k) % COLS == 0 and PAINT.search(seg):
        check(f"no paint at margin edge k={k}", False, "painted with 0 room")

check("ghost was active (non-vacuous)", n_paints >= 50, f"paints={n_paints}")
check("all paints within margin", not any(f.startswith("k=") for f in fails))

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
