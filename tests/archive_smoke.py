#!/usr/bin/env python3
"""Unix PTY check: lazy encrypted archive navigation, masked retry, viewer, copy and exit."""
import os
from terminal_harness import Terminal
import pathlib
import shutil
import tempfile

project = pathlib.Path(__file__).resolve().parents[1]
binary = pathlib.Path(os.environ.get('MC_TEST_BINARY', project / 'target/debug/mc')).resolve()
with tempfile.TemporaryDirectory(prefix='mc-archive-') as tmp:
    root = pathlib.Path(tmp)
    left, right = root / 'left', root / 'right'
    left.mkdir(); right.mkdir()
    shutil.copy(project / 'tests/fixtures/locked.zip', left / 'locked.zip')
    session = Terminal([binary, left, right], rows=30, columns=110, env={})
    output = session.output
    pump, send, wait_for = session.pump, session.send, session.wait_for
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
        send(b'\x1b')
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
        send(b'\x1b'); pump(.3); send(b'\x1b')
        send(b'\x1b[21~'); pump(.3)
        session.wait_exit()
        print('Archive PTY passed: metadata size, masked password retry/cancel, built-in viewer, cached password, copy, read-only guard, parent exit')
    finally:
        session.close()
