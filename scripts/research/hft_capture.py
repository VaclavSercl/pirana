#!/usr/bin/env python3
"""Public Bitfinex raw-frame capture. No credentials, orders, pruning or overwrite.

Each line is independently parseable; readers MUST ignore an unterminated final
line after a crash. A session begins on every connection attempt. Full storage or
persistent queue saturation fails closed and exits nonzero, retaining all previous segments.
"""
import argparse
import asyncio
import base64
import fcntl
import json
import os
from pathlib import Path
import queue
import signal
import sys
import threading
import time
import uuid

URL = 'wss://api-pub.bitfinex.com/ws/2'
CONF_FLAGS = 65536 | 131072  # SEQ_ALL | OB_CHECKSUM


def subscriptions():
    return [{'event': 'conf', 'flags': CONF_FLAGS},
            {'event': 'subscribe', 'channel': 'book', 'symbol': 'tBTCUSD',
             'prec': 'P0', 'freq': 'F0', 'len': '25'},
            {'event': 'subscribe', 'channel': 'trades', 'symbol': 'tBTCUSD'},
            {'event': 'subscribe', 'channel': 'ticker', 'symbol': 'tBTCUSD'}]


def event(kind, session, **fields):
    return dict(schema=1, kind=kind, session=session, wall_ns=time.time_ns(),
                monotonic_ns=time.monotonic_ns(), **fields)


def frame(session, raw):
    # No JSON normalization: preserve original WS message text or binary bytes.
    if isinstance(raw, bytes):
        return event('frame', session, encoding='base64', raw=base64.b64encode(raw).decode('ascii'))
    return event('frame', session, encoding='utf8', raw=raw)


def complete_records(path):
    """Replay helper: a crash-truncated final line is not a valid record."""
    with Path(path).open('rb') as stream:
        for line in stream:
            if not line.endswith(b'\n'):
                return
            yield json.loads(line)


class SegmentStore:
    def __init__(self, directory, max_bytes, segment_bytes, sync_seconds=1.0):
        self.directory = Path(directory)
        self.directory.mkdir(parents=True, exist_ok=True)
        self.lock = (self.directory / '.capture.lock').open('a')
        try:
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BaseException:
            self.lock.close()
            raise
        self.max_bytes, self.segment_bytes = max_bytes, segment_bytes
        self.used = sum(p.stat().st_size for p in self.directory.glob('*.jsonl'))
        self.stream = None
        self.size = 0
        self.sync_seconds = sync_seconds
        self.last_sync = time.monotonic()
        self.dirty = False

    def sync(self, force=False):
        if self.stream and self.dirty and (force or time.monotonic() - self.last_sync >= self.sync_seconds):
            self.stream.flush()
            os.fsync(self.stream.fileno())
            self.dirty = False
            self.last_sync = time.monotonic()

    def append(self, record):
        data = (json.dumps(record, separators=(',', ':'), ensure_ascii=True) + '\n').encode()
        if self.used + len(data) > self.max_bytes:
            raise RuntimeError('capture storage limit reached; no files removed')
        if self.stream is None or self.size + len(data) > self.segment_bytes:
            if self.stream:
                self.sync(True)
                self.stream.close()
            path = self.directory / f'{time.time_ns()}-{uuid.uuid4().hex}.jsonl'
            self.stream = path.open('xb')
            fd = os.open(self.directory, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
            self.size = 0
        self.stream.write(data)
        self.size += len(data)
        self.used += len(data)
        self.dirty = True
        self.sync()

    def close(self):
        try:
            if self.stream:
                try:
                    self.sync(True)
                finally:
                    self.stream.close()
        finally:
            self.lock.close()


class Writer:
    """One dedicated disk thread; bounded queue, failure propagated to receiver."""
    def __init__(self, store, capacity):
        self.store = store
        self.queue = queue.Queue(capacity)
        self.error = None
        self.done = threading.Event()
        self.thread = threading.Thread(target=self.run, name='capture-writer')
        self.thread.start()

    def run(self):
        try:
            while not self.done.is_set() or not self.queue.empty():
                try:
                    item = self.queue.get(timeout=0.05)
                except queue.Empty:
                    self.store.sync()
                    continue
                self.store.append(item)
        except BaseException as exc:
            self.error = exc
        finally:
            try:
                self.store.close()
            except BaseException as exc:
                self.error = self.error or exc

    async def emit(self, item, *, stop=None, deadline=float('inf'), timeout=2.0):
        """Bounded cooperative backpressure; never drop or recreate a received frame.

        The caller creates the record before awaiting, preserving receive timestamps.
        A stopped/full writer is a visible incomplete session, never successful capture.
        """
        if not 0 < timeout <= 10:
            raise ValueError('enqueue timeout must be in (0,10]')
        until = min(deadline, time.monotonic() + timeout)
        while True:
            if self.error:
                raise RuntimeError('capture writer failed') from self.error
            if self.done.is_set() or not self.thread.is_alive():
                raise RuntimeError('capture writer stopped')
            if stop is not None and stop.is_set():
                raise RuntimeError('capture stopped before record accepted; session incomplete')
            remaining = until - time.monotonic()
            if remaining <= 0:
                raise RuntimeError('capture enqueue deadline exceeded; session incomplete')
            try:
                self.queue.put_nowait(item)
                return
            except queue.Full:
                # Yield to both the disk thread and loop; WS/TCP buffers stay bounded.
                await asyncio.sleep(min(0.005, remaining))

    def close(self):
        self.done.set()
        self.thread.join()
        if self.error:
            raise RuntimeError('capture writer failed') from self.error


async def capture(args, connect, stop):
    store = SegmentStore(args.directory, args.max_bytes, args.segment_bytes, args.sync_seconds)
    writer = Writer(store, args.queue_size)
    began = time.monotonic()
    deadline = began + args.duration if args.duration else float('inf')
    session = None
    next_connect = began
    try:
        while not stop.is_set() and time.monotonic() < deadline:
            # Poll stop/writer health while enforcing <= one attempt / reconnect-seconds.
            while time.monotonic() < next_connect and not stop.is_set() and time.monotonic() < deadline:
                if writer.error:
                    raise RuntimeError('capture writer failed') from writer.error
                await asyncio.sleep(min(0.1, next_connect - time.monotonic()))
            if stop.is_set() or time.monotonic() >= deadline:
                break
            session = uuid.uuid4().hex
            await writer.emit(event('session_start', session, url=URL, flags=CONF_FLAGS),
                              stop=stop, deadline=deadline, timeout=args.enqueue_seconds)
            next_connect = time.monotonic() + args.reconnect_seconds
            reason = 'completed'
            try:
                async with connect(URL, open_timeout=min(10, max(0.01, deadline-time.monotonic())),
                                   close_timeout=2, ping_interval=20, ping_timeout=20,
                                   max_size=256*1024, max_queue=16) as ws:
                    for request in subscriptions():
                        await ws.send(json.dumps(request))
                        await writer.emit(event('sent', session, request=request),
                                          stop=stop, deadline=deadline, timeout=args.enqueue_seconds)
                    while not stop.is_set() and time.monotonic() < deadline:
                        if writer.error:
                            raise RuntimeError('capture writer failed') from writer.error
                        try:
                            raw = await asyncio.wait_for(ws.recv(), timeout=min(0.1, max(0.001, deadline-time.monotonic())))
                        except asyncio.TimeoutError:
                            continue
                        await writer.emit(frame(session, raw), stop=stop, deadline=deadline,
                                          timeout=args.enqueue_seconds)
                    reason = 'signal' if stop.is_set() else 'duration'
            except (OSError, asyncio.TimeoutError) as exc:
                reason = type(exc).__name__
            except Exception as exc:
                # ConnectionClosed is provided by websockets; avoid dependency at import.
                if type(exc).__name__.startswith('ConnectionClosed'):
                    reason = type(exc).__name__
                else:
                    reason = 'fatal_' + type(exc).__name__
                    raise
            finally:
                primary = sys.exc_info()[1]
                try:
                    await writer.emit(event('disconnect', session, reason=reason),
                                      timeout=args.enqueue_seconds)
                except BaseException:
                    if primary is None:
                        raise
                    # Preserve the original frame/write failure; missing terminal record
                    # cannot be confused with success and remains visible in the journal.
                    print('capture terminal record unavailable; original failure retained', file=sys.stderr)
        await writer.emit(event('capture_stop', session, reason='signal' if stop.is_set() else 'duration'),
                          timeout=args.enqueue_seconds)
    finally:
        primary = sys.exc_info()[1]
        try:
            await asyncio.to_thread(writer.close)
        except BaseException:
            if primary is None:
                raise
            print('capture writer close failed; original failure retained', file=sys.stderr)


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', default='/var/lib/pirana-hft-research')
    parser.add_argument('--max-bytes', type=int, default=20*1024**3)
    parser.add_argument('--segment-bytes', type=int, default=64*1024**2)
    parser.add_argument('--queue-size', type=int, default=128)
    parser.add_argument('--enqueue-seconds', type=float, default=2.0)
    parser.add_argument('--sync-seconds', type=float, default=0.5)
    parser.add_argument('--reconnect-seconds', type=float, default=10)
    parser.add_argument('--duration', type=float, default=0)
    args = parser.parse_args(argv)
    if not (args.max_bytes > 0 and args.segment_bytes > 0 and args.queue_size > 0
            and 0 < args.sync_seconds <= 1 and args.reconnect_seconds >= 5 and args.duration >= 0
            and 0 < args.enqueue_seconds <= 10):
        parser.error('positive sizes, sync in (0,1], enqueue in (0,10], reconnect>=5s, duration>=0 required')
    return args


async def main(args):
    import websockets
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    await capture(args, websockets.connect, stop)


if __name__ == '__main__':
    asyncio.run(main(arguments()))
