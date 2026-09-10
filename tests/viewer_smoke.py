#!/usr/bin/env python3
"""Unix PTY integration: built-in formats, scrolling, resize, read-only input, cleanup."""
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import tempfile
import termios
import time

project = Path(__file__).resolve().parents[1]
binary = Path(os.environ.get("MC_TEST_BINARY", project / "target/debug/mc")).resolve()
kitty = os.environ.get("MC_TEST_GRAPHICS") == "kitty"
test_image = Path(os.environ.get("MC_TEST_IMAGE", project / "tests/fixtures/viewer.png"))
rgb = re.compile(rb"\x1b\[(?:38|48);2;\d+;\d+;\d+(?:;|m)")
with tempfile.TemporaryDirectory(prefix="mc-viewer-") as tmp:
    root = Path(tmp)
    left, right, cache = root / "left", root / "right", root / "cache"
    for path in (left, right, cache):
        path.mkdir()
    text = "".join(f"row {n:05d}\n" for n in range(100_000)) + "LAST ROW\n"
    (left / "01.txt").write_text(text)
    for source, target in [("viewer.md", "02.md"), ("viewer.png", "03.png"), ("viewer.pdf", "04.pdf")]:
        shutil.copyfile(test_image if source == "viewer.png" else project / "tests/fixtures" / source, left / target)
    (left / "05.pdf").write_bytes(b"%PDF-broken")
    originals = {p.name: p.read_bytes() for p in left.iterdir()}
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.environ["TMPDIR"] = str(cache)
        # Pixel colors must survive NO_COLOR; the panels must still honor it.
        os.environ["NO_COLOR"] = "1"
        for name in ("KITTY_WINDOW_ID", "TERM_PROGRAM", "LC_TERMINAL", "TMUX", "WEZTERM_EXECUTABLE", "KONSOLE_VERSION"):
            os.environ.pop(name, None)
        os.execv(str(binary), [str(binary), str(left), str(right)])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 110, 0, 0))
    output = bytearray()
    queried = False

    def pump(seconds=0.15):
        global queried
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([fd], [], [], 0.02)[0]:
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    return
                output.extend(data)
                if kitty and not queried and b"\x1b[5n" in output:
                    queried = True
                    os.write(fd, b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c\x1b[6;22;11t\x1b[0n")
                if b"\x1b[6n" in data:
                    os.write(fd, b"\x1b[1;1R")

    def send(data):
        os.write(fd, data)
        pump()

    def wait_for(value):
        until = time.monotonic() + 15
        while value not in output:
            assert time.monotonic() < until, output[-4000:]
            pump(0.05)

    def repaint():
        output.clear()
        send(b"\x0c")

    def open_next():
        send(b"q")
        send(b"\x1b[B")
        output.clear()
        send(b"\x1bOR")

    def wait_for_image():
        if kitty:
            wait_for(b"\x1b_Gq=2,i=")
            # A large transfer in row one used to make Ratatui skip every later row.
            # Check the final row's explicit Kitty diacritic, not merely a transfer.
            wait_for("\U0010eeee\u0357\u0305".encode())  # row 15, column 0
        else:
            until = time.monotonic() + 15
            while not rgb.search(output):
                assert time.monotonic() < until, "Image pixels have no terminal colors"
                pump(0.05)

    try:
        pump(0.7)
        wait_for(b"01.txt")
        assert not rgb.search(output), "Panels should honor NO_COLOR"
        send(b"\x1b[B")
        # View menu entry routes to the same F3 viewer.
        send(b"\x1b[20~")
        send(b"\x1b[C")
        send(b"\r")
        wait_for(b"Read-only")
        wait_for(b"row 00000")
        send(b"\x1b[6~")
        pump(0.2)
        repaint()
        wait_for(b"row 00028")
        send(b"\x1b[<65;20;10M")  # mouse wheel down
        pump(0.2)
        repaint()
        wait_for(b"row 00031")
        send(b"\x1b[F")
        wait_for(b"LAST ROW")
        for key in (b"\x1bOQ", b"\x1bOS", b"\x1b[15~", b"\x1b[19~", b"\x1b[21~", b"\x1b[3~", b"abc"):
            send(key)
        repaint()
        wait_for(b"Read-only")
        assert not list(right.iterdir())
        # Resize at the end must keep the final row visible.
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 18, 70, 0, 0))
        os.kill(pid, signal.SIGWINCH)
        pump(0.3)
        repaint()
        wait_for(b"LAST ROW")
        open_next()
        wait_for(b"Markdown")
        wait_for(b"Viewer example")
        assert b"# Viewer example" not in output
        open_next()
        wait_for(b"Image")
        wait_for_image()
        send(b"++\x1b[B\x1b[C0")
        # A steady frame/repaint must keep the image visible after first transmission.
        pump(0.4)
        repaint()
        if kitty:
            wait_for("\U0010eeee".encode())
        else:
            wait_for_image()
        open_next()
        wait_for(b"page 1/2")
        wait_for_image()
        send(b"n")
        pump(0.4)
        repaint()
        wait_for(b"page 2/2")
        send(b"p")
        pump(0.4)
        repaint()
        wait_for(b"page 1/2")
        open_next()
        wait_for(b"Cannot view file")
        send(b"\x1b")
        repaint()
        wait_for(b"01.txt")
        assert not rgb.search(output), "Image color override leaked into the panels"
        assert {p.name: p.read_bytes() for p in left.iterdir()} == originals
        assert not list(cache.iterdir()), "Viewer temporary paths leaked"
        send(b"\x1b[21~")
        until = time.monotonic() + 5
        while True:
            done, status = os.waitpid(pid, os.WNOHANG)
            if done:
                assert os.waitstatus_to_exitcode(status) == 0
                break
            assert time.monotonic() < until
            pump()
        print("Viewer PTY passed: menu/F3, 100,001 rows, keyboard/mouse scroll, resize, Markdown, images, PDF pages, read-only keys, errors, cleanup")
    finally:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(pid, 0)
        except ChildProcessError:
            pass
        os.close(fd)
