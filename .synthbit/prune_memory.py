#!/usr/bin/env python3
"""Transactional memory pruning for the managed SESSION LOG in AGENTS.md."""
from __future__ import annotations
import re
import sys
from pathlib import Path

from common import Blocked, atomic, lock, read, repo_root, safe

AGENTS = 'AGENTS.md'
SESSIONS_START = '<!-- synthbit:sessions:start -->'
SESSIONS_END = '<!-- synthbit:sessions:end -->'
ARCHIVE_DIR = '.synthbit/archive'
MAX_RETAINED = 5

# Matches: - [2026-09-19T10:14:00Z] [run:session-id] ...
ENTRY_RE = re.compile(
    r'^\s*-\s*\[(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)\]\s*\[run:([^\]]+)\](.*)$'
)
FIELD_RE = re.compile(r'\[(\w+):([^\]]*)\]')


def parse_section(text: str) -> list[dict]:
    """Parse all session entries from the section body.

    Handles multi-line entries: a line starting with `- [` begins a new
    entry; subsequent lines that don't start a new entry are treated as
    continuation of the current entry.
    """
    entries: list[dict] = []
    current: dict | None = None
    for line in text.splitlines():
        m = ENTRY_RE.match(line)
        if m:
            if current is not None:
                entries.append(current)
            ts, sid, rest = m.group(1), m.group(2), m.group(3)
            yyyymm = ts[:4] + ts[5:7]
            fields = dict(FIELD_RE.findall(rest))
            current = {
                'timestamp': ts,
                'session_id': sid,
                'yyyymm': yyyymm,
                'fields': fields,
                'raw': line,
            }
        elif current is not None:
            current['raw'] += '\n' + line
    if current is not None:
        entries.append(current)
    return entries


def dedup(entries: list[dict]) -> list[dict]:
    """Remove duplicate session IDs, keeping the last (newest) occurrence."""
    seen: dict[str, dict] = {}
    for e in entries:
        seen[e['session_id']] = e
    return [e for e in entries if seen.get(e['session_id']) is e]


def write_archive(root: Path, entries: list[dict]) -> int:
    """Persist entries to archive files grouped by YYYYMM.

    Crash-recoverable: reads existing archive first and skips entries
    already present (by session ID). Archives are written BEFORE the
    source section is modified, so a crash between archive and section
    update leaves no duplicates on the next run.
    """
    by_month: dict[str, list[dict]] = {}
    for e in entries:
        by_month.setdefault(e['yyyymm'], []).append(e)

    archived = 0
    for yyyymm, group in sorted(by_month.items()):
        rel = f'{ARCHIVE_DIR}/memory_{yyyymm}.md'
        p = safe(root, rel)

        existing = ''
        if p.exists():
            existing = read(p).decode()

        # Collect session IDs already in this archive
        existing_ids: set[str] = set()
        for line in existing.splitlines():
            m = ENTRY_RE.match(line)
            if m:
                existing_ids.add(m.group(2))

        new_entries = [e for e in group if e['session_id'] not in existing_ids]
        if not new_entries:
            continue

        if not existing:
            content = f'# Archived Session Log — {yyyymm}\n\n'
        else:
            content = existing.rstrip('\n') + '\n'

        for e in new_entries:
            content += e['raw'] + '\n'
            archived += 1

        atomic(root, rel, content.encode(), 0o600)

    return archived


def rebuild_section(retained: list[dict]) -> str:
    """Reindex: rebuild section body from retained entries in order."""
    return '\n'.join(e['raw'] for e in retained)


def prune(agents_text: str) -> tuple[list[dict], list[dict], int, int] | None:
    """Determine which entries to retain and which to archive.

    Returns None when no pruning is needed. Otherwise returns
    (retained, archived, section_start, section_end).
    """
    if SESSIONS_START not in agents_text or SESSIONS_END not in agents_text:
        raise Blocked('Session log markers missing in AGENTS.md')

    start = agents_text.index(SESSIONS_START) + len(SESSIONS_START)
    end = agents_text.index(SESSIONS_END)

    section_body = agents_text[start:end]
    entries = parse_section(section_body)

    if len(entries) <= MAX_RETAINED:
        return None

    entries = dedup(entries)

    # Sort chronologically, retain newest MAX_RETAINED
    entries.sort(key=lambda e: e['timestamp'])
    retained = entries[-MAX_RETAINED:]
    archived = entries[:-MAX_RETAINED]

    return retained, archived, start, end


def main() -> int:
    root = repo_root()
    agents = safe(root, AGENTS)
    text = read(agents).decode()

    result = prune(text)
    if result is None:
        return 0

    retained, archived, start, end = result

    # Crash-recoverable: persist archives BEFORE modifying AGENTS.md
    n_archived = write_archive(root, archived)

    # Reindex and rebuild the section
    new_body = rebuild_section(retained)
    new_text = text[:start] + '\n' + new_body + '\n' + text[end:]

    # Preserve AGENTS.md's current mode
    mode = agents.stat().st_mode & 0o777
    atomic(root, AGENTS, new_text.encode(), mode)

    print(f'Pruned {n_archived} entries, retained {len(retained)}', file=sys.stderr)
    return 0


if __name__ == '__main__':
    try:
        with lock(repo_root(), '.synthbit/prune.lock'):
            sys.exit(main())
    except Blocked as e:
        print(f'BLOCKED: {e}', file=sys.stderr)
        sys.exit(1)
