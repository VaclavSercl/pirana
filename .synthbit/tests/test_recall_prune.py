#!/usr/bin/env python3
"""Tests for recall, prune, ledger, dispatcher, install."""
import ast
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import recall
import prune_memory
import ledger
import install


class RecallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='recall space ž ')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        (self.root / 'AGENTS.md').write_text('# Test\n<!-- synthbit:sessions:start -->\n<!-- synthbit:sessions:end -->\n', encoding='utf-8')

    def test_empty_query(self):
        result = recall.search(self.root, '', 'fallback')
        self.assertEqual(result['status'], 'EMPTY_QUERY')

    def test_no_match_fallback(self):
        result = recall.search(self.root, 'nonexistent_token_xyz', 'fallback')
        self.assertEqual(result['status'], 'NO_MATCH')

    def test_seed_and_search(self):
        archive = self.root / '.synthbit' / 'archive'
        archive.mkdir(parents=True, exist_ok=True)
        (archive / 'memory_202609.md').write_text(
            '- [2026-09-19T10:14:00Z] [run:test-session] [goal:Deploy harness] [changes:PLAN.md,gate.py] [decision:ok]\n'
            '- [2026-09-20T11:00:00Z] [run:second] [goal:Update config] [changes:config.json]\n',
            encoding='utf-8'
        )
        result = recall.search(self.root, 'deploy harness', 'fallback')
        self.assertEqual(result['status'], 'MATCH')
        self.assertGreaterEqual(len(result['results']), 1)

    def test_fts_unavailable_fails_closed(self):
        # Simulate FTS unavailable by monkeypatching
        orig = recall._fts_available
        recall._fts_available = lambda: False
        try:
            result = recall.search(self.root, 'query', 'auto')
            # Should fall back to fallback
            self.assertEqual(result['backend'], 'fallback')
        finally:
            recall._fts_available = orig

    def test_reindex(self):
        archive = self.root / '.synthbit' / 'archive'
        archive.mkdir(parents=True, exist_ok=True)
        (archive / 'memory_202609.md').write_text(
            '- [2026-09-19T10:14:00Z] [run:s1] [goal:Test reindex]\n',
            encoding='utf-8'
        )
        count = recall.build_index(self.root)
        self.assertGreater(count, 0)

    def test_recalled_file_written(self):
        archive = self.root / '.synthbit' / 'archive'
        archive.mkdir(parents=True, exist_ok=True)
        (archive / 'memory_202609.md').write_text(
            '- [2026-09-19T10:14:00Z] [run:s1] [goal:Write recalled context test]\n',
            encoding='utf-8'
        )
        result = recall.search(self.root, 'recalled context', 'fallback')
        self.assertEqual(result['status'], 'MATCH')
        # Check recalled file exists
        recalled = self.root / '.synthbit' / '.recalled_context'
        meta = self.root / '.synthbit' / '.recalled_context.meta.json'
        self.assertTrue(recalled.exists())
        self.assertTrue(meta.exists())
        m = json.loads(meta.read_text())
        self.assertEqual(m['status'], 'MATCH')

    def test_stale_invalidation(self):
        """Searching twice invalidates the first generation."""
        archive = self.root / '.synthbit' / 'archive'
        archive.mkdir(parents=True, exist_ok=True)
        (archive / 'memory_202609.md').write_text(
            '- [2026-09-19T10:14:00Z] [run:s1] [goal:first]\n',
            encoding='utf-8'
        )
        r1 = recall.search(self.root, 'first', 'fallback')
        r2 = recall.search(self.root, 'nonexistent_xyz', 'fallback')
        # After failed search, recalled should be invalidated
        meta = self.root / '.synthbit' / '.recalled_context.meta.json'
        m = json.loads(meta.read_text())
        self.assertIn(m['status'], ('EMPTY_QUERY', 'NO_MATCH', 'FAILED', 'INVALIDATED'))

    def test_fts_backend_when_available(self):
        if not recall._fts_available():
            self.skipTest('FTS5 not available')
        archive = self.root / '.synthbit' / 'archive'
        archive.mkdir(parents=True, exist_ok=True)
        (archive / 'memory_202609.md').write_text(
            '- [2026-09-19T10:14:00Z] [run:fts-test] [goal:FTS search test]\n',
            encoding='utf-8'
        )
        # Build index first
        recall.build_index(self.root)
        result = recall.search(self.root, 'FTS search', 'fts5')
        self.assertEqual(result['status'], 'MATCH')


class PruneTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='prune space ž ')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)

    def test_no_pruning_needed(self):
        agents = '# Test\n<!-- synthbit:sessions:start -->\n- [2026-09-19T10:14:00Z] [run:s1] [goal:a]\n<!-- synthbit:sessions:end -->\n'
        result = prune_memory.prune(agents)
        self.assertIsNone(result)

    def test_prune_archives_older(self):
        lines = ['# Test', '<!-- synthbit:sessions:start -->']
        for i in range(8):
            lines.append(f'- [2026-09-{19+i:02d}T10:14:00Z] [run:s{i}] [goal:test{i}]')
        lines.append('<!-- synthbit:sessions:end -->')
        agents = '\n'.join(lines) + '\n'
        result = prune_memory.prune(agents)
        self.assertIsNotNone(result)
        retained, archived, _, _ = result
        self.assertEqual(len(retained), 5)
        self.assertEqual(len(archived), 3)

    def test_archive_write_and_read(self):
        entries = [
            {'timestamp': '2026-09-19T10:14:00Z', 'session_id': 'old1', 'yyyymm': '202609',
             'raw': '- [2026-09-19T10:14:00Z] [run:old1] [goal:archived]', 'fields': {}},
            {'timestamp': '2026-09-19T11:00:00Z', 'session_id': 'old2', 'yyyymm': '202609',
             'raw': '- [2026-09-19T11:00:00Z] [run:old2] [goal:archived2]', 'fields': {}},
        ]
        count = prune_memory.write_archive(self.root, entries)
        self.assertEqual(count, 2)
        archive_file = self.root / '.synthbit' / 'archive' / 'memory_202609.md'
        self.assertTrue(archive_file.exists())
        content = archive_file.read_text()
        self.assertIn('old1', content)
        self.assertIn('old2', content)

    def test_dedup_preserves_last(self):
        entries = [
            {'timestamp': '2026-09-19T10:00:00Z', 'session_id': 'dup', 'yyyymm': '202609',
             'raw': 'first', 'fields': {}},
            {'timestamp': '2026-09-19T11:00:00Z', 'session_id': 'dup', 'yyyymm': '202609',
             'raw': 'second', 'fields': {}},
        ]
        result = prune_memory.dedup(entries)
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0]['raw'], 'second')


class LedgerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='ledger space ž ')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        subprocess.run(['git', 'config', 'user.name', 'Test'], check=True, cwd=self.root)
        subprocess.run(['git', 'config', 'user.email', 'test@example.invalid'], check=True, cwd=self.root)
        (self.root / 'initial.txt').write_text('init\n')
        subprocess.run(['git', 'add', '.'], check=True, cwd=self.root)
        subprocess.run(['git', 'commit', '-qm', 'init'], check=True, cwd=self.root)

    def test_doctor(self):
        info = ledger.doctor(self.root)
        self.assertIn('ledger_path', info)
        self.assertEqual(info['event_count'], 0)

    def test_append_and_read(self):
        ledger.append_event(self.root, {'type': 'test', 'value': 1})
        events = ledger.events(self.root)
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]['type'], 'test')

    def test_checkpoint_and_preview(self):
        ledger.create_checkpoint(self.root, 'run-1', 'test checkpoint')
        run = ledger.latest_completed_run(self.root)
        self.assertEqual(run['run_id'], 'run-1')
        preview = ledger.preview_rollback(self.root)
        self.assertTrue(preview['eligible'])

    def test_events_ordered(self):
        ledger.append_event(self.root, {'type': 'a', 'seq': 1})
        ledger.append_event(self.root, {'type': 'b', 'seq': 2})
        events = ledger.events(self.root)
        self.assertEqual(events[0]['seq'], 1)
        self.assertEqual(events[1]['seq'], 2)


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='install space ž ')
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)
        self.repo = self.home / 'repo'
        self.repo.mkdir()
        (self.repo / '.synthbit' / 'bin').mkdir(parents=True)
        (self.repo / '.synthbit' / 'bin' / 'ai-run').write_text('#!/bin/sh\necho ai-run\n')

    def test_find_bashrc_creates(self):
        bashrc = self.home / '.bashrc'
        self.assertFalse(bashrc.exists())
        result = install.find_bashrc(self.home)
        # Returns default .bashrc path even if not exists
        self.assertEqual(result, bashrc)

    def test_install_block_idempotent(self):
        bashrc = self.home / '.bashrc'
        bashrc.write_text('# existing\n')
        result1 = install.install_block(bashrc, install.BASHRC_SNIPPET)
        self.assertTrue(result1)
        result2 = install.install_block(bashrc, install.BASHRC_SNIPPET)
        self.assertFalse(result2)

    def test_remove_block(self):
        bashrc = self.home / '.bashrc'
        install.install_block(bashrc, install.BASHRC_SNIPPET)
        self.assertIn(install.BLOCK_START, bashrc.read_text())
        result = install.remove_block(bashrc)
        self.assertTrue(result)
        self.assertNotIn(install.BLOCK_START, bashrc.read_text())

    def test_dry_run_no_changes(self):
        bashrc = self.home / '.bashrc'
        bashrc.write_text('# existing\n')
        # dry_run_install should not modify any files
        result = install.dry_run_install(self.repo)
        self.assertTrue(result['dry_run'])
        # bashrc should be unchanged
        self.assertEqual(bashrc.read_text(), '# existing\n')


class IntegrationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='integration space ž ')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        subprocess.run(['git', 'config', 'user.name', 'Test'], check=True, cwd=self.root)
        subprocess.run(['git', 'config', 'user.email', 'test@example.invalid'], check=True, cwd=self.root)
        # Copy synthbit into this test repo
        sb = self.root / '.synthbit'
        sb.mkdir(parents=True, exist_ok=True)
        src = Path(__file__).resolve().parents[1]
        for f in src.glob('*.py'):
            shutil.copy2(f, sb / f.name)
        for f in src.glob('*.sh'):
            shutil.copy2(f, sb / f.name)

    def test_syntax_of_all_components(self):
        sb = self.root / '.synthbit'
        for f in sb.glob('*.py'):
            data = f.read_bytes()
            ast.parse(data, filename=f.name)

    def test_recall_search_with_archive(self):
        archive = self.root / '.synthbit' / 'archive'
        archive.mkdir(parents=True, exist_ok=True)
        (archive / 'memory_202609.md').write_text(
            '- [2026-09-19T10:14:00Z] [run:integration] [goal:Integration test for full pipeline]\n',
            encoding='utf-8'
        )
        result = recall.search(self.root, 'integration pipeline', 'fallback')
        self.assertEqual(result['status'], 'MATCH')


if __name__ == '__main__':
    unittest.main()
