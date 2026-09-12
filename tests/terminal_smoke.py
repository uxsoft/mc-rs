#!/usr/bin/env python3
"""Unix PTY integration smoke test. Run after cargo build; uses disposable files only."""
import os
from terminal_harness import Terminal
import pathlib
import tempfile

binary = pathlib.Path(os.environ.get('MC_TEST_BINARY', pathlib.Path(__file__).resolve().parents[1] / 'target/debug/mc-rs')).resolve()
with tempfile.TemporaryDirectory(prefix='mc-smoke-') as tmp:
    root = pathlib.Path(tmp)
    left, right = root / 'left', root / 'right'
    left.mkdir(); right.mkdir()
    (left / 'alpha.txt').write_text('cat smoke content\n')
    session = Terminal([binary, left, right], rows=30, columns=110, env={"XDG_DATA_HOME": root / "data", "VISUAL": root / "missing-editor"})
    output = session.output
    pump, send, wait_for = session.pump, session.send, session.wait_for
    try:
        pump(.7)
        assert b'alpha.txt' in output
        send(b'alpha')  # Typing in a pane starts quick navigation directly.
        output.clear()
        send(b'\x0c')  # Repaint to inspect a complete frame.
        assert b'Find: alpha' in output
        for name in ['renamed é.txt', 'alpha.txt']:
            send(b'\x1bOQ')  # F2 renames the highlighted item in place.
            assert b'Rename in place' in output
            send(b'\x15' + name.encode() + b'\r')
            wait_for(lambda: (left / name).exists())
            pump(.2)  # Allow the completed job's listing to reveal the new name.
        send(b'\x1b[H')  # Home clears the query and returns to the parent row.
        send(b'\x1b[20~')  # F9 File dropdown
        assert b'Create directory' in output
        send(b'\x1b[C')  # switch to View
        send(b'\x0c')
        assert b'Show hidden files' in output
        send(b'\x1b[F'); send(b'\r')  # Refresh action closes the menu
        send(b'\x1b[<0;7;1M'); send(b'\x1b[<0;7;1m')  # File title
        send(b'\x1b[<0;12;7M'); send(b'\x1b[<0;12;7m')  # Background jobs
        send(b'\x0c')
        assert b'Background jobs' in output and b'Rename' in output
        send(b'\r')
        send(b'\x1b[<0;18;1M'); send(b'\x1b[<0;18;1m')  # Go title
        send(b'\x1b[B'); send(b'\r')  # Find dialog
        send(b'\x0c')
        assert b'Find filename' in output
        send(b'\x1b')  # close dialog
        send(b'\x1b[B')  # select alpha
        send(b'\x1bOR')  # F3 built-in viewer
        wait_for(lambda: b'cat smoke content' in output and b'Read-only' in output)
        send(b'\x1b')
        # Enter still uses external cat.
        output.clear(); send(b'\r')
        wait_for(lambda: b'Press Enter to return' in output)
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
        session.resize(10, 36); pump()
        send(b'\x1b[21~')
        session.wait_exit()
        assert b'\x1b[?1049l' in output and b'\x1b[?1000l' in output
        pathlib.Path('/tmp/mc-terminal-smoke.log').write_bytes(output)
        print('PTY smoke passed: built-in viewer, Enter cat, copy, move, mkdir, trash, permanent delete, search, mouse, Space multi-selection, directory sizes, batch copy/move, resize, clean exit')
    finally:
        session.close()
