#!/usr/bin/env python3
"""Unix PTY check: lazy encrypted archive navigation, masked retry, cat, copy and exit."""
import fcntl
import os
import pathlib
import pty
import select
import shutil
import signal
import struct
import tempfile
import termios
import time

project = pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='mc-archive-') as tmp:
    root = pathlib.Path(tmp)
    left, right = root / 'left', root / 'right'
    left.mkdir(); right.mkdir()
    shutil.copy(project / 'tests/fixtures/locked.zip', left / 'locked.zip')
    pid, fd = pty.fork()
    if pid == 0:
        os.environ['TERM'] = 'xterm-256color'
        os.execv(str(project / 'target/debug/mc'), ['mc', str(left), str(right)])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 110, 0, 0))
    output = bytearray()
    def pump(seconds=.2):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([fd], [], [], .02)[0]:
                try: data = os.read(fd, 65536)
                except OSError: return
                output.extend(data)
                if b'\x1b[6n' in data: os.write(fd, b'\x1b[1;1R')
    def send(data):
        os.write(fd, data); pump()
    def wait_for(predicate):
        until = time.monotonic() + 8
        while not predicate():
            assert time.monotonic() < until, output[-4000:]
            pump(.05)
    try:
        pump(.6)
        send(b'\x1b[B'); send(b'\r')  # Enter archive; plaintext headers need no password.
        send(b'\x0c')
        assert b'folder' in output and b'Unlock archive' not in output
        send(b'\x1b[B'); send(b' ')  # Select folder, size comes from metadata.
        pump(.2); send(b'\x0c')
        assert b'25' in output and b'1 selected' in output, output[-4000:]
        send(b'\x1b[H'); send(b'\x1b[B'); send(b'\r')  # folder
        send(b'\x1b[B'); send(b'\x1bOR')  # F3
        wait_for(lambda: b'Unlock archive' in output)
        output.clear(); send(b'wrong'); send(b'\x0c')
        assert b'wrong' not in output  # Typed password must never be rendered.
        send(b'\r'); pump(.5); send(b'\x0c')
        assert b'Password rejected' in output
        output.clear(); send(b'correct'); send(b'\x0c')
        assert b'correct' not in output
        send(b'\r')
        wait_for(lambda: b'archive cat smoke content' in output)
        send(b'\r')
        output.clear(); send(b'\x1b[15~'); send(b'\r')  # F5 reuses password.
        wait_for(lambda: (right / 'secret.txt').exists())
        assert (right / 'secret.txt').read_bytes() == b'archive cat smoke content\n'
        assert b'Unlock archive' not in output
        # F6 must leave archive data untouched.
        send(b'\x1b[17~'); send(b'\r'); send(b'\x0c')
        assert b'Read-only archive' in output
        send(b'\r')
        send(b'\x1b[H'); send(b'\r'); send(b'\x1b[H'); send(b'\r')
        send(b'\x0c')
        assert sorted(p.name for p in left.iterdir()) == ['locked.zip']
        # Reopening starts a fresh session; cancel its password prompt safely.
        send(b'\x1b[B'); send(b'\r'); send(b'\x1b[B'); send(b'\r'); send(b'\x1b[B')
        output.clear(); send(b'\x1bOR'); wait_for(lambda: b'Unlock archive' in output)
        send(b'\x1b'); pump(.3); send(b'\r')
        send(b'\x1b[21~'); pump(.3)
        result = os.waitpid(pid, os.WNOHANG)
        assert result[0] == pid and os.waitstatus_to_exitcode(result[1]) == 0, output[-4000:]
        print('Archive PTY passed: metadata size, masked password retry/cancel, cat stream, cached password, copy, read-only guard, parent exit')
    finally:
        try: os.kill(pid, signal.SIGKILL)
        except ProcessLookupError: pass
        try: os.waitpid(pid, 0)
        except ChildProcessError: pass
        os.close(fd)
