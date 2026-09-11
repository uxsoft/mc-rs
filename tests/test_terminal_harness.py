import unittest
from unittest.mock import patch
from terminal_harness import Terminal, Screen


class HarnessTests(unittest.TestCase):
    def test_queries_split_at_every_byte_are_answered_once(self):
        terminal = Terminal.__new__(Terminal)
        terminal.output, terminal.pending = bytearray(), b""
        terminal.kitty, terminal.queried, terminal.fd = False, False, -1
        terminal.screen = Screen()
        with patch('terminal_harness.os.write') as write:
            for byte in b'\x1b[6n\x1b[5n':
                terminal.feed(bytes([byte]))
            self.assertEqual([call.args[1] for call in write.call_args_list], [b'\x1b[1;1R', b'\x1b[0n'])
        terminal.feed(b'x' * (5 * 1024 * 1024))
        self.assertLessEqual(len(terminal.output), 4 * 1024 * 1024)
        self.assertLessEqual(len(terminal.screen.cells), 30 * 110)

    def test_screen_tracks_viewport_pixels_and_discards_native_payload(self):
        screen = Screen()
        for byte in b'\x1b[3;4H\x1b[48;2;233;47;71m ':
            screen.feed(bytes([byte]))
        self.assertEqual(screen.cells[2, 3], (' ', None, (233, 47, 71)))
        screen.feed(b'\x1b_G' + b'x' * 10000)
        self.assertLessEqual(len(screen.pending), 1)
        screen.feed(b'\x1b')
        screen.feed(b'\\\x1b[2J')
        self.assertFalse(screen.cells)


if __name__ == '__main__':
    unittest.main()
