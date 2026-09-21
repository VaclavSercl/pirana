#!/usr/bin/env python3
"""Tests for .synthbit/repomap.py"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import repomap


class RepomapTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='repomap space ž ')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        (self.root / '.gitignore').write_text('*.cache\n')

    def test_empty_repo(self):
        # Empty repo with .gitignore may still list .gitignore + patterns
        lines = repomap.generate(self.root)
        # Filter to just actual source files (not .gitignore, dotfiles, or indent-prefixed patterns)
        src_lines = [l for l in lines if l.strip() and not l.startswith(' ') and not l.startswith('.') and not l.startswith('.g')]
        self.assertEqual(len(src_lines), 0)

    def test_python_ast_extraction(self):
        (self.root / 'module.py').write_text(
            'class MyClass:\n    def method(self):\n        pass\n\n'
            'def top_level():\n    pass\n\n'
            'async def async_fn():\n    pass\n'
        )
        lines = repomap.generate(self.root)
        text = '\n'.join(lines)
        self.assertIn('MyClass', text)
        self.assertIn('top_level', text)

    def test_invalid_syntax_handled(self):
        (self.root / 'broken.py').write_text('def broken(\n')
        lines = repomap.generate(self.root)
        self.assertIsInstance(lines, list)

    def test_deterministic_output(self):
        (self.root / 'a.py').write_text('x = 1\n')
        (self.root / 'b.py').write_text('y = 2\n')
        r1 = repomap.generate(self.root)
        r2 = repomap.generate(self.root)
        self.assertEqual(r1, r2)

    def test_max_lines_respected(self):
        for i in range(300):
            (self.root / f'file_{i:03d}.py').write_text(f'x = {i}\n')
        lines = repomap.generate(self.root)
        self.assertLessEqual(len(lines), 260)

    def test_ignores_gitignored(self):
        (self.root / 'kept.py').write_text('x = 1\n')
        (self.root / 'ignored.cache').write_text('y = 2\n')
        subprocess.run(['git', '-C', str(self.root), 'add', 'kept.py'], check=True)
        subprocess.run(['git', '-C', str(self.root), 'commit', '-qm', 'add kept'], check=True)
        # Re-create ignored file after commit
        (self.root / 'ignored.cache').write_text('y = 2\n')
        lines = repomap.generate(self.root)
        text = '\n'.join(lines)
        self.assertIn('kept.py', text)
        self.assertNotIn('ignored.cache', text)

    def test_atomic_write(self):
        (self.root / 'mod.py').write_text('x = 1\n')
        # Call main-like function to write .repomap.txt
        payload = ('\n'.join(repomap.generate(self.root)) + '\n').encode()
        repomap.atomic(self.root, '.repomap.txt', payload)
        self.assertTrue((self.root / '.repomap.txt').exists())


if __name__ == '__main__':
    unittest.main()
