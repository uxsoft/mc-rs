#!/usr/bin/env python3
"""Invoked by remote_servers.py with disposable server endpoints/known_hosts."""
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import tempfile
import termios
import time

binary = Path(os.environ.get("MC_TEST_BINARY", Path(__file__).resolve().parents[1] / "target/debug/mc")).resolve()
for key in ["MC_TEST_FTP", "MC_TEST_SFTP", "MC_TEST_SSH"]:
    with tempfile.TemporaryDirectory(prefix="mc-remote-dest-") as destination:
        pid, fd = pty.fork()
        if pid == 0:
            os.environ["TERM"] = "xterm-256color"
            os.execv(str(binary), [str(binary), os.environ[key], destination])
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 140, 0, 0))
        output = bytearray()

        def pump(seconds=0.15):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if select.select([fd], [], [], 0.02)[0]:
                    try:
                        data = os.read(fd, 65536)
                    except OSError:
                        return
                    output.extend(data)
                    if b"\x1b[6n" in data:
                        os.write(fd, b"\x1b[1;1R")

        def send(data):
            os.write(fd, data)
            pump()

        def wait_for(predicate):
            until = time.monotonic() + 15
            while not predicate():
                assert time.monotonic() < until, (key, __import__("re").sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", output)[-5000:])
                pump()

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
            send(b"\r")
            send(b"\x1b[15~"); send(b"\r")  # F5 copy to local panel
            wait_for(lambda: Path(destination, "hello.txt").exists())
            assert Path(destination, "hello.txt").read_text() == "hello world"
            pump(0.4)
            send(b"\x1b[21~")  # F10
            until = time.monotonic() + 5
            while True:
                done, status = os.waitpid(pid, os.WNOHANG)
                if done:
                    assert status == 0
                    break
                assert time.monotonic() < until, output[-3000:]
                pump()
        finally:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(fd)
print("Remote terminal tests passed: FTP/SFTP/SSH startup, masked retry, selection, cat, copy, exit")
