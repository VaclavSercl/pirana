#!/usr/bin/env python3
"""Izolovaný výzkumný sidecar pro veřejné tick data (T2 experiment).

Připojuje se k VEŘEJNÉMU Bitfinex WS (žádné API klíče), sbírá trade ticky
do ODDĚLENÉHO souboru a zároveň je realtime streamuje přes lokální HTTP
endpoint pro validaci. Žádný kontakt s pirana.service, žádné burzovní
operace, žádné zápisy do trade_ledger.jsonl.

Výstup: /var/lib/pirana/research_ticks.jsonl — čistá data pro backtest
různých entry strategií bez kontaminace živým systémem.

Spuštění: nohup python3 scripts/tick_research.py &
"""
import asyncio
import json
import os
import signal
import sys
import time
from collections import deque
from datetime import datetime, timezone

# --- Config ---
WS_URL = "wss://api-pub.bitfinex.com/ws/2"
SYMBOL = "tBTCUSD"
DATA_PATH = "/var/lib/pirana/research_ticks.jsonl"
BUFFER_SIZE = 500  # flush každých N ticků
HEALTH_PORT = 8081  # lokální HTTP pro zdravotní check

# --- State ---
ticks = deque(maxlen=10000)  # rolling buffer pro analytiku
writer = None
writer_written = 0
running = True


def now_ms():
    return int(time.time() * 1000)


def tick_to_dict(msg):
    """Parse trade update [ID, MTS, AMOUNT, PRICE] z te/tu zprávy."""
    # msg: [chanId, "te"|"tu", [id, mts, amount, price]]
    if not isinstance(msg, list) or len(msg) < 3:
        return None
    trade = msg[2]
    if not isinstance(trade, list) or len(trade) < 4:
        return None
    trade_id, mts, amount, price = trade[0], trade[1], trade[2], trade[3]
    if not all(isinstance(x, (int, float)) for x in [mts, amount, price]):
        return None
    return {
        "tid": trade_id,
        "ts": mts // 1000,
        "ms": int(mts),
        "p": float(price),
        "q": abs(float(amount)),
        "s": 1 if amount > 0 else -1,  # buy-side vs sell-side
        "recv_ms": now_ms(),
    }


async def flush_ticks(force=False):
    global writer, writer_written
    if not ticks:
        return
    if writer is None:
        os.makedirs(os.path.dirname(DATA_PATH), exist_ok=True)
        writer = open(DATA_PATH, "a")
    while ticks:
        t = ticks.popleft()
        writer.write(json.dumps(t) + "\n")
        writer_written += 1
    if force or writer_written % BUFFER_SIZE == 0:
        writer.flush()


async def ws_loop():
    global running
    import websockets

    while running:
        try:
            async with websockets.connect(WS_URL) as ws:
                # Subscribe to public trades
                await ws.send(json.dumps({
                    "event": "subscribe",
                    "channel": "trades",
                    "symbol": SYMBOL
                }))
                # Wait for subscription confirmation (skip info event)
                while True:
                    resp = await asyncio.wait_for(ws.recv(), timeout=10)
                    data = json.loads(resp)
                    if data.get("event") == "subscribed":
                        break
                    if data.get("event") == "info":
                        continue  # skip initial info
                    print(f"[WARN] unexpected: {data}", flush=True)
                    await asyncio.sleep(5)
                    continue

                print(f"[OK] Subscribed to {SYMBOL} trades", flush=True)

                async for raw in ws:
                    if not running:
                        break
                    try:
                        msg = json.loads(raw)
                    except json.JSONDecodeError:
                        continue
                    # Heartbeat
                    if isinstance(msg, list) and msg[1] == "hb":
                        continue
                    # Trade update
                    if isinstance(msg, list) and len(msg) >= 3 and msg[1] in ("te", "tu"):
                        t = tick_to_dict(msg)
                        if t:
                            ticks.append(t)
                            await flush_ticks()

        except asyncio.CancelledError:
            break
        except Exception as e:
            print(f"[ERR] WS: {e}", flush=True)
            await asyncio.sleep(5)


async def health_server():
    """Lokální HTTP endpoint pro zdravotní check a statistiky."""
    from aiohttp import web

    async def handle(request):
        return web.json_response({
            "status": "ok",
            "ticks_buffered": len(ticks),
            "ticks_written": writer_written,
            "data_path": DATA_PATH,
            "uptime": now_ms(),
        })

    app = web.Application()
    app.router.add_get("/health", handle)
    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", HEALTH_PORT)
    await site.start()
    print(f"[OK] Health endpoint: http://127.0.0.1:{HEALTH_PORT}/health", flush=True)
    while running:
        await asyncio.sleep(1)
    await runner.cleanup()


async def main():
    global running
    loop = asyncio.get_event_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, lambda: setattr(sys.modules[__name__], "running", False))

    print(f"[START] Tick research sidecar → {DATA_PATH}", flush=True)
    await asyncio.gather(ws_loop(), health_server())
    await flush_ticks(force=True)
    if writer:
        writer.close()


if __name__ == "__main__":
    asyncio.run(main())
