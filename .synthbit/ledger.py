#!/usr/bin/env python3
"""Authoritative common-directory ledger with append-only JSONL and safe rollback."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid

from common import Blocked, atomic, digest, git, head, json_bytes, paths, read, repo_root, safe, utc


SCHEMA_VERSION = 1
LEDGER_REFS = 'refs/synthbit/ledger'
RECOVERY_REFS = 'refs/synthbit/recovery'


def git_common_dir(root: Path) -> Path:
    """Resolve the authoritative common directory for managed state."""
    r = git(root, 'rev-parse', '--git-common-dir')
    raw = r.stdout.decode().strip()
    p = Path(raw)
    if p.is_absolute():
        return p
    # Relative to the worktree, not cwd
    p = (root / p).resolve()
    # Validate containment
    if not str(p.resolve()).startswith(str(root.resolve())) and p != root / '.git':
        # Linked worktree: common dir is outside but valid
        pass
    if not p.exists():
        raise Blocked('Common directory does not exist: ' + str(p))
    return p


def common_store(root: Path, *relative: str) -> Path:
    base = git_common_dir(root) / 'synthbit'
    base.mkdir(parents=True, exist_ok=True)
    return base.joinpath(*relative)


def ledger_path(root: Path) -> Path:
    return common_store(root, 'ledger.jsonl')


def append_event(root: Path, event: dict) -> dict:
    """Durably append an event to the authoritative ledger."""
    event = dict(event)
    event['event_id'] = event.get('event_id') or uuid.uuid4().hex
    event['schema_version'] = SCHEMA_VERSION
    event['timestamp'] = event.get('timestamp') or utc()
    record = json_bytes(event)
    store = ledger_path(root)
    with open(store, 'ab') as f:
        f.write(record)
        f.flush()
        os.fsync(f.fileno())
    # Maintain recovery ref pointing to latest event
    git(root, 'update-ref', LEDGER_REFS, event['event_id'], check=False)
    return event


def events(root: Path) -> list[dict]:
    store = ledger_path(root)
    if not store.exists():
        return []
    records = []
    for line in store.read_bytes().split(b'\n'):
        if not line:
            continue
        try:
            records.append(json.loads(line))
        except ValueError:
            # Truncated/corrupt record: stop here
            break
    return records


def latest_completed_run(root: Path) -> dict | None:
    best = None
    for ev in events(root):
        if ev.get('type') == 'checkpoint' and ev.get('status') == 'completed':
            if best is None or ev.get('timestamp', '') > best.get('timestamp', ''):
                best = ev
    return best


def preview_rollback(root: Path) -> dict:
    """Read-only preview of rollback eligibility."""
    run = latest_completed_run(root)
    if not run:
        return {'eligible': False, 'reason': 'No completed checkpoint found'}
    return {
        'eligible': True,
        'run_id': run.get('run_id'),
        'attempt': run.get('attempt'),
        'timestamp': run.get('timestamp'),
        'branch': run.get('branch'),
        'head_sha': run.get('head_sha'),
        'description': run.get('description', '')
    }


def perform_rollback(root: Path, run_id: str, hard: bool = False) -> dict:
    """Rollback a managed checkpoint using git revert --no-commit."""
    # Find the run
    run = None
    for ev in events(root):
        if ev.get('run_id') == run_id and ev.get('type') == 'checkpoint':
            run = ev
            break
    if not run:
        raise Blocked('Run not found in ledger: ' + run_id)
    
    sha = run.get('head_sha')
    if not sha:
        raise Blocked('Run has no associated commit SHA')
    
    current = head(root)
    if not current:
        raise Blocked('No current HEAD to rollback from')
    
    # Check clean state
    if git(root, 'status', '--porcelain=v1', '-z').stdout:
        raise Blocked('Working tree is not clean')
    
    try:
        if hard:
            # Hard reset - requires explicit confirmation upstream
            git(root, 'reset', '--hard', sha)
            method = 'hard-reset'
        else:
            # Revert with --no-commit so Gauntlet can verify
            git(root, 'revert', '--no-commit', sha)
            method = 'revert'
        
        # Record rollback event
        result = append_event(root, {
            'type': 'rollback',
            'target_run': run_id,
            'target_sha': sha,
            'method': method,
            'status': 'completed'
        })
        return {'status': 'completed', 'method': method, 'event_id': result['event_id']}
    except Blocked as e:
        append_event(root, {
            'type': 'rollback',
            'target_run': run_id,
            'status': 'failed',
            'reason': str(e)
        })
        raise


def create_checkpoint(root: Path, run_id: str, description: str, attempt: int = 1,
                      status: str = 'completed', **extra) -> dict:
    """Record a checkpoint event."""
    current = head(root)
    return append_event(root, {
        'type': 'checkpoint',
        'run_id': run_id,
        'attempt': attempt,
        'description': description,
        'branch': git(root, 'symbolic-ref', '--short', 'HEAD').stdout.decode().strip(),
        'head_sha': current,
        'status': status,
        **extra
    })


def doctor(root: Path) -> dict:
    store = ledger_path(root)
    count = len(events(root))
    latest = latest_completed_run(root)
    return {
        'ledger_path': str(store),
        'event_count': count,
        'latest_checkpoint': latest.get('run_id') if latest else None,
        'common_dir': str(git_common_dir(root))
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    
    sub.add_parser('doctor', help='Report ledger status')
    
    sub.add_parser('preview', help='Preview rollback eligibility')
    
    apply_parser = sub.add_parser('apply', help='Apply rollback')
    apply_parser.add_argument('--run-id', required=True)
    apply_parser.add_argument('--hard', action='store_true')
    
    list_parser = sub.add_parser('list', help='List ledger events')
    list_parser.add_argument('--limit', type=int, default=20)
    
    args = parser.parse_args(argv)
    root = repo_root()
    
    if args.command == 'doctor':
        print(json.dumps(doctor(root), indent=2))
        return 0
    elif args.command == 'preview':
        print(json.dumps(preview_rollback(root), indent=2))
        return 0 if preview_rollback(root)['eligible'] else 1
    elif args.command == 'apply':
        result = perform_rollback(root, args.run_id, args.hard)
        print(json.dumps(result, indent=2))
        return 0
    elif args.command == 'list':
        events_list = events(root)[-args.limit:]
        for ev in events_list:
            print(json.dumps(ev, ensure_ascii=True))
        return 0


if __name__ == '__main__':
    raise SystemExit(main())
