"""Read-only verification with fail-closed candidate and policy attestation."""
from __future__ import annotations
import argparse
import ast
import fnmatch
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from collections import Counter

from common import Blocked, atomic, changed, digest, fingerprint, git, head, json_bytes, paths, read, repo_root, safe, utc

MAX_OUTPUT = 1024 * 1024
PHASES = ('static', 'tests')


def result(name, status, reason, command=None, exit_status=None, duration=0.0):
    return {'name': name, 'status': status, 'reason': reason,
            'command': command or [], 'exit_status': exit_status,
            'duration_seconds': round(duration, 6)}


def validate_config(config):
    allowed = {'schema_version', 'commands', 'allowed_paths', 'dependency_changes',
               'permission_changes', 'network_audit', 'audit_commands', 'max_file_bytes',
               'runtime', 'limits', 'recall', 'publication', 'docker', 'approved_scope'}
    if not isinstance(config, dict) or set(config) - allowed or config.get('schema_version') != 1:
        raise Blocked('Invalid policy schema or unknown policy keys')
    for key in ('allowed_paths', 'dependency_changes', 'permission_changes'):
        if not isinstance(config.get(key), list) or any(not isinstance(x, str) or '..' in Path(x).parts or x.startswith('/') for x in config[key]):
            raise Blocked('Invalid policy path list: ' + key)
    maximum = config.get('max_file_bytes')
    if type(maximum) is not int or not 1024 <= maximum <= 16 * 1024 * 1024:
        raise Blocked('Invalid file read bound')
    if type(config.get('network_audit')) is not bool:
        raise Blocked('Network policy must be explicit boolean')
    for category in ('commands', 'audit_commands'):
        if not isinstance(config.get(category), list):
            raise Blocked('Commands must be arrays')
        for c in config[category]:
            if not isinstance(c, dict) or set(c) - {'name', 'phase', 'argv', 'cwd', 'timeout', 'required', 'format'}:
                raise Blocked('Invalid command schema')
            argv = c.get('argv')
            if not isinstance(argv, list) or not argv or any(not isinstance(a, str) or '\0' in a for a in argv):
                raise Blocked('Commands require nonempty argv arrays')
            cwd = c.get('cwd')
            if not isinstance(cwd, str) or Path(cwd).is_absolute() or '..' in Path(cwd).parts:
                raise Blocked('Command working directory escapes root')
            if type(c.get('required')) is not bool or type(c.get('timeout')) not in (int, float) or not 0 < c['timeout'] <= 3600:
                raise Blocked('Invalid command requirement or timeout')
            if not isinstance(c.get('name'), str) or not c['name']:
                raise Blocked('Command name required')
            if category == 'commands' and c.get('phase') not in PHASES:
                raise Blocked('Unknown verification phase')
            if category == 'audit_commands' and c.get('format') not in ('npm', 'pip', 'cargo'):
                raise Blocked('Unsupported structured auditor')
    return config


def bounded_process(argv, cwd, timeout):
    """Limit diagnostic storage; stop and wait for the entire process group."""
    start = time.monotonic()
    with tempfile.TemporaryFile() as out:
        proc = subprocess.Popen(argv, cwd=cwd, stdout=out, stderr=subprocess.STDOUT,
                                stdin=subprocess.DEVNULL, start_new_session=True)
        failure = None
        try:
            while proc.poll() is None:
                if time.monotonic() - start > timeout:
                    failure = 'Command timed out'
                    break
                if os.fstat(out.fileno()).st_size > MAX_OUTPUT:
                    failure = 'Command output exceeded bound'
                    break
                time.sleep(0.02)
        finally:
            # Also stop descendants that outlive their foreground verification command.
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                proc.wait(timeout=1)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
        if failure:
            raise Blocked(failure)
        if os.fstat(out.fileno()).st_size > MAX_OUTPUT:
            raise Blocked('Command output exceeded bound')
        out.seek(0)
        return proc.returncode, out.read(MAX_OUTPUT), time.monotonic() - start


def run_command(root, command, audit=False):
    argv = [sys.executable if a == '{python}' else a for a in command['argv']]
    executable = argv[0]
    start = time.monotonic()
    try:
        cwd = safe(root, command['cwd'])
        if '/' in executable:
            candidate = Path(executable)
            if not candidate.is_absolute():
                candidate = safe(cwd, executable)
            exists = candidate.is_file() and os.access(candidate, os.X_OK)
        else:
            exists = shutil.which(executable) is not None
        if not exists:
            return result(command['name'], 'BLOCKED' if command['required'] else 'SKIP', 'Executable unavailable', argv)
        code, output, elapsed = bounded_process(argv, cwd, command['timeout'])
        if audit:
            try:
                normalized = normalize_audit(command['format'], json.loads(output.decode('utf-8')), code)
            except (ValueError, UnicodeError):
                normalized = {'status': 'BLOCKED', 'reason': 'Auditor did not provide valid bounded JSON'}
            if normalized['status'] == 'BLOCKED' and not command['required']:
                normalized['status'] = 'SKIP'
            return result(command['name'], normalized['status'], normalized['reason'], argv, code, elapsed)
        return result(command['name'], 'PASS' if code == 0 else 'FAIL',
                      'Command completed' if code == 0 else 'Verification command failed; raw output withheld', argv, code, elapsed)
    except (OSError, Blocked) as exc:
        return result(command['name'], 'BLOCKED' if command['required'] else 'SKIP',
                      'Execution unavailable: ' + type(exc).__name__, argv, None, time.monotonic() - start)


def normalize_audit(kind, data, code):
    """Never infer severity from exit status or advisory count."""
    if not isinstance(data, dict) or data.get('error'):
        return {'status': 'BLOCKED', 'reason': 'Audit infrastructure or schema error'}
    findings = []
    if kind == 'npm' and isinstance(data.get('vulnerabilities'), dict):
        for item in data['vulnerabilities'].values():
            if not isinstance(item, dict):
                return {'status': 'BLOCKED', 'reason': 'Invalid npm finding'}
            findings.append(str(item.get('severity', 'UNKNOWN')).upper() if item.get('nodes') else 'UNKNOWN')
    elif kind == 'pip' and isinstance(data.get('dependencies'), list):
        for dep in data['dependencies']:
            for item in dep.get('vulns', []):
                findings.append(str(item.get('severity', 'UNKNOWN')).upper())
    elif kind == 'cargo' and isinstance(data.get('vulnerabilities'), dict) and isinstance(data['vulnerabilities'].get('list'), list):
        for item in data['vulnerabilities']['list']:
            # RustSec CVSS vector cannot be equated with a severity label.
            findings.append(str(item.get('advisory', {}).get('severity', 'UNKNOWN')).upper())
    else:
        return {'status': 'BLOCKED', 'reason': 'Unsupported audit JSON schema'}
    if code not in (0, 1) or (code and not findings):
        return {'status': 'BLOCKED', 'reason': 'Auditor failed without interpretable findings'}
    counts = dict(Counter(findings))
    if 'CRITICAL' in findings:
        return {'status': 'FAIL', 'reason': 'Confirmed critical resolved dependency finding; counts=' + json.dumps(counts, sort_keys=True)}
    if any(s not in ('LOW', 'MODERATE', 'MEDIUM', 'HIGH', 'INFO') for s in findings):
        return {'status': 'BLOCKED', 'reason': 'Finding severity UNKNOWN; counts=' + json.dumps(counts, sort_keys=True)}
    return {'status': 'PASS', 'reason': 'No confirmed critical findings; lower-severity counts=' + json.dumps(counts, sort_keys=True)}


TOKEN = re.compile(r'(?:AKIA|ASIA)[A-Z0-9]{16}|(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{20,}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,}|-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----')
ASSIGNMENT = re.compile(r'(?i)(?:token|secret|password|api[_-]?key)\s*[:=]\s*[\"\x27]([A-Za-z0-9+/=_-]{20,})[\"\x27]')
CONFLICT = re.compile(r'^(?:<{7}(?: |$)|={7}$|>{7}(?: |$))', re.M)


def secret_findings(text):
    if TOKEN.search(text):
        return ['Recognizable credential/private-key format']
    for match in ASSIGNMENT.finditer(text):
        value = match.group(1)
        counts = Counter(value)
        entropy = -sum((n / len(value)) * math.log2(n / len(value)) for n in counts.values())
        if entropy >= 3.5:
            return ['Suspicious high-entropy credential assignment; review required']
    return []


def matches(name, patterns):
    return any(fnmatch.fnmatchcase(name, pattern) for pattern in patterns)


def blob(root, oid, maximum):
    size = int(git(root, 'cat-file', '-s', oid).stdout)
    if size > maximum:
        raise Blocked('Oversized Git candidate object')
    return git(root, 'cat-file', 'blob', oid).stdout


def candidate_versions(root, baseline, maximum):
    names = set(changed(root, baseline))
    indexed = {}
    for entry in git(root, 'ls-files', '--stage', '-z').stdout.split(b'\0'):
        if not entry:
            continue
        info, raw_name = entry.split(b'\t', 1)
        mode, oid, stage = info.split()
        name = os.fsdecode(raw_name)
        indexed.setdefault(name, []).append((mode.decode(), oid.decode(), stage))
    # git diff baseline includes final worktree but can hide staged-only changes.
    names.update(os.fsdecode(x) for x in git(root, 'diff', '--cached', '--name-only', '-z', *([baseline] if baseline else []), '--').stdout.split(b'\0') if x)
    commits = []
    current = head(root)
    if baseline and current and current != baseline:
        commits = git(root, 'rev-list', baseline + '..' + current).stdout.decode().splitlines()
        if len(commits) > 1000:
            raise Blocked('Run commit scan limit exceeded')
    historical = []
    for commit in commits:
        for raw_name in git(root, 'diff-tree', '--root', '--no-commit-id', '--name-only', '-r', '-z', commit).stdout.split(b'\0'):
            if not raw_name:
                continue
            name = os.fsdecode(raw_name)
            tree = git(root, 'ls-tree', '-z', commit, '--', name).stdout
            if tree:
                info, _ = tree.split(b'\t', 1)
                mode, kind, oid = info.split()
                if kind == b'blob':
                    historical.append((name, 'commit', mode.decode(), oid.decode()))
                else:
                    historical.append((name, 'unsupported', mode.decode(), ''))
    for name in sorted(names):
        p = root / name
        try:
            safe(root, name)
            if p.exists():
                yield name, 'worktree', '100755' if p.stat().st_mode & 0o111 else '100644', read(p, maximum)
        except (Blocked, OSError):
            yield name, 'unsupported', '', None
        for mode, oid, stage in indexed.get(name, []):
            if mode not in ('100644', '100755') or stage != b'0':
                yield name, 'unsupported', mode, None
            else:
                try:
                    yield name, 'index', mode, blob(root, oid, maximum)
                except Blocked:
                    yield name, 'unsupported', mode, None
    for name, source, mode, oid in historical:
        try:
            data = blob(root, oid, maximum) if source != 'unsupported' and mode in ('100644', '100755') else None
        except Blocked:
            data = None
        yield name, source, mode, data


def scan_candidates(root, baseline, config):
    checks = []
    seen = set()
    for name, source, mode, data in candidate_versions(root, baseline, config['max_file_bytes']):
        label = json.dumps(name, ensure_ascii=True) + ' [' + source + ']'
        if data is None:
            checks.append(result(label, 'BLOCKED', 'Symlink, unresolved index, oversized or unsupported candidate'))
            continue
        key = (name, digest(data))
        if key in seen:
            continue
        seen.add(key)
        if not matches(name, config['allowed_paths']):
            checks.append(result(label, 'FAIL', 'Candidate outside approved path scope'))
        base = Path(name).name.lower()
        if base == '.env' or base.startswith('.env.') or base in ('id_rsa', 'id_ed25519', 'credentials.json') or base.endswith(('.pem', '.key', '.p12', '.pfx')):
            checks.append(result(label, 'FAIL', 'Sensitive filename requires separate explicit review'))
        if base.endswith('.lock') or base in ('package-lock.json', 'yarn.lock', 'pnpm-lock.yaml', 'cargo.toml', 'package.json', 'pyproject.toml', 'requirements.txt', 'go.mod', 'go.sum'):
            if not matches(name, config['dependency_changes']):
                checks.append(result(label, 'FAIL', 'Dependency/lockfile change outside approved scope'))
        try:
            if b'\0' in data:
                raise UnicodeError()
            text = data.decode('utf-8')
        except UnicodeError:
            checks.append(result(label, 'BLOCKED', 'Binary/non-UTF8 candidate needs explicit scanner policy'))
            continue
        for finding in secret_findings(text):
            checks.append(result(label, 'FAIL', finding))
        if CONFLICT.search(text):
            checks.append(result(label, 'FAIL', 'Conflict marker detected'))
    if not checks:
        checks.append(result('candidate-security', 'PASS', 'All candidate versions inspected; no findings'))
    return checks


def syntax_checks(root):
    checks = []
    for name in paths(root):
        p = root / name
        if not p.exists() or p.suffix not in ('.py', '.sh'):
            continue
        try:
            safe(root, name)
            content = read(p)
            if p.suffix == '.py':
                ast.parse(content, filename=name)
                checks.append(result('syntax:' + json.dumps(name), 'PASS', 'Python AST parsed'))
            else:
                checks.append(run_command(root, {'name': 'syntax:' + json.dumps(name), 'argv': ['bash', '-n', '--', name], 'cwd': '.', 'timeout': 10, 'required': True}))
        except SyntaxError:
            checks.append(result('syntax:' + json.dumps(name), 'FAIL', 'Invalid Python syntax'))
        except (OSError, Blocked, ValueError):
            checks.append(result('syntax:' + json.dumps(name), 'BLOCKED', 'Source unavailable or unsafe'))
    return checks


def run_gate(root, config=None, baseline=None):
    root = Path(root)
    checks = []
    started = time.monotonic()
    initial = None
    policy_digest = None
    baseline = baseline if baseline is not None else head(root)
    try:
        if config is None:
            config = json.loads(read(safe(root, '.synthbit/config.json')))
        validate_config(config)
        policy_digest = digest(json_bytes(config))
        initial = fingerprint(root)
        if os.environ.get('USE_DOCKER_SANDBOX') == '1':
            raise Blocked('Requested Docker isolation unavailable: no validated configured container adapter; native fallback prohibited')
        checks.extend(syntax_checks(root))
        for phase in PHASES:
            for command in config['commands']:
                if command['phase'] == phase:
                    check = run_command(root, command)
                    checks.append(check)
                    if phase == 'tests' and check['status'] == 'FAIL':
                        break
            if checks and checks[-1]['status'] == 'FAIL' and phase == 'tests':
                break
        if not config['audit_commands']:
            manifests = [p for p in paths(root) if Path(p).name in ('Cargo.toml', 'package.json', 'pyproject.toml', 'requirements.txt', 'go.mod')]
            checks.append(result('supply-chain', 'SKIP' if manifests else 'NOT_APPLICABLE',
                                 'No configured auditor; dependency assessment unavailable' if manifests else 'No external dependency manifests detected'))
        for command in config['audit_commands']:
            if not config['network_audit']:
                checks.append(result(command['name'], 'BLOCKED' if command['required'] else 'SKIP', 'Network audit not authorized'))
            else:
                checks.append(run_command(root, command, audit=True))
        checks.extend(scan_candidates(root, baseline, config))
        for args in [('diff', '--check'), ('diff', '--cached', '--check')]:
            r = git(root, *args, check=False)
            checks.append(result('diff-whitespace', 'PASS' if r.returncode == 0 else 'FAIL', 'Git whitespace/conflict check', ['git', *args], r.returncode))
        # Mode changes are independent from text security findings.
        if baseline:
            raw = git(root, 'diff', '--raw', '-z', baseline, '--').stdout.split(b'\0')
            for i in range(0, len(raw) - 1, 2):
                fields = raw[i].split()
                if len(fields) >= 2 and fields[0][1:] != fields[1] and fields[0][1:] != b'000000' and fields[1] != b'000000':
                    name = os.fsdecode(raw[i+1])
                    if not matches(name, config['permission_changes']):
                        checks.append(result('mode:' + json.dumps(name), 'FAIL', 'Unapproved permission/object type change'))
        final = fingerprint(root)
        checks.append(result('candidate-stability', 'PASS' if initial == final else 'FAIL', 'Source and index fingerprint unchanged' if initial == final else 'Candidate changed during verification'))
    except (Blocked, OSError, ValueError, TypeError) as exc:
        checks.append(result('gate-infrastructure', 'BLOCKED', 'Verification unavailable: ' + type(exc).__name__))
    code = 2 if any(c['status'] == 'BLOCKED' for c in checks) else 1 if any(c['status'] == 'FAIL' for c in checks) else 0
    report = {'schema_version': 1, 'timestamp': utc(), 'baseline': baseline,
              'candidate_fingerprint': initial, 'policy_digest': policy_digest,
              'duration_seconds': time.monotonic() - started, 'exit_code': code, 'checks': checks}
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline')
    parser.add_argument('--policy', help='Frozen policy file; must be inside repository')
    args = parser.parse_args(argv)
    try:
        root = repo_root()
        config = json.loads(read(safe(root, args.policy))) if args.policy else None
        report = run_gate(root, config, args.baseline)
        for item in report['checks']:
            print(item['status'] + ' ' + item['name'] + ': ' + item['reason'])
        print('Gauntlet exit:', report['exit_code'])
        atomic(root, '.synthbit/reports/gauntlet.json', json_bytes(report))
        return report['exit_code']
    except (Blocked, OSError, ValueError) as exc:
        print('BLOCKED Gauntlet infrastructure: ' + type(exc).__name__, file=sys.stderr)
        return 2

if __name__ == '__main__':
    raise SystemExit(main())
