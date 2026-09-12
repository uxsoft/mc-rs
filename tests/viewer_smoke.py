#!/usr/bin/env python3
"""Unix PTY integration: built-in formats, scrolling, resize, read-only input, cleanup."""
import os
from terminal_harness import Terminal
from pathlib import Path
import re
import shutil
import tempfile
import struct
import zlib

project = Path(__file__).resolve().parents[1]
binary = Path(os.environ.get("MC_TEST_BINARY", project / "target/debug/mc-rs")).resolve()
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
    if "MC_TEST_IMAGE" not in os.environ:
        # Known solid pixels distinguish image painting from every UI theme color.
        def chunk(kind, data):
            return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
        png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 32, 32, 8, 2, 0, 0, 0))
        png += chunk(b"IDAT", zlib.compress((b"\0" + bytes([233, 47, 71]) * 32) * 32)) + chunk(b"IEND", b"")
        (left / "03.png").write_bytes(png)
    (left / "05.pdf").write_bytes(b"%PDF-broken")
    originals = {p.name: p.read_bytes() for p in left.iterdir()}
    session = Terminal([binary, left, right], rows=30, columns=110, env={"TMPDIR": cache, "NO_COLOR": "1", **{name: None for name in ("KITTY_WINDOW_ID", "TERM_PROGRAM", "LC_TERMINAL", "TMUX", "WEZTERM_EXECUTABLE", "KONSOLE_VERSION")}}, kitty=kitty)
    output = session.output
    pump, send, wait_for = session.pump, session.send, session.wait_for

    def repaint():
        output.clear()
        session.screen.cells.clear()
        send(b"\x0c")

    def open_next():
        send(b"q")
        send(b"\x1b[B")
        output.clear()
        session.screen.cells.clear()
        send(b"\x1bOR")

    def wait_for_image(known=False):
        if kitty:
            wait_for(b"\x1b_Gq=2,i=")
            # A large transfer in row one used to make Ratatui skip every later row.
            # Check the final row's explicit Kitty diacritic, not merely a transfer.
            wait_for("\U0010eeee\u0357\u0305".encode())  # row 15, column 0
        else:
            def painted():
                for (row, col), (_, fg, bg) in session.screen.cells.items():
                    if not (1 <= row < 17 and 0 <= col < 70):
                        continue
                    if known and "MC_TEST_IMAGE" not in os.environ:
                        if (233, 47, 71) in (fg, bg):
                            return True
                    elif fg not in (None, (210, 219, 230), (96, 210, 190), (123, 138, 156)) or bg not in (None, (19, 23, 31)):
                        return True
                return False
            wait_for(painted)

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
        session.resize(18, 70)
        pump(0.3)
        repaint()
        wait_for(b"LAST ROW")
        open_next()
        wait_for(b"Markdown")
        wait_for(b"Viewer example")
        assert b"# Viewer example" not in output
        open_next()
        wait_for(b"Image")
        wait_for_image(known=True)
        send(b"++\x1b[B\x1b[C0")
        # A steady frame/repaint must keep the image visible after first transmission.
        pump(0.4)
        repaint()
        if kitty:
            wait_for("\U0010eeee\u0357\u0305".encode())
        else:
            wait_for_image(known=True)
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
        session.wait_exit()
        print("Viewer PTY passed: menu/F3, 100,001 rows, keyboard/mouse scroll, resize, Markdown, images, PDF pages, read-only keys, errors, cleanup")
    finally:
        session.close()
