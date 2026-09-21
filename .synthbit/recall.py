#!/usr/bin/env python3
"""Fresh recall with FTS5 + fallback backend."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
import re
import sqlite3
import sys
import time
import uuid
from pathlib import Path

from common import Blocked, atomic, read, repo_root, safe, utc

DB_PATH = '.synthbit/memory_index.db'
RECALLED_FILE = '.synthbit/.recalled_context'
META_FILE = '.synthbit/.recalled_context.meta.json'
ARCHIVE_DIR = '.synthbit/archive'
SCHEMA_VERSION = 1
MAX_RESULTS = 2

# FTS5 query tokenizer: escape special chars
F_ESCAPE = re.compile(r'[^\w\s\-]')


def _db_path(root: Path) -> Path:
    return safe(root, DB_PATH)


def _fts_available() -> bool:
    try:
        conn = sqlite3.connect(':memory:')
        conn.execute('CREATE VIRTUAL TABLE t USING fts5(x)')
        conn.close()
        return True
    except Exception:
        return False


def _escape_fts(query: str) -> str:
    """Escape user text into safe FTS5 MATCH tokens."""
    tokens = []
    for part in query.split():
        cleaned = F_ESCAPE.sub('', part)
        if cleaned:
            tokens.append('"' + cleaned.replace('"', '') + '"')
    return ' OR '.join(tokens) if tokens else '""'


def _open_db(root: Path) -> sqlite3.Connection:
    db_path = _db_path(root)
    conn = sqlite3.connect(str(db_path))
    conn.execute('PRAGMA journal_mode=WAL')
    conn.execute('PRAGMA foreign_keys=ON')
    return conn


def build_index(root: Path) -> int:
    """Build or update the memory index from archive files."""
    archive = safe(root, ARCHIVE_DIR)
    if not archive.exists():
        return 0
    
    conn = _open_db(root)
    conn.execute('''CREATE VIRTUAL TABLE IF NOT EXISTS memory USING fts5(
        session_id UNINDEXED,
        timestamp UNINDEXED,
        content,
        source UNINDEXED
    )''')
    
    count = 0
    for f in sorted(archive.glob('memory_*.md')):
        content = read(f).decode('utf-8', errors='replace')
        # Split into session entries
        entries = re.split(r'(?=^\s*-\s*\[\d{4}-\d{2}-\d{2}T)', content, flags=re.MULTILINE)
        for entry in entries:
            if not entry.strip():
                continue
            # Extract session ID
            m = re.search(r'\[run:([^\]]+)\]', entry)
            sid = m.group(1) if m else ''
            m = re.search(r'\[(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)\]', entry)
            ts = m.group(1) if m else ''
            if sid:
                conn.execute(
                    'INSERT INTO memory(session_id, timestamp, content, source) VALUES (?, ?, ?, ?)',
                    (sid, ts, entry, f.name)
                )
                count += 1
    
    conn.commit()
    conn.close()
    return count


def _clear_recalled(root: Path) -> None:
    """Atomically clear recalled context and invalidate prior generation."""
    out_path = safe(root, RECALLED_FILE)
    meta_path = safe(root, META_FILE)
    atomic(root, RECALLED_FILE, b'', 0o600)
    atomic(root, META_FILE, json.dumps({
        'schema_version': SCHEMA_VERSION,
        'generation': uuid.uuid4().hex,
        'status': 'INVALIDATED',
        'timestamp': utc()
    }).encode(), 0o600)


def _publish_recalled(root: Path, content: str, meta: dict) -> None:
    """Atomically publish new recalled context and valid metadata."""
    out_path = safe(root, RECALLED_FILE)
    meta_path = safe(root, META_FILE)
    atomic(root, RECALLED_FILE, content.encode('utf-8'), 0o600)
    atomic(root, META_FILE, json.dumps(meta).encode(), 0o600)


def _search_fts(conn: sqlite3.Connection, query: str, limit: int) -> list[dict]:
    """Search using FTS5."""
    escaped = _escape_fts(query)
    if not escaped:
        return []
    cur = conn.execute(
        '''SELECT session_id, timestamp, source, snippet(memory, 1, '[', ']', ' ... ', 32) as snip
           FROM memory WHERE memory MATCH ? ORDER BY rank LIMIT ?''',
        (escaped, limit)
    )
    return [{'session_id': r[0], 'timestamp': r[1], 'source': r[2], 'snippet': r[3]} for r in cur.fetchall()]


def _search_fallback(root: Path, query: str, limit: int) -> list[dict]:
    """Fallback: bounded Unicode-aware Python text matching."""
    archive = safe(root, ARCHIVE_DIR)
    if not archive.exists():
        return []
    
    query_lower = query.lower().split()
    results = []
    
    for f in sorted(archive.glob('memory_*.md')):
        content = read(f).decode('utf-8', errors='replace')
        entries = re.split(r'(?=^\s*-\s*\[\d{4}-\d{2}-\d{2}T)', content, flags=re.MULTILINE)
        for entry in entries:
            if not entry.strip():
                continue
            entry_lower = entry.lower()
            score = sum(1 for t in query_lower if t in entry_lower)
            if score > 0:
                m = re.search(r'\[run:([^\]]+)\]', entry)
                sid = m.group(1) if m else ''
                m = re.search(r'\[(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)\]', entry)
                ts = m.group(1) if m else ''
                results.append({
                    'session_id': sid,
                    'timestamp': ts,
                    'source': f.name,
                    'snippet': entry[:200],
                    'score': score
                })
    
    # Sort by score desc, then timestamp desc for deterministic tie-breaking
    results.sort(key=lambda x: (-x.get('score', 0), x.get('timestamp', '')))
    return results[:limit]


def search(root: Path, query: str, backend: str = 'auto', limit: int = MAX_RESULTS) -> dict:
    """Main search entry point with generation tracking."""
    # Invalidate previous generation
    _clear_recalled(root)
    
    if not query.strip():
        return {'status': 'EMPTY_QUERY', 'results': [], 'backend': backend}
    
    if backend == 'auto':
        backend = 'fts5' if _fts_available() else 'fallback'
    
    results = []
    if backend == 'fts5':
        try:
            conn = _open_db(root)
            results = _search_fts(conn, query, limit)
            conn.close()
        except Exception as e:
            # FTS5 failed - report unavailable, don't silently switch
            return {'status': 'FAILED', 'backend': 'fts5', 'reason': str(e)}
    elif backend == 'fallback':
        results = _search_fallback(root, query, limit)
    else:
        return {'status': 'FAILED', 'backend': backend, 'reason': f'Unknown backend: {backend}'}
    
    if not results:
        return {'status': 'NO_MATCH', 'results': [], 'backend': backend}
    
    # Publish recalled context
    content = '\n\n'.join(r.get('snippet', '') for r in results if r.get('snippet'))
    meta = {
        'schema_version': SCHEMA_VERSION,
        'generation': uuid.uuid4().hex,
        'status': 'MATCH',
        'backend': backend,
        'result_count': len(results),
        'timestamp': utc(),
        'sources': list(set(r.get('source', '') for r in results)),
        'digest': hashlib.sha256(content.encode()).hexdigest()
    }
    _publish_recalled(root, content, meta)
    
    return {'status': 'MATCH', 'results': results, 'backend': backend}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--backend', choices=['auto', 'fts5', 'fallback'], default='auto')
    parser.add_argument('--reindex', action='store_true', help='Rebuild the index')
    parser.add_argument('query', nargs='?', help='Search query')
    args = parser.parse_args(argv)
    
    root = repo_root()
    
    if args.reindex:
        count = build_index(root)
        print(f'Indexed {count} entries')
        return 0
    
    if not args.query:
        print('BLOCKED: query required', file=sys.stderr)
        return 2
    
    result = search(root, args.query, args.backend)
    print(json.dumps(result, indent=2))
    return 0 if result['status'] in ('MATCH', 'NO_MATCH', 'EMPTY_QUERY') else 1


if __name__ == '__main__':
    raise SystemExit(main())
