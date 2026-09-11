"""Shared Unix PTY lifecycle and bounded, incremental terminal-query handling."""
import fcntl
import os
import pty
import select
import signal
import struct
import termios
import time


class Terminal:
    def __init__(self, args, rows=30, columns=110, env=None, kitty=False):
        self.screen = Screen()
        self.output = bytearray()
        self.pending = b""
        self.kitty = kitty
        self.queried = False
        self.status = None
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            child_env = dict(os.environ, TERM="xterm-256color")
            for key, value in (env or {}).items():
                if value is None:
                    child_env.pop(key, None)
                else:
                    child_env[key] = str(value)
            os.execve(str(args[0]), list(map(str, args)), child_env)
        self.resize(rows, columns)

    def resize(self, rows, columns):
        self.screen.rows, self.screen.columns = rows, columns
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        os.kill(self.pid, signal.SIGWINCH)

    def feed(self, data):
        self.screen.feed(data)
        self.output.extend(data)
        del self.output[:-4 * 1024 * 1024]
        data = self.pending + data
        # Keep only an incomplete suffix; queries split across reads still get one reply.
        queries = {b"\x1b[6n": b"\x1b[1;1R", b"\x1b[5n": b"\x1b[0n"}
        for query, reply in queries.items():
            for _ in range(data.count(query)):
                if query == b"\x1b[5n" and self.kitty and not self.queried:
                    self.queried = True
                    reply = b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c\x1b[6;22;11t\x1b[0n"
                os.write(self.fd, reply)
        self.pending = next((data[-n:] for n in range(3, 0, -1)
                             if any(q.startswith(data[-n:]) for q in queries)), b"")

    def pump(self, seconds=.15):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([self.fd], [], [], min(.02, max(0, until-time.monotonic())))[0]:
                try:
                    data = os.read(self.fd, 65536)
                except OSError:
                    return
                if not data:
                    return
                self.feed(data)

    def send(self, data):
        os.write(self.fd, data)
        self.pump()

    def wait_for(self, predicate, timeout=15):
        until = time.monotonic() + timeout
        if not callable(predicate):
            value = predicate
            predicate = lambda: value in self.output
        while not predicate():
            assert time.monotonic() < until, self.output[-5000:]
            self.pump(.05)

    def wait_exit(self, timeout=5):
        until = time.monotonic() + timeout
        while self.status is None:
            done, status = os.waitpid(self.pid, os.WNOHANG)
            if done:
                self.status = status
                break
            assert time.monotonic() < until, self.output[-5000:]
            self.pump(.05)
        assert os.waitstatus_to_exitcode(self.status) == 0, self.output[-5000:]

    def close(self):
        if self.status is None:
            try:
                os.kill(self.pid, signal.SIGKILL)
                os.waitpid(self.pid, 0)
            except (ProcessLookupError, ChildProcessError):
                pass
        os.close(self.fd)

class Screen:
    """Small incremental VT screen used to verify painted cells, not stale output."""
    def __init__(self):
        import codecs
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.pending = ''
        self.string_escape = False
        self.rows, self.columns = 30, 110
        self.row = self.col = 0
        self.fg = self.bg = None
        self.cells = {}

    def feed(self, data):
        import re
        import unicodedata
        text = self.pending + self.decoder.decode(data)
        self.pending = ''
        if self.string_escape:
            end = text.find('\x1b\\')
            if end < 0:
                self.pending = '\x1b' if text.endswith('\x1b') else ''
                return
            text = text[end+2:]
            self.string_escape = False
        while text:
            if text.startswith('\x1b'):
                if len(text) < 2:
                    break
                if text[1] in '_P]':
                    end = text.find('\x1b\\', 2)
                    if end < 0:
                        self.string_escape = True
                        self.pending = '\x1b' if text.endswith('\x1b') else ''
                        return
                    text = text[end+2:]
                    continue
                if text[1] == '[':
                    match = re.match(r'\x1b\[([0-?]*)([ -/]*)([@-~])', text)
                    if not match:
                        break
                    args, _, command = match.groups()
                    numbers = [int(n or 0) for n in args.split(';')] if not args.startswith('?') else []
                    n = (numbers[0] if numbers else 0) or 1
                    if command in 'Hf':
                        self.row = n-1
                        self.col = ((numbers[1] if len(numbers)>1 else 1) or 1)-1
                    elif command == 'A': self.row = max(0, self.row-n)
                    elif command == 'B': self.row += n
                    elif command == 'C': self.col += n
                    elif command == 'D': self.col = max(0, self.col-n)
                    elif command == 'G': self.col = n-1
                    elif command == 'd': self.row = n-1
                    elif command == 'J' and numbers == [2]: self.cells.clear()
                    elif command == 'm':
                        i = 0
                        while i < len(numbers):
                            code = numbers[i]
                            if code == 0: self.fg = self.bg = None
                            elif code == 39: self.fg = None
                            elif code == 49: self.bg = None
                            elif code in (38, 48) and numbers[i+1:i+2] == [2] and len(numbers) >= i+5:
                                color = tuple(numbers[i+2:i+5])
                                if code == 38: self.fg = color
                                else: self.bg = color
                                i += 4
                            i += 1
                    text = text[match.end():]
                    continue
                text = text[2:]
                continue
            end = text.find('\x1b')
            plain, text = (text, '') if end < 0 else (text[:end], text[end:])
            for ch in plain:
                if ch == '\r': self.col = 0
                elif ch == '\n': self.row += 1
                elif ch >= ' ' and not unicodedata.combining(ch):
                    if 0 <= self.row < self.rows and 0 <= self.col < self.columns:
                        self.cells[self.row, self.col] = (ch, self.fg, self.bg)
                    self.col += 2 if unicodedata.east_asian_width(ch) in 'WF' else 1
        # Native payloads can be large; they are already bounded by the application.
        self.pending = text
