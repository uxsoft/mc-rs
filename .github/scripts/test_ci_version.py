import tempfile
import tomllib
import unittest
from pathlib import Path

from set_ci_version import stamp


class VersionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'mc').mkdir()
        for name in ['LICENSE', 'mc/LICENSE']:
            (self.root / name).write_text('same license')
        self.manifest = self.root / 'mc/Cargo.toml'
        self.lock = self.root / 'mc/Cargo.lock'
        self.manifest.write_text('[package]\nname = "mc-rs"\nversion = "0.1.0"\n\n[dependencies]\nother = "0.1.0"\n')
        self.lock.write_text('version = 4\n\n[[package]]\nname = "mc-rs"\nversion = "0.1.0"\n\n[[package]]\nname = "other"\nversion = "0.1.0"\n')

    def test_stamps_both_files_without_changing_dependencies(self):
        self.assertEqual(stamp(self.root, '42'), '0.1.42')
        manifest = tomllib.loads(self.manifest.read_text())
        lock = tomllib.loads(self.lock.read_text())
        self.assertEqual(manifest['package']['version'], '0.1.42')
        self.assertEqual(manifest['dependencies']['other'], '0.1.0')
        self.assertEqual([p['version'] for p in lock['package']], ['0.1.42', '0.1.0'])
        self.assertEqual(stamp(self.root, '42'), '0.1.42')  # A rerun stays deterministic.

    def test_preserves_major_minor(self):
        self.manifest.write_text(self.manifest.read_text().replace('0.1.0', '2.7.9'))
        self.lock.write_text(self.lock.read_text().replace('0.1.0', '2.7.9'))
        self.assertEqual(stamp(self.root, '123'), '2.7.123')

    def test_rejects_invalid_run_numbers_without_writes(self):
        before = self.manifest.read_bytes(), self.lock.read_bytes()
        for number in ['', '0', '-1', '01', '1.2', '1; echo unsafe', ' 42']:
            with self.subTest(number=number), self.assertRaises(ValueError):
                stamp(self.root, number)
            self.assertEqual(before, (self.manifest.read_bytes(), self.lock.read_bytes()))

    def test_rejects_unsynchronized_lock_without_partial_write(self):
        self.lock.write_text(self.lock.read_text().replace('name = "mc-rs"', 'name = "wrong"'))
        before = self.manifest.read_bytes()
        with self.assertRaises(ValueError):
            stamp(self.root, '42')
        self.assertEqual(before, self.manifest.read_bytes())

    def test_rejects_license_drift(self):
        (self.root / 'mc/LICENSE').write_text('different')
        with self.assertRaises(ValueError):
            stamp(self.root, '42')


if __name__ == '__main__':
    unittest.main()
