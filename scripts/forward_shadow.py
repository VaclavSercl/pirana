#!/usr/bin/env python3
"""Forward quote-observation journal. Read-only toward Pirana and the exchange.

Records local API snapshots from NOW, with receipt and monotonic timestamps.
These are sampled observations, not exchange fills or a complete HFT replay.
No observation alone qualifies as a resolved shadow trade for promotion.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import time
import urllib.request


def fingerprint(paths):
    digest = hashlib.sha256()
    for path in sorted(map(Path, paths), key=str):
        digest.update(str(path).encode() + b"\0")
        digest.update(path.read_bytes())
    return digest.hexdigest()


def observation(snapshot, received_at, monotonic_ns, revision):
    if not isinstance(snapshot, dict):
        raise ValueError("snapshot must be an object")
    book = snapshot.get("order_book", {})
    if not isinstance(book, dict):
        raise ValueError("missing order book")
    sides = {}
    for side in ("bids", "asks"):
        rows = book.get(side)
        if not isinstance(rows, list) or not rows:
            raise ValueError("missing " + side)
        levels = []
        for row in rows[:25]:
            p, q = row.get("price"), row.get("quantity")
            if any(isinstance(v, bool) or not isinstance(v, (int, float))
                   or not math.isfinite(v) or v <= 0 for v in (p, q)):
                raise ValueError("invalid book level")
            levels.append({"price": p, "quantity": q})
        sides[side] = levels
    if max(r["price"] for r in sides["bids"]) >= min(r["price"] for r in sides["asks"]):
        raise ValueError("crossed or locked book")
    # Do not copy arbitrary API fields into evidence (including possible secrets).
    return {
        "schema_version": 1, "kind": "quote_observation",
        "received_at": received_at, "monotonic_ns": monotonic_ns,
        "source_revision": revision, "source": "local_pirana_snapshot",
        "exchange_quote_timestamp": None, "promotion_eligible": False,
        "limitation": "sampled book; exchange freshness and actual fills unverified",
        "book": sides,
        "uptime_seconds": snapshot.get("uptime_seconds"),
        "system_mode": snapshot.get("system_mode"),
        "signals": [{k: s.get(k) for k in ("id", "timestamp", "signal_type", "executed")}
                    for s in snapshot.get("recent_signals", [])[:30] if isinstance(s, dict)],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--interval", type=float, default=1.0)
    parser.add_argument("--count", type=int, default=0, help="0 = continuous")
    parser.add_argument("--max-file-mb", type=int, default=32)
    parser.add_argument("--revision-file", type=Path, action="append", required=True)
    args = parser.parse_args()
    if not 0.25 <= args.interval <= 60 or args.count < 0 or args.max_file_mb < 1:
        parser.error("invalid interval/count/size")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    revision = fingerprint(args.revision_file)
    started = time.time_ns()
    index = 0
    with (args.output_dir / "collector.lock").open("a") as lock:
        import fcntl
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        while not args.count or index < args.count:
            tick_start = time.monotonic()
            try:
                with urllib.request.urlopen("http://127.0.0.1:8080/api/snapshot", timeout=3) as r:
                    snapshot = json.load(r)
                row = observation(snapshot, time.time(), time.monotonic_ns(), revision)
            except Exception as exc:
                row = {"schema_version": 1, "kind": "observation_gap", "received_at": time.time(),
                       "source_revision": revision, "promotion_eligible": False,
                       "error_type": type(exc).__name__}
            path = args.output_dir / f"observations-{started}.jsonl"
            if path.exists() and path.stat().st_size >= args.max_file_mb * 1024 * 1024:
                started = time.time_ns()
                path = args.output_dir / f"observations-{started}.jsonl"
            # Never overwrite or rotate away evidence. Use bounded files and stop if
            # the run directory exceeds 512 MiB; operator can archive and resume.
            if index % 60 == 0 and sum(p.stat().st_size for p in args.output_dir.glob("*.jsonl")) > 512 * 1024 * 1024:
                raise RuntimeError("evidence ceiling reached; archive this run before resuming")
            with path.open("a") as out:
                out.write(json.dumps(row, allow_nan=False) + "\n")
                out.flush()
            index += 1
            if not args.count or index < args.count:
                time.sleep(max(0.0, args.interval - (time.monotonic() - tick_start)))


if __name__ == "__main__":
    main()
