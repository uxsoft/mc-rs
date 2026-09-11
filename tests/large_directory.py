#!/usr/bin/env python3
"""Large local directory navigation and idle automatic refresh, using disposable files."""
import os
from terminal_harness import Terminal
from pathlib import Path
import tempfile
import time
binary=Path(os.environ.get("MC_TEST_BINARY",Path(__file__).resolve().parents[1]/"target/debug/mc")).resolve()
with tempfile.TemporaryDirectory(prefix="mc-large-") as tmp:
    root=Path(tmp)
    for i in range(20000):(root/f"entry-{i:05}").touch()
    started=time.monotonic()
    session = Terminal([binary, root, root], rows=30, columns=120, env={})
    output = session.output
    pump, send, wait_for = session.pump, session.send, session.wait_for
    try:
        wait_for(lambda:b"entry-" in output)
        first=time.monotonic()-started
        send(b"\x1bOP");wait_for(lambda:b"Keyboard help" in output);send(b"\r")
        until=time.monotonic()+45
        while b"20000 items" not in output:
            assert time.monotonic()<until,output[-3000:]
            send(b"\x0c")
            pump(.2)
        (root/"aaa-auto-refresh").touch()
        refreshed=time.monotonic()
        wait_for(lambda:b"aaa-auto-refresh" in output)
        refreshed=time.monotonic()-refreshed
        send(b"\x1b[21~")
        session.wait_exit()
        print(f"Large directory PTY passed: 20,000 entries, first entry {first:.2f}s, automatic refresh {refreshed:.2f}s, keyboard response and clean exit")
    finally:
        session.close()
