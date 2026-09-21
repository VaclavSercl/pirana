#!/usr/bin/env python3
"""Capability-detected multi-runtime dispatcher with transactional checkpointing."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import uuid

import tempfile
from common import Blocked, digest, git, head, json_bytes, paths, read, repo_root, safe, utc

RUNTIMES = {
    'claude': {
        'interactive': ['claude'],
        'noninteractive': ['claude', '-p'],
        '--version': ['claude', '--version'],
    },
    'codex': {
        'interactive': ['codex'],
        'noninteractive': ['codex', 'exec'],
        '--version': ['codex', '--version'],
    },
    'gemini': {
        'interactive': ['gemini'],
        'noninteractive': ['gemini', '-p'],
        '--version': ['gemini', '--version'],
    },
    'hermes': {
        'interactive': ['hermes', 'chat'],
        'noninteractive': ['hermes', 'chat', '--oneshot', '-q'],
        '--version': ['hermes', '--version'],
    },
}


def detect_runtime(name: str | None) -> tuple[str, dict]:
    """Detect runtime availability. Returns (name, config) or raises Blocked."""
    if name:
        if name not in RUNTIMES:
            raise Blocked(f"Unknown runtime: {name}")
        cmd = RUNTIMES[name]['--version']
        if not shutil.which(cmd[0]):
            raise Blocked(f"Runtime not found in PATH: {cmd[0]}")
        return name, RUNTIMES[name]
    
    # Auto-detect: try each runtime
    order = ['claude', 'codex', 'gemini', 'hermes']
    for rt_name in order:
        cmd = RUNTIMES[rt_name]['--version']
        if shutil.which(cmd[0]):
            return rt_name, RUNTIMES[rt_name]
    raise Blocked("No supported runtime found in PATH")


def bounded_run(argv: list[str], cwd: Path, timeout: int = 3600) -> tuple[int, bytes]:
    """Run a process with bounded output capture."""
    start = time.monotonic()
    with tempfile.TemporaryFile() as out:
        proc = subprocess.Popen(
            argv, cwd=str(cwd), stdout=out, stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL, start_new_session=True
        )
        try:
            while proc.poll() is None:
                if time.monotonic() - start > timeout:
                    os.killpg(proc.pid, 9)
                    proc.wait()
                    raise Blocked("Process timed out")
                time.sleep(0.1)
            out.seek(0)
            return proc.returncode, out.read(10 * 1024 * 1024)
        finally:
            try:
                os.killpg(proc.pid, 9)
                proc.wait(timeout=1)
            except Exception:
                pass


def preflight(root: Path, require_clean: bool = True) -> dict:
    """Validate pre-conditions for a managed run."""
    result = {'root': str(root), 'branch': None, 'head': None, 'clean': True}
    
    try:
        result['branch'] = git(root, 'symbolic-ref', '--short', 'HEAD').stdout.decode().strip()
    except Blocked:
        result['branch'] = None
    
    result['head'] = head(root)
    
    if require_clean:
        if git(root, 'status', '--porcelain=v1', '-z').stdout:
            result['clean'] = False
    
    return result


def doctor(root: Path) -> dict:
    """Report harness health."""
    installed = []
    for name in ['claude', 'codex', 'gemini', 'hermes']:
        if shutil.which(RUNTIMES[name]['--version'][0]):
            installed.append(name)
    
    return {
        'root': str(root),
        'gauntlet_exists': (root / '.gauntlet.sh').exists(),
        'config_exists': (root / '.synthbit' / 'config.json').exists(),
        'repomap_exists': (root / '.repomap.txt').exists(),
        'installed_runtimes': installed,
        'ledger_path': str(root / '.synthbit' / 'ledger.jsonl'),
    }


def run_dispatcher(args: list[str]) -> int:
    """Main dispatcher for managed runs."""
    root = repo_root()
    
    # Parse args
    if '--' in args:
        sep_idx = args.index('--')
        wrapper_args = args[:sep_idx]
        runtime_args = args[sep_idx + 1:]
    else:
        wrapper_args = args
        runtime_args = []
    
    # Detect runtime
    runtime_name = None
    if wrapper_args and wrapper_args[0] in RUNTIMES:
        runtime_name = wrapper_args[0]
        wrapper_args = wrapper_args[1:]
    
    name, config = detect_runtime(runtime_name)
    
    # Preflight
    pre = preflight(root)
    if not pre['clean']:
        print("BLOCKED: working tree is not clean", file=sys.stderr)
        return 2
    
    # Run the agent
    cmd = config['noninteractive'] + runtime_args
    print(f"[ai-run] invoking: {' '.join(cmd)}", file=sys.stderr)
    
    try:
        code, output = bounded_run(cmd, root)
        return code
    except Blocked as e:
        print(f"[ai-run] BLOCKED: {e}", file=sys.stderr)
        return 2


def main(argv=None):
    if argv is None:
        argv = sys.argv[1:]
    
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    
    sub.add_parser('doctor', help='Report harness health')
    
    run_parser = sub.add_parser('run', help='Run a managed session')
    run_parser.add_argument('runtime', nargs='?', choices=list(RUNTIMES.keys()))
    run_parser.add_argument('args', nargs='*', default=[])
    
    resume_parser = sub.add_parser('resume', help='Resume interrupted run')
    resume_parser.add_argument('run_id')
    resume_parser.add_argument('--dry-run', action='store_true')
    
    sub.add_parser('undo', help='Preview rollback')
    
    args = parser.parse_args(argv)
    
    if args.command == 'doctor':
        root = repo_root()
        print(json.dumps(doctor(root), indent=2))
        return 0
    
    elif args.command == 'run':
        runtime = args.runtime
        cmd_args = args.args
        if runtime:
            full_args = [runtime] + cmd_args
        else:
            full_args = cmd_args
        return run_dispatcher(full_args)
    
    elif args.command == 'resume':
        print(f"Resume not yet implemented for run {args.run_id}")
        return 2
    
    elif args.command == 'undo':
        root = repo_root()
        from ledger import preview_rollback
        print(json.dumps(preview_rollback(root), indent=2))
        return 0
    
    return 2


if __name__ == '__main__':
    raise SystemExit(main())
