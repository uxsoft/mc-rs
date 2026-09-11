#!/usr/bin/env python3
"""Per-job controls with one throttled SFTP upload and one independent local copy."""
import os
from terminal_harness import Terminal
from pathlib import Path
import tempfile
from urllib.parse import urlparse, unquote

binary=Path(os.environ.get("MC_TEST_BINARY",Path(__file__).resolve().parents[1]/"target/debug/mc")).resolve()
remote=Path(unquote(urlparse(os.environ["MC_TEST_SFTP"]).path))
marker=remote/"slow-upload"
marker.touch()
with tempfile.TemporaryDirectory(prefix="mc-jobs-") as tmp:
    root=Path(tmp)
    source=root/"source";source.mkdir()
    (source/"a-slow.bin").write_bytes(b"x"*(16*1024*1024))
    (source/"b-quick.txt").write_text("independent copy")
    target=remote/"a-slow.bin"
    session = Terminal([binary, source, os.environ["MC_TEST_SFTP"]], rows=32, columns=150, env={})
    output = session.output
    pump, send = session.pump, session.send
    def wait_for(predicate):
        session.wait_for(predicate, timeout=30)
    try:
        wait_for(lambda:b"Remote authentication" in output)
        send(b"test-password\r");wait_for(lambda:b"hello.txt" in output)
        send(b"\x1b[H");send(b"\x1b[B");send(b"\x1b[15~");send(b"\r")
        wait_for(lambda:bool(list(remote.glob(".mc-upload-*"))))
        send(b"\x1b[B");send(b"\x1b[15~");send(b"\x15"+str(root/"quick.txt").encode()+b"\r")
        wait_for(lambda:(root/"quick.txt").exists())
        assert not target.exists(),"Upload must still be running"
        send(b"\x1b[20~");send(b"\x1b[B"*4);send(b"\r");send(b"\x0c")
        wait_for(lambda:b"retry failed" in output)
        send(b"c")  # selected completed local job; must not cancel the upload
        pump(.3);assert list(remote.glob(".mc-upload-*"))
        send(b"\x1b[A");send(b"\x0c")
        assert b"ETA" in output and b"/s" in output
        send(b"c");wait_for(lambda:not list(remote.glob(".mc-upload-*")))
        assert not target.exists() and (source/"a-slow.bin").exists()
        marker.unlink()
        send(b"r")
        output.clear();send(b"\x0c")
        wait_for(lambda:b"Remote authentication" in output)
        send(b"test-password\r")
        wait_for(lambda:target.exists())
        assert target.read_bytes()==(source/"a-slow.bin").read_bytes()
        assert (root/"quick.txt").read_text()=="independent copy"
        send(b"\r");pump(.5);send(b"\x1b[21~")
        session.wait_exit()
    finally:
        marker.unlink(missing_ok=True);target.unlink(missing_ok=True)
        session.close()
print("Job terminal tests passed: independent jobs, selected cancellation, progress, explicit reconnect/retry")
