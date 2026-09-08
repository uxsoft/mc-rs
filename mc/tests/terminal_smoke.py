#!/usr/bin/env python3
"""Unix PTY integration smoke test. Run after cargo build; uses disposable files only."""
import fcntl
import os
import pathlib
import pty
import select
import signal
import struct
import tempfile
import termios
import time

binary = pathlib.Path(__file__).resolve().parents[1] / 'target/debug/mc'
with tempfile.TemporaryDirectory(prefix='mc-smoke-') as tmp:
    root = pathlib.Path(tmp)
    left, right = root / 'left', root / 'right'
    left.mkdir(); right.mkdir()
    (left / 'alpha.txt').write_text('cat smoke content\n')
    pid, fd = pty.fork()
    if pid == 0:
        os.environ['TERM'] = 'xterm-256color'
        os.environ['XDG_DATA_HOME'] = str(root / 'data')
        os.environ['VISUAL'] = str(root / 'missing-editor')
        os.execv(str(binary), [str(binary), str(left), str(right)])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 110, 0, 0))
    output = bytearray()
    def pump(seconds=.25):
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
        until=time.monotonic()+5
        while not predicate():
            assert time.monotonic()<until, output[-3000:]
            pump(.05)
    try:
        pump(.7)
        assert b'alpha.txt' in output
        send(b'\x1b[B')  # select alpha
        send(b'\x1bOR')  # F3 cat
        assert b'cat smoke content' in output
        send(b'\r')
        send(b'\x1bOS')  # F4 missing editor must restore a usable TUI
        assert b'External program' in output
        send(b'\r')
        send(b'\x1b[15~')  # F5 copy
        send(b'\r')
        wait_for(lambda: (right/'alpha.txt').exists())
        assert (right/'alpha.txt').read_text()=='cat smoke content\n'
        send(b'\x1b[17~')  # F6 move to a new name
        send(b'\x15'+str(right/'moved.txt').encode()+b'\r')
        wait_for(lambda: (right/'moved.txt').exists() and not (left/'alpha.txt').exists())
        send(b'\x1b[18~')  # F7 mkdir
        send(b'new-directory\r')
        wait_for(lambda: (left/'new-directory').is_dir())
        send(b'\x1b[H'); send(b'\x1b[B')
        send(b'\x1b[19~')  # F8 trash default
        send(b'\r')
        wait_for(lambda: not (left/'new-directory').exists())
        # An unusable trash location must keep the source, never permanently delete it.
        (root/'data').rename(root/'data-good')
        (root/'data').write_text('not a directory')
        (left/'trash-failure').write_text('keep me')
        send(b'\x12'); send(b'\x1b[H'); send(b'\x1b[B')
        send(b'\x1b[19~'); send(b'\r'); pump(.4)
        assert (left/'trash-failure').read_text() == 'keep me'
        (root/'data').unlink(); (root/'data-good').rename(root/'data')
        # Mouse function key opens help, then closes by keyboard.
        send(b'\x1b[<0;3;30M'); send(b'\x1b[<0;3;30m')
        assert b'Keyboard help' in output
        send(b'\r')
        # Search on the other panel and navigate to the exact matched file.
        send(b'\t'); send(b'\x1b?'); send(b'moved\r'); pump(.3); send(b'\r')
        pump(.3)
        send(b'\x1b[19~'); send(b'\t'); send(b'\r')  # explicitly permanent delete
        wait_for(lambda: not (right/'moved.txt').exists())
        # Space selects and advances across directories; both copy and move use the selection.
        for name, content in [('batch-a', 'abc'), ('batch-b', '12345')]:
            (left/name).mkdir()
            (left/name/'file').write_text(content)
        send(b'\t'); send(b'\x12'); send(b'\x1b[H'); send(b'\x1b[B')
        send(b'  '); pump(.3); send(b'\x0c')  # full redraw makes the total observable in the PTY stream
        assert b'8 bytes' in output
        send(b'\x1b[15~'); send(b'\r')
        wait_for(lambda: (right/'batch-a/file').exists() and (right/'batch-b/file').exists())
        moved_batch = right/'moved-batch'
        moved_batch.mkdir()
        send(b'\x1b[17~'); send(b'\x15'+str(moved_batch).encode()+b'\r')
        wait_for(lambda: not (left/'batch-a').exists() and not (left/'batch-b').exists())
        assert (moved_batch/'batch-a/file').read_text() == 'abc'
        assert (moved_batch/'batch-b/file').read_text() == '12345'
        # Resize still leaves quit usable.
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 10, 36, 0, 0)); os.kill(pid,signal.SIGWINCH); pump()
        send(b'\x1b[21~')
        done, status = os.waitpid(pid, 0)
        assert os.waitstatus_to_exitcode(status)==0
        assert b'\x1b[?1049l' in output and b'\x1b[?1000l' in output
        pathlib.Path('/tmp/mc-terminal-smoke.log').write_bytes(output)
        print('PTY smoke passed: cat, copy, move, mkdir, trash, permanent delete, search, mouse, Space multi-selection, directory sizes, batch copy/move, resize, clean exit')
    finally:
        try: os.kill(pid,signal.SIGKILL)
        except ProcessLookupError: pass
        os.close(fd)
