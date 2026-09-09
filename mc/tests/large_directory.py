#!/usr/bin/env python3
"""Large local directory navigation and idle automatic refresh, using disposable files."""
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
binary=Path(os.environ.get("MC_TEST_BINARY",Path(__file__).resolve().parents[1]/"target/debug/mc")).resolve()
with tempfile.TemporaryDirectory(prefix="mc-large-") as tmp:
    root=Path(tmp)
    for i in range(20000):(root/f"entry-{i:05}").touch()
    started=time.monotonic()
    pid,fd=pty.fork()
    if pid==0:
        os.environ["TERM"]="xterm-256color"
        os.execv(str(binary),[str(binary),str(root),str(root)])
    fcntl.ioctl(fd,termios.TIOCSWINSZ,struct.pack("HHHH",30,120,0,0))
    output=bytearray()
    def pump(seconds=.1):
        until=time.monotonic()+seconds
        while time.monotonic()<until:
            if select.select([fd],[],[],.02)[0]:
                try:data=os.read(fd,65536)
                except OSError:return
                output.extend(data)
                if b"\x1b[6n" in data:os.write(fd,b"\x1b[1;1R")
    def send(data):os.write(fd,data);pump()
    def wait_for(predicate):
        until=time.monotonic()+15
        while not predicate():
            assert time.monotonic()<until,output[-3000:]
            pump()
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
        until=time.monotonic()+5
        while True:
            done,status=os.waitpid(pid,os.WNOHANG)
            if done:assert status==0;break
            assert time.monotonic()<until
            pump()
        print(f"Large directory PTY passed: 20,000 entries, first entry {first:.2f}s, automatic refresh {refreshed:.2f}s, keyboard response and clean exit")
    finally:
        try:os.kill(pid,signal.SIGKILL)
        except ProcessLookupError:pass
        os.close(fd)
