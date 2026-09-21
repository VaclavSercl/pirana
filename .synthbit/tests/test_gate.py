import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import gate


class GateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='gate space ü ')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        (self.root / '.gitignore').write_text('.synthbit/reports/\n')
        self.config = {'schema_version': 1, 'commands': [], 'allowed_paths': ['*'],
                       'dependency_changes': [], 'permission_changes': [],
                       'network_audit': False, 'audit_commands': [], 'max_file_bytes': 2097152}

    def test_config_rejects_shell_and_escape(self):
        for command in [{'argv': 'echo x', 'cwd': '.', 'timeout': 2, 'required': True},
                        {'argv': ['echo'], 'cwd': '../', 'timeout': 2, 'required': True}]:
            with self.assertRaises(gate.Blocked):
                gate.validate_config(dict(self.config, commands=[command]))

    def test_required_optional_missing(self):
        cmd = {'name': 'missing', 'phase': 'tests', 'argv': ['no-such-tool-synthbit'], 'cwd': '.', 'timeout': 1, 'required': True}
        self.assertEqual(gate.run_command(self.root, cmd)['status'], 'BLOCKED')
        cmd['required'] = False
        self.assertEqual(gate.run_command(self.root, cmd)['status'], 'SKIP')

    def test_test_failure_is_failure(self):
        cmd = {'name': 'fails', 'phase': 'tests', 'argv': [sys.executable, '-c', 'raise SystemExit(3)'], 'cwd': '.', 'timeout': 2, 'required': True}
        self.assertEqual(gate.run_command(self.root, cmd)['status'], 'FAIL')

    def test_audit_findings_differ_from_error(self):
        data = {'vulnerabilities': {'x': {'severity': 'critical', 'range': '*', 'nodes': ['node_modules/x']}}}
        self.assertEqual(gate.normalize_audit('npm', data, 1)['status'], 'FAIL')
        self.assertEqual(gate.normalize_audit('npm', {'error': 'unreachable'}, 1)['status'], 'BLOCKED')
        data['vulnerabilities']['x']['severity'] = 'high'
        self.assertEqual(gate.normalize_audit('npm', data, 1)['status'], 'PASS')

    def test_secrets_no_values_in_output(self):
        value = 'AKIA' + 'A' * 16
        (self.root / 'new.py').write_text('key = "' + value + '"\n')
        results = gate.scan_candidates(self.root, None, self.config)
        self.assertTrue(any(r['status'] == 'FAIL' for r in results))
        self.assertNotIn(value, json.dumps(results))

    def test_staged_secret_even_if_worktree_cleaned(self):
        value = 'sk_live_' + 'A1b2C3' * 6
        (self.root / 'x.txt').write_text(value)
        subprocess.run(['git', '-C', str(self.root), 'add', '--', 'x.txt'], check=True)
        (self.root / 'x.txt').write_text('clean')
        self.assertTrue(any(r['status'] == 'FAIL' for r in gate.scan_candidates(self.root, None, self.config)))

    def test_symlink_and_binary_blocked(self):
        (self.root / 'link').symlink_to('/etc/passwd')
        self.assertTrue(any(r['status'] == 'BLOCKED' for r in gate.scan_candidates(self.root, None, self.config)))
        (self.root / 'link').unlink()
        (self.root / 'blob').write_bytes(b'\0binary')
        self.assertTrue(any(r['status'] == 'BLOCKED' for r in gate.scan_candidates(self.root, None, self.config)))

    def test_sensitive_filename_and_conflict(self):
        (self.root / '.env').write_text('NOT_SECRET=example')
        (self.root / 'x.txt').write_text('<' * 7 + ' ours\n')
        results = gate.scan_candidates(self.root, None, self.config)
        self.assertGreaterEqual(sum(r['status'] == 'FAIL' for r in results), 2)

    def test_success_fail_block_exit_classes(self):
        self.assertEqual(gate.run_gate(self.root, self.config)['exit_code'], 0)
        (self.root / 'bad.py').write_text('def broken(\n')
        self.assertEqual(gate.run_gate(self.root, self.config)['exit_code'], 1)
        (self.root / 'bad.py').unlink()
        self.config['commands'] = [{'name': 'required', 'phase': 'tests', 'argv': ['missing-synthbit'], 'cwd': '.', 'timeout': 1, 'required': True}]
        self.assertEqual(gate.run_gate(self.root, self.config)['exit_code'], 2)

    def test_mutation_during_gate_invalidates(self):
        self.config['commands'] = [{'name': 'mutating', 'phase': 'tests', 'argv': [sys.executable, '-c', 'from pathlib import Path; Path("new.txt").write_text("mutation")'], 'cwd': '.', 'timeout': 2, 'required': True}]
        self.assertEqual(gate.run_gate(self.root, self.config)['exit_code'], 1)

    def test_deleted_out_of_scope_is_failure(self):
        (self.root / 'outside.txt').write_text('original')
        subprocess.run(['git', '-C', str(self.root), 'add', '--', '.'], check=True)
        subprocess.run(['git', '-C', str(self.root), '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-qm', 'baseline'], check=True)
        (self.root / 'outside.txt').unlink()
        self.config['allowed_paths'] = ['allowed/*']
        self.assertEqual(gate.run_gate(self.root, self.config)['exit_code'], 1)

    def test_leading_hyphen_shell_source(self):
        (self.root / '-strange.sh').write_text('#!/usr/bin/env bash\ntrue\n')
        self.assertEqual(gate.run_gate(self.root, self.config)['exit_code'], 0)

    def test_docker_request_never_silent_fallback(self):
        old = os.environ.get('USE_DOCKER_SANDBOX')
        os.environ['USE_DOCKER_SANDBOX'] = '1'
        try:
            self.assertEqual(gate.run_gate(self.root, self.config)['exit_code'], 2)
        finally:
            if old is None: os.environ.pop('USE_DOCKER_SANDBOX', None)
            else: os.environ['USE_DOCKER_SANDBOX'] = old

if __name__ == '__main__':
    unittest.main()
