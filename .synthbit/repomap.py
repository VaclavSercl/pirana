#!/usr/bin/env python3
"""Deterministic repository map generator (Python stdlib only).

Enumerates Git-tracked and non-ignored untracked files via common.paths().
Python files: AST-parsed for classes/functions with compact signatures.
Other files: bounded heuristic extraction.
Output written atomically to .repomap.txt, deterministic order, <= 250 lines.
"""
from __future__ import annotations

import ast
import json
import re
import stat
import sys
from pathlib import Path

from common import paths, safe, atomic, read, repo_root

MAX_LINES = 250
PER_FILE_CAP = 10
HEURISTIC_SCAN = 80


def _expr(node: ast.AST) -> str:
    """Compact expression rendering for decorators, bases, annotations."""
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        return _expr(node.value) + '.' + node.attr
    if isinstance(node, ast.Call):
        return _expr(node.func) + '(...)'
    if isinstance(node, ast.Constant):
        return repr(node.value)
    if isinstance(node, ast.Subscript):
        return _expr(node.value) + '[...]'
    if isinstance(node, ast.Tuple):
        return ', '.join(_expr(e) for e in node.elts)
    if isinstance(node, ast.List):
        return '[...]'
    if isinstance(node, ast.BinOp):
        return '...'
    return '...'


def _annotation(node: ast.AST | None) -> str:
    return '' if node is None else ' -> ' + _expr(node)


def _args(node: ast.arguments) -> str:
    """Compact argument list from ast.arguments."""
    parts: list[str] = []
    args = node.args
    defaults_off = len(args) - len(node.defaults)
    for i, arg in enumerate(args):
        s = arg.arg
        if arg.annotation:
            s += ': ...'
        di = i - defaults_off
        if di >= 0 and node.defaults[di] is not None:
            s += '=...'
        parts.append(s)
    if node.vararg:
        parts.append('*' + node.vararg.arg)
    for arg in node.kwonlyargs:
        s = arg.arg
        if arg.annotation:
            s += ': ...'
        parts.append(s)
    if node.kwarg:
        parts.append('**' + node.kwarg.arg)
    return ', '.join(parts)


def _decorators(node: ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef) -> str:
    if not node.decorator_list:
        return ''
    return ' '.join('@' + _expr(d) for d in node.decorator_list) + ' '


def _sig(node: ast.FunctionDef | ast.AsyncFunctionDef) -> str:
    prefix = 'async ' if isinstance(node, ast.AsyncFunctionDef) else ''
    dec = _decorators(node)
    args = _args(node.args)
    ret = _annotation(node.returns)
    return f'{dec}{prefix}def {node.name}({args}){ret}'


def extract_python(path: Path) -> list[str]:
    """AST-based extraction of classes and functions with compact signatures."""
    try:
        source = read(path).decode('utf-8')
        tree = ast.parse(source, filename=str(path))
    except Exception:
        return ['  <unparseable>']

    results: list[str] = []
    for node in ast.iter_child_nodes(tree):
        if isinstance(node, ast.ClassDef):
            bases = ', '.join(_expr(b) for b in node.bases)
            results.append(f'  class {node.name}({bases})')
            for child in ast.iter_child_nodes(node):
                if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    results.append('    ' + _sig(child))
                    if len(results) >= PER_FILE_CAP:
                        return results[:PER_FILE_CAP]
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            results.append('  ' + _sig(node))
            if len(results) >= PER_FILE_CAP:
                return results[:PER_FILE_CAP]
    return results


_SHELL_KW = frozenset(('if', 'while', 'for', 'until', 'case', 'select'))


def _extract_sh(lines: list[str]) -> list[str]:
    results: list[str] = []
    for line in lines[:HEURISTIC_SCAN]:
        stripped = line.strip()
        if not stripped or stripped.startswith('#'):
            continue
        m = re.match(r'^(?:function\s+)?(\w+)\s*\(\)', stripped)
        if m and m.group(1) not in _SHELL_KW:
            results.append(f'  fn {m.group(1)}')
        if len(results) >= PER_FILE_CAP:
            break
    return results


def _extract_toml(lines: list[str]) -> list[str]:
    results: list[str] = []
    for line in lines[:HEURISTIC_SCAN]:
        s = line.strip()
        if not s or s.startswith('#'):
            continue
        m = re.match(r'^\[([^\]]+)\]', s)
        if m:
            results.append(f'  section [{m.group(1)}]')
            continue
        m = re.match(r'^(\w[\w-]*)[=:]', s)
        if m:
            results.append(f'  key {m.group(1)}')
        if len(results) >= PER_FILE_CAP:
            break
    return results


def _extract_md(lines: list[str]) -> list[str]:
    results: list[str] = []
    for line in lines[:HEURISTIC_SCAN]:
        m = re.match(r'^(#{1,3})\s+(.+)', line.rstrip())
        if m:
            results.append(f'  {m.group(1)} {m.group(2).strip()}')
        if len(results) >= PER_FILE_CAP:
            break
    return results


def _extract_json(text: str) -> list[str]:
    try:
        data = json.loads(text)
    except Exception:
        return ['  <invalid json>']
    results: list[str] = []
    if isinstance(data, dict):
        for k in list(data.keys())[:PER_FILE_CAP]:
            results.append(f'  key {k}')
    elif isinstance(data, list):
        results.append(f'  array[{len(data)}]')
    return results


def _extract_rs(lines: list[str]) -> list[str]:
    results: list[str] = []
    for line in lines[:HEURISTIC_SCAN]:
        m = re.match(
            r'^\s*(?:pub\s+)?(?:async\s+)?(?:unsafe\s+)?'
            r'(fn|struct|enum|trait|impl)\s+(\w+)',
            line,
        )
        if m:
            results.append(f'  {m.group(1)} {m.group(2)}')
        if len(results) >= PER_FILE_CAP:
            break
    return results


def extract_heuristic(path: Path) -> list[str]:
    """Bounded heuristic extraction for non-Python files."""
    try:
        source = read(path).decode('utf-8')
    except Exception:
        return ['  <unreadable>']

    suffix = path.suffix.lower()
    lines = source.splitlines()

    if suffix == '.sh':
        return _extract_sh(lines)
    if suffix in ('.toml', '.yaml', '.yml'):
        return _extract_toml(lines)
    if suffix == '.md':
        return _extract_md(lines)
    if suffix == '.json':
        return _extract_json(source)
    if suffix == '.rs':
        return _extract_rs(lines)

    # Generic: first meaningful content lines
    results: list[str] = []
    for line in lines[:HEURISTIC_SCAN]:
        stripped = line.strip()
        if stripped and not stripped.startswith('#') and not stripped.startswith('//'):
            results.append(f'  {stripped[:72]}')
        if len(results) >= 3:
            break
    return results


def generate(root: Path) -> list[str]:
    """Generate the repomap lines in deterministic (sorted) order."""
    file_list = paths(root)
    output: list[str] = []

    for rel in file_list:
        try:
            full = safe(root, rel)
        except Exception:
            continue

        try:
            st = full.lstat()
        except FileNotFoundError:
            continue

        if stat.S_ISLNK(st.st_mode) or not stat.S_ISREG(st.st_mode):
            continue

        entries = extract_python(full) if rel.endswith('.py') else extract_heuristic(full)
        if entries:
            output.append(rel)
            output.extend(entries)
        else:
            output.append(rel)

        if len(output) >= MAX_LINES:
            break

    return output[:MAX_LINES]


def main() -> int:
    root = repo_root()
    lines = generate(root)
    payload = ('\n'.join(lines) + '\n') if lines else '# (empty repository)\n'
    atomic(root, '.repomap.txt', payload.encode('utf-8'))
    print(f'repomap: {len(lines)} lines written to .repomap.txt')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
