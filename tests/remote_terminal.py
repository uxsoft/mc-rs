#!/usr/bin/env python3
"""Invoked by remote_servers.py with disposable server endpoints/known_hosts."""
import os
from terminal_harness import Terminal
from pathlib import Path
import tempfile

binary = Path(os.environ.get("MC_TEST_BINARY", Path(__file__).resolve().parents[1] / "target/debug/mc-rs")).resolve()
for key in ["MC_TEST_FTP", "MC_TEST_SFTP", "MC_TEST_SSH"]:
    with tempfile.TemporaryDirectory(prefix="mc-remote-dest-") as destination:
        session = Terminal([binary, os.environ[key], destination], rows=30, columns=140, env={})
        output = session.output
        pump, send, wait_for = session.pump, session.send, session.wait_for
        try:
            wait_for(lambda: b"Remote authentication" in output)
            send(b"wrong-password\r")
            pump(4)
            send(b"\x0c")
            wait_for(lambda: b"Authentication rejected" in output)
            send(b"test-password\r")
            wait_for(lambda: b"hello.txt" in output)
            assert b"wrong-password" not in output and b"test-password" not in output
            send(b"\x1b[H")  # parent row
            send(b"\x1b[B")  # folder
            send(b" ")  # select directory and advance to hello.txt
            wait_for(lambda: b"selected" in output)
            send(b"\x1b[A"); send(b" ")  # deselect folder; cursor returns to hello.txt
            send(b"\x1bOR")  # F3
            wait_for(lambda: b"hello world" in output)
            send(b"\x1b")
            send(b"\x1b[15~"); send(b"\r")  # F5 copy to local panel
            wait_for(lambda: Path(destination, "hello.txt").exists())
            assert Path(destination, "hello.txt").read_text() == "hello world"
            pump(0.4)
            send(b"\x1b[21~")  # F10
            session.wait_exit()
        finally:
            session.close()
print("Remote terminal tests passed: FTP/SFTP/SSH startup, masked retry, selection, viewer, copy, exit")
