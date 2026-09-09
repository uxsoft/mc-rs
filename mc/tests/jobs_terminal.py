#!/usr/bin/env python3
"""Per-job controls with one throttled SFTP upload and one independent local copy."""
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
    pid,fd=pty.fork()
    if pid==0:
        os.environ["TERM"]="xterm-256color"
        os.execv(str(binary),[str(binary),str(source),os.environ["MC_TEST_SFTP"]])
    fcntl.ioctl(fd,termios.TIOCSWINSZ,struct.pack("HHHH",32,150,0,0))
    output=bytearray()
    def pump(seconds=.15):
        until=time.monotonic()+seconds
        while time.monotonic()<until:
            if select.select([fd],[],[],.02)[0]:
                try:data=os.read(fd,65536)
                except OSError:return
                output.extend(data)
                if b"\x1b[6n" in data:os.write(fd,b"\x1b[1;1R")
    def send(data):os.write(fd,data);pump()
    def wait_for(predicate):
        until=time.monotonic()+30
        while not predicate():
            assert time.monotonic()<until,output[-5000:]
            pump()
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
        until=time.monotonic()+5
        while True:
            done,status=os.waitpid(pid,os.WNOHANG)
            if done:assert status==0;break
            assert time.monotonic()<until
            pump()
    finally:
        marker.unlink(missing_ok=True);target.unlink(missing_ok=True)
        try:os.kill(pid,signal.SIGKILL)
        except ProcessLookupError:pass
        os.close(fd)
print("Job terminal tests passed: independent jobs, selected cancellation, progress, explicit reconnect/retry")
