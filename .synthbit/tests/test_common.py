import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import common

class CommonTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='synthbit space ž ')
        self.root = Path(self.tmp.name)
        subprocess.run(['git', 'init', '-q', '-b', 'task', str(self.root)], check=True)
    def tearDown(self):
        self.tmp.cleanup()
    def test_containment_and_symlink(self):
        with self.assertRaises(common.Blocked): common.safe(self.root, '../escape')
        (self.root/'link').symlink_to('/tmp', target_is_directory=True)
        with self.assertRaises(common.Blocked): common.atomic(self.root, 'link/escape', b'no')
    def test_atomic_mode_and_idempotence(self):
        common.atomic(self.root, 'file', b'one', 0o640)
        s=(self.root/'file').stat()
        common.atomic(self.root, 'file', b'one')
        self.assertEqual(s.st_ino, (self.root/'file').stat().st_ino)
        common.atomic(self.root, 'file', b'two')
        self.assertEqual((self.root/'file').stat().st_mode & 0o777, 0o640)
    def test_unusual_files_index_and_worktree_fingerprint(self):
        (self.root/'-odd\nž.py').write_text('x=1\n')
        first=common.fingerprint(self.root)
        common.git(self.root, 'add', '--', '-odd\nž.py')
        self.assertNotEqual(first, common.fingerprint(self.root))
        second=common.fingerprint(self.root)
        (self.root/'-odd\nž.py').write_text('x=2\n')
        self.assertNotEqual(second, common.fingerprint(self.root))
        self.assertIn('-odd\nž.py', common.paths(self.root))
    def test_nested_resolution(self):
        (self.root/'nested').mkdir()
        self.assertEqual(common.repo_root(self.root/'nested'), self.root)
        self.assertIsNone(common.head(self.root))
    def test_lock_contention_release(self):
        with common.lock(self.root, 'lock'):
            with self.assertRaises(common.Blocked):
                with common.lock(self.root, 'lock'): pass
        with common.lock(self.root, 'lock'): pass
    def test_read_bounds(self):
        (self.root/'large').write_bytes(b'x'*20)
        with self.assertRaises(common.Blocked): common.read(self.root/'large', 10)
    def test_ignore_nested(self):
        (self.root/'.gitignore').write_text('*.cache\n')
        (self.root/'a.cache').write_text('ignored')
        self.assertNotIn('a.cache', common.paths(self.root))

if __name__ == '__main__': unittest.main()

class InventoryRegressionTests(unittest.TestCase):
    setUp = CommonTests.setUp
    tearDown = CommonTests.tearDown
    def test_staged_change_restored_in_worktree_remains_candidate(self):
        p=self.root/'source.py';p.write_text('x=1\n')
        common.git(self.root, 'add', '--', 'source.py')
        common.git(self.root, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture')
        p.write_text('x=2\n');common.git(self.root,'add','--','source.py')
        p.write_text('x=1\n')
        self.assertIn('source.py',common.changed(self.root))
    def test_root_ending_newline(self):
        nested=self.root/'repo\n';nested.mkdir()
        (self.root/'repo').mkdir()
        common.git(nested,'init','-q','-b','task')
        self.assertEqual(common.repo_root(nested),nested)

class UnsafeObjectTests(unittest.TestCase):
    setUp = CommonTests.setUp
    tearDown = CommonTests.tearDown
    def test_fifo_read_does_not_block(self):
        p=self.root/'fifo';os.mkfifo(p)
        code='import common; from pathlib import Path; common.read(Path('+repr(str(p))+'))'
        try:
            r=subprocess.run([sys.executable,'-c',code],env=dict(os.environ,PYTHONPATH=str(Path(__file__).resolve().parents[1])),capture_output=True,timeout=1)
        except subprocess.TimeoutExpired:
            self.fail('FIFO read blocked')
        self.assertNotEqual(r.returncode,0)
    def test_corrupt_head_is_not_unborn(self):
        (self.root/'.git/HEAD').write_text('broken\n')
        with self.assertRaises(common.Blocked):common.head(self.root)
