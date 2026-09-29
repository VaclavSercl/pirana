"""All stores and WebSockets are fake; never open the live research directory."""
import asyncio
import importlib.util
from pathlib import Path
import threading
import time
import pytest

spec = importlib.util.spec_from_file_location("hft_capture_tested", Path(__file__).parents[1] / "scripts/research/hft_capture.py")
hft = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hft)


class Store:
    def __init__(self, blocked=False, fail=False):
        self.entered = threading.Event()
        self.release = threading.Event()
        if not blocked:
            self.release.set()
        self.rows = []
        self.fail = fail
        self.closed = False
    def append(self, row):
        self.entered.set()
        assert self.release.wait(2), "test barrier was not released"
        if self.fail:
            raise OSError("fake disk failure")
        self.rows.append(row)
    def sync(self):
        pass
    def close(self):
        self.closed = True


async def saturated():
    store = Store(blocked=True)
    writer = hft.Writer(store, 1)
    await writer.emit({"n": 0})
    assert await asyncio.to_thread(store.entered.wait, 1)
    await writer.emit({"n": 1})
    return store, writer


def test_transient_backpressure_preserves_order_timestamp_and_event_loop():
    async def run():
        store, writer = await saturated()
        record = hft.frame("session", "exact raw frame")
        original = dict(record)
        pending = asyncio.create_task(writer.emit(record))
        try:
            # Queue remains full while the independent loop task runs.
            await asyncio.sleep(0)
            assert not pending.done()
            assert writer.queue.qsize() == 1
            store.release.set()
            await asyncio.wait_for(pending, 1)
        finally:
            store.release.set()
            await asyncio.to_thread(writer.close)
        assert store.rows == [{"n": 0}, {"n": 1}, original]
        assert record == original
        assert store.closed
    asyncio.run(run())


@pytest.mark.parametrize("mode", ["timeout", "stop", "duration", "cancel"])
def test_full_queue_exit_is_bounded_and_never_accepts_missing_record(mode):
    async def run():
        store, writer = await saturated()
        stop = asyncio.Event()
        kwargs = {"timeout": .02}
        if mode == "stop":
            stop.set()
            kwargs["stop"] = stop
        if mode == "duration":
            kwargs["deadline"] = time.monotonic() - 1
        try:
            task = asyncio.create_task(writer.emit({"n": 2}, **kwargs))
            if mode == "cancel":
                await asyncio.sleep(0)
                task.cancel()
                with pytest.raises(asyncio.CancelledError):
                    await task
            else:
                with pytest.raises(RuntimeError, match="incomplete"):
                    await asyncio.wait_for(task, 1)
            assert writer.queue.qsize() == 1
        finally:
            store.release.set()
            await asyncio.to_thread(writer.close)
        assert store.rows == [{"n": 0}, {"n": 1}]
    asyncio.run(run())


def test_writer_failure_is_causal_and_no_later_record_accepted():
    async def run():
        store = Store(fail=True)
        writer = hft.Writer(store, 1)
        await writer.emit({"n": 0})
        await asyncio.to_thread(writer.thread.join, 1)
        assert not writer.thread.is_alive()
        with pytest.raises(RuntimeError, match="writer failed") as error:
            await writer.emit({"n": 1})
        assert isinstance(error.value.__cause__, OSError)
        assert not store.rows
        with pytest.raises(RuntimeError, match="writer failed"):
            writer.close()
    asyncio.run(run())


def test_stopped_writer_cannot_accept_record():
    async def run():
        writer = hft.Writer(Store(), 1)
        await asyncio.to_thread(writer.close)
        with pytest.raises(RuntimeError, match="writer stopped"):
            await writer.emit({"n": 1})
        assert writer.queue.empty()
    asyncio.run(run())


def test_original_receive_failure_survives_terminal_and_close_failure(monkeypatch, capsys):
    original = RuntimeError("original frame failure")
    class Writer:
        def __init__(self, *args):
            self.error = None
        async def emit(self, record, **kwargs):
            if record["kind"] == "frame":
                raise original
            if record["kind"] == "disconnect":
                raise RuntimeError("secondary terminal failure")
        def close(self):
            raise RuntimeError("secondary close failure")
    class Socket:
        async def __aenter__(self): return self
        async def __aexit__(self, *args): pass
        async def send(self, request): pass
        async def recv(self): return "original raw"
    monkeypatch.setattr(hft, "Writer", Writer)
    monkeypatch.setattr(hft, "SegmentStore", lambda *args: object())
    async def run():
        with pytest.raises(RuntimeError) as error:
            await hft.capture(hft.arguments([]), lambda *a, **k: Socket(), asyncio.Event())
        assert error.value is original
    asyncio.run(run())
    assert "terminal record unavailable" in capsys.readouterr().err


def test_signal_racing_received_frame_is_explicitly_incomplete(monkeypatch):
    store = Store()
    stop = asyncio.Event()
    sent = []
    class Socket:
        async def __aenter__(self): return self
        async def __aexit__(self, *args): pass
        async def send(self, request): sent.append(request)
        async def recv(self):
            # Signal after receipt must not quietly classify an unaccepted frame as saved.
            stop.set()
            return "unaccepted raw"
    monkeypatch.setattr(hft, "SegmentStore", lambda *args: store)
    async def run():
        with pytest.raises(RuntimeError, match="session incomplete"):
            await hft.capture(hft.arguments([]), lambda *a, **k: Socket(), stop)
    asyncio.run(run())
    assert len(sent) == 4
    assert store.rows[0]["kind"] == "session_start"
    assert store.rows[-1]["kind"] == "disconnect"
    assert store.rows[-1]["reason"] == "fatal_RuntimeError"
    assert not any(row["kind"] == "capture_stop" for row in store.rows)


@pytest.mark.parametrize("value", ["nan", "inf", "0", "-1", "11"])
def test_enqueue_budget_validated(value):
    with pytest.raises(SystemExit): hft.arguments(["--enqueue-seconds", value])


def test_storage_and_memory_defaults_unchanged():
    args = hft.arguments([])
    assert args.queue_size == 128
    assert args.max_bytes == 20 * 1024**3
    assert args.segment_bytes == 64 * 1024**2
    assert args.sync_seconds == .5
    store = object.__new__(hft.SegmentStore)
    store.max_bytes = 1
    store.used = 1
    with pytest.raises(RuntimeError, match="storage limit"):
        store.append({"n": 1})


def test_burst_larger_than_capacity_drains_without_duplicates():
    async def run():
        store = Store()
        writer = hft.Writer(store, 2)
        try:
            for n in range(350):
                await writer.emit({"n": n})
                assert writer.queue.qsize() <= 2
        finally:
            await asyncio.to_thread(writer.close)
        assert [r["n"] for r in store.rows] == list(range(350))
    asyncio.run(run())
