"""Bounded I/O, cooperative POSIX locks and Git-aware candidate inventory."""
from __future__ import annotations
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import time
import uuid

MAX_BYTES = 2 * 1024 * 1024

class Blocked(RuntimeError):
    """Required operation could not be established safely."""


def uid_check(path: Path) -> None:
    if path.lstat().st_uid != os.getuid():
        raise Blocked('Unexpected path owner: ' + str(path))


def safe(root: Path, relative: str | Path) -> Path:
    root = Path(root).absolute()
    p = Path(relative)
    if p.is_absolute() or '..' in p.parts:
        raise Blocked('Path must be relative and contained')
    out = root / p
    for node in [root, *root.parents]:
        if node.is_symlink():
            raise Blocked('Symlink in authorized root')
    node = root
    for part in p.parts:
        node = node / part
        if node.is_symlink():
            raise Blocked('Symlink in managed path: ' + str(p))
    return out


def read(path: Path, limit: int = MAX_BYTES) -> bytes:
    path = Path(path)
    if path.is_symlink():
        raise Blocked('Symlink read refused')
    if not stat.S_ISREG(path.lstat().st_mode):
        raise Blocked('Nonregular file refused')
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | getattr(os, 'O_NOFOLLOW', 0))
    try:
        s = os.fstat(fd)
        if not stat.S_ISREG(s.st_mode) or s.st_size > limit:
            raise Blocked('Nonregular or oversized candidate: ' + path.name)
        if not s.st_mode & 0o444:
            raise Blocked('Unreadable file: ' + path.name)
        data = os.read(fd, limit + 1)
        if len(data) > limit:
            raise Blocked('Read limit exceeded')
        return data
    finally:
        os.close(fd)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def json_bytes(obj) -> bytes:
    return (json.dumps(obj, ensure_ascii=True, sort_keys=True, separators=(',', ':')) + '\n').encode()


def sync_dir(path: Path) -> None:
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic(root: Path, relative: str | Path, data: bytes, mode: int = 0o600) -> None:
    p = safe(root, relative)
    p.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    safe(root, relative)
    if p.exists():
        uid_check(p)
        if not p.is_file():
            raise Blocked('Write target is not a regular file')
        mode = stat.S_IMODE(p.stat().st_mode)
        if read(p, max(MAX_BYTES, len(data))) == data:
            return
    fd, temp = tempfile.mkstemp(prefix='.synthbit-', dir=p.parent)
    try:
        os.fchmod(fd, mode)
        with os.fdopen(fd, 'wb') as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        safe(root, relative)
        os.replace(temp, p)
        sync_dir(p.parent)
    finally:
        if os.path.exists(temp):
            os.unlink(temp)


@contextlib.contextmanager
def lock(root: Path, relative: str):
    p = safe(root, relative)
    p.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = os.open(p, os.O_CREAT | os.O_RDWR | os.O_NONBLOCK | getattr(os, 'O_NOFOLLOW', 0), 0o600)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise Blocked('Nonregular lock refused')
        if os.fstat(fd).st_uid != os.getuid():
            raise Blocked('Lock owner mismatch')
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as e:
            raise Blocked('Another managed operation holds the lock') from e
        yield
    finally:
        os.close(fd)


def git(root: Path, *args: str, check: bool = True, timeout: int = 30, input=None):
    env = dict(os.environ, GIT_TERMINAL_PROMPT='0', GIT_LITERAL_PATHSPECS='1')
    for key in list(env):
        if key in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR'):
            env.pop(key)
    try:
        result = subprocess.run(['git', '-C', str(root), *args], input=input,
                                capture_output=True, timeout=timeout, env=env)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise Blocked('Git execution unavailable: ' + type(e).__name__) from e
    if check and result.returncode:
        raise Blocked('Git command failed: ' + args[0] + ' (exit ' + str(result.returncode) + ')')
    return result


def repo_root(start: Path | None = None) -> Path:
    start = Path(start or Path.cwd()).absolute()
    result = git(start, 'rev-parse', '--show-toplevel')
    root = Path(os.fsdecode(result.stdout.removesuffix(b'\n')))
    safe(root, '.')
    uid_check(root)
    return root


def head(root: Path) -> str | None:
    r = git(root, 'rev-parse', '--verify', 'HEAD', check=False)
    if r.returncode == 0:
        oid = r.stdout.decode().strip()
        git(root, 'cat-file', '-e', oid + '^{commit}')
        return oid
    ref = git(root, 'symbolic-ref', 'HEAD').stdout.decode().strip()
    exists = git(root, 'show-ref', '--verify', '--quiet', ref, check=False)
    if exists.returncode != 1:
        raise Blocked('HEAD cannot be verified as committed or unborn')
    return None


def branch(root: Path) -> str:
    return git(root, 'symbolic-ref', '--short', 'HEAD').stdout.decode().strip()


def paths(root: Path) -> list[str]:
    r = git(root, 'ls-files', '-z', '--cached', '--others', '--exclude-standard')
    return sorted(set(os.fsdecode(p) for p in r.stdout.split(b'\0') if p))


def manifest(root: Path) -> dict:
    files = {}
    for rel in paths(root):
        p = root / rel
        # Symlinks are represented, never followed. Parent symlinks are refused.
        safe(root, Path(rel).parent)
        try:
            s = p.lstat()
        except FileNotFoundError:
            files[rel] = {'type': 'deleted'}
            continue
        mode = stat.S_IMODE(s.st_mode)
        if stat.S_ISLNK(s.st_mode):
            files[rel] = {'type': 'symlink', 'mode': mode, 'digest': digest(os.fsencode(os.readlink(p)))}
        elif stat.S_ISREG(s.st_mode):
            files[rel] = {'type': 'file', 'mode': mode, 'digest': digest(read(p))}
        else:
            raise Blocked('Unsupported candidate object: ' + rel)
    return {'head': head(root), 'branch': branch(root), 'index': git(root, 'ls-files', '--stage', '-z').stdout.hex(),
            'untracked': [os.fsdecode(p) for p in git(root, 'ls-files', '--others', '--exclude-standard', '-z').stdout.split(b'\0') if p],
            'files': files}


def fingerprint(root: Path) -> str:
    return digest(json_bytes(manifest(root)))


def changed(root: Path, baseline: str | None = None) -> list[str]:
    baseline = baseline if baseline is not None else head(root)
    names = set(os.fsdecode(p) for p in git(root, 'ls-files', '--others', '--exclude-standard', '-z').stdout.split(b'\0') if p)
    if baseline:
        for args in [('diff', '--name-only', '-z', baseline, 'HEAD', '--'),
                     ('diff', '--cached', '--name-only', '-z', baseline, '--'),
                     ('diff', '--name-only', '-z', '--')]:
            names.update(os.fsdecode(p) for p in git(root, *args).stdout.split(b'\0') if p)
    else:
        names.update(paths(root))
    return sorted(names)


def clean(root: Path) -> bool:
    return not git(root, 'status', '--porcelain=v1', '-z', '--untracked-files=all').stdout


def git_operations(root: Path) -> None:
    for name in ['MERGE_HEAD', 'CHERRY_PICK_HEAD', 'REVERT_HEAD', 'rebase-merge', 'rebase-apply', 'index.lock']:
        raw = git(root, 'rev-parse', '--git-path', name).stdout.removesuffix(b'\n')
        p = Path(os.fsdecode(raw))
        if not p.is_absolute():
            p = root / p
        if p.exists():
            raise Blocked('Git operation in progress: ' + name)


def utc() -> str:
    return time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())


def identity() -> str:
    return uuid.uuid4().hex
