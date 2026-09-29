#!/usr/bin/env python3
"""Read-only strategy calibration evidence, never a legacy-counter adapter.

Daily returns are realized strategy PnL in sats divided by observed account
opening equity in sats. They are not total-account or mark-to-market returns.
"""
from collections import defaultdict
from datetime import datetime, timezone
from decimal import Decimal, localcontext
import hashlib
import json
import os
import stat
import sqlite3
from pathlib import Path

DAY = 86400000
MAX_BYTES = 32 * 1024 * 1024
MAX_EQUITY_BYTES = 128 * 1024 * 1024
MAX_FILLS = 100000
SOURCE = "authenticated_strategy_position_roundtrips"


def strict_json(raw):
    def reject(_): raise ValueError("nonfinite_json")
    def pairs(values):
        out = {}
        for key, value in values:
            if key in out: raise ValueError("duplicate_json_key")
            out[key] = value
        return out
    return json.loads(raw, parse_float=Decimal, parse_constant=reject, object_pairs_hook=pairs)


def dec(value):
    if type(value) not in (str, int, float, Decimal) or len(str(value)) > 128:
        raise ValueError("invalid_decimal")
    result = Decimal(str(value))
    if not result.is_finite() or abs(result.adjusted()) > 50:
        raise ValueError("invalid_decimal")
    return result


def integer(value, positive=False):
    if type(value) is not int or value < (1 if positive else 0):
        raise ValueError("invalid_integer")
    return value


def text(value):
    return format(value, "f")


def _read(path, maximum):
    path = Path(path)
    if path.is_symlink() or not path.is_file(): raise ValueError("unsafe_or_missing_evidence")
    before = path.stat()
    if before.st_size > maximum: raise ValueError("evidence_size_limit")
    with path.open("rb") as handle: raw = handle.read(maximum + 1)
    after = path.stat()
    stamp = lambda s: (s.st_ino, s.st_size, s.st_mtime_ns)
    if len(raw) > maximum or stamp(before) != stamp(after): raise ValueError("evidence_changed_during_read")
    return raw, hashlib.sha256(raw).hexdigest()


def _reserved(fill):
    return (str(fill.get("trade_id")) in ("1978200001", "999999999999")
            or str(fill.get("order_id")) == "244505000001"
            or str(fill.get("cid")) == "28638000000001")


def _fills(con, now):
    owned = not con.in_transaction
    if owned: con.execute("BEGIN")
    try:
        sync = con.execute("SELECT cursor_ms,coverage_start_ms,complete FROM sync WHERE id=1").fetchone()
        if (not sync or type(sync[0]) is not int or not 0 <= now-sync[0] <= 120000
                or sync[1] != 0 or sync[2] != 1):
            raise ValueError("canonical_sync_not_fresh_complete")
        result, total, seen = [], 0, set()
        digest = hashlib.sha256()
        for tid, oid, payload in con.execute("SELECT trade_id,order_id,payload FROM fills ORDER BY trade_id,order_id"):
            if not isinstance(payload, str): raise ValueError("invalid_fill_payload_type")
            total += len(payload.encode())
            if total > MAX_BYTES or len(result) >= MAX_FILLS: raise ValueError("fill_evidence_limit")
            digest.update(payload.encode()); digest.update(b"\n")
            f = strict_json(payload)
            identity = (integer(f["trade_id"], True), integer(f["order_id"], True))
            if identity != (tid, oid) or identity in seen: raise ValueError("compound_fill_identity_mismatch")
            seen.add(identity)
            mts = integer(f["mts"], True)
            if mts > sync[0] or mts > now: raise ValueError("future_or_unsynced_fill")
            if f["symbol"] != "tBTCUSD" or dec(f["exec_amount"]) == 0 or dec(f["exec_price"]) <= 0:
                raise ValueError("invalid_canonical_fill")
            dec(f["fee"])
            cid = f.get("cid")
            if cid is not None and (type(cid) not in (str, int) or len(str(cid)) > 64):
                raise ValueError("invalid_cid")
            result.append(f)
        return sorted(result, key=lambda f: (f["mts"], f["trade_id"], f["order_id"])), sync[0], digest.hexdigest()
    finally:
        if owned: con.rollback()  # Only closes this helper's read transaction.


def _position(position, now):
    pid = integer(position["position_id"], True)
    order = integer(position["exchange_order_id"], True)
    if integer(position["entry_mts"], True) > now or dec(position["entry_price"]) <= 0:
        raise ValueError("invalid_position_entry_metadata")
    if position.get("side") != "Buy": raise ValueError("invalid_position_side")
    excluded = any(position.get(k) is not False for k in ("is_paper", "is_shadow", "is_rebalance"))
    return pid, order, excluded


def _roundtrips(fills, journal, now):
    if not isinstance(journal, dict) or type(journal.get("schema_version")) is not int or journal["schema_version"] != 2:
        raise ValueError("invalid_position_schema")
    current = set()
    for key in ("positions", "recovery_candidates"):
        if not isinstance(journal.get(key), list): raise ValueError("invalid_current_positions")
        for position in journal[key]: current.add(_position(position,now)[0])
    intents, settled = journal.get("exit_intents"), journal.get("settled_exit_cids")
    if not isinstance(intents, dict) or not isinstance(settled, list) or len(settled) != len(set(settled)):
        raise ValueError("invalid_exit_journal")
    if not all(isinstance(cid, str) and cid in intents for cid in settled): raise ValueError("missing_settled_intent")
    groups, mapping, order_owner = {}, {}, {}
    for cid, position in intents.items():
        if not isinstance(cid,str) or not cid.isascii() or not cid.isdecimal() or not 0 < int(cid) < 2**63:
            raise ValueError("invalid_exit_cid")
        pid, order, excluded = _position(position,now)
        if order in order_owner and order_owner[order] != pid: raise ValueError("ambiguous_entry_order")
        order_owner[order] = pid
        group = groups.setdefault(pid, dict(order=order, excluded=excluded, cids=[], sells=[],
                                           entry_metadata=(position.get("entry_mts"),position.get("entry_price"))))
        if (group["order"] != order or group["excluded"] != excluded
                or group["entry_metadata"] != (position.get("entry_mts"),position.get("entry_price"))): raise ValueError("conflicting_position_metadata")
        mapping[cid] = pid
        if cid in settled: group["cids"].append(cid)
    by_order = defaultdict(list)
    for fill in fills: by_order[fill["order_id"]].append(fill)
    bad_days, excluded_ids, trades, daily = defaultdict(set), [], [], defaultdict(lambda: [Decimal(0), Decimal(0)])
    assigned = set()
    for fill in fills:
        if _reserved(fill):
            excluded_ids.append([fill["trade_id"], fill["order_id"]])
            if dec(fill["exec_amount"]) < 0: bad_days[fill["mts"]//DAY].add("synthetic_execution")
            continue
        if dec(fill["exec_amount"]) >= 0: continue
        cid = str(fill.get("cid"))
        if cid not in mapping or cid not in settled:
            bad_days[fill["mts"]//DAY].add("unattributed_or_unsettled_sell")
            continue
        groups[mapping[cid]]["sells"].append(fill)
    for pid, group in sorted(groups.items()):
        sells = group["sells"]
        if not sells: continue
        reason = None
        buys = by_order.get(group["order"], [])
        if group["excluded"]: reason = "paper_shadow_or_rebalance_position"
        if not buys or any(_reserved(f) or dec(f["exec_amount"]) <= 0 for f in buys): reason = "unknown_actual_entry_basis"
        if any(f.get("fee_currency") not in ("USD", "BTC") for f in buys+sells): reason = "unsupported_fee_currency"
        if any(abs(dec(f["fee"])) >= abs(dec(f["exec_amount"])) *
               (dec(f["exec_price"]) if f.get("fee_currency") == "USD" else 1) for f in buys+sells):
            reason = "implausible_fee_quantity"
        if any(f["mts"] > min(s["mts"] for s in sells) for f in buys): reason = "entry_exit_time_conflict"
        for cid in group["cids"]:
            if len({f["order_id"] for f in sells if str(f.get("cid")) == cid}) > 1: reason = "ambiguous_exit_cid"
        if reason:
            for f in sells: bad_days[f["mts"]//DAY].add(reason)
            continue
        net_qty = sum((dec(f["exec_amount"]) + (dec(f["fee"]) if f["fee_currency"] == "BTC" else 0) for f in buys), Decimal(0))
        cost = sum((dec(f["exec_amount"]) * dec(f["exec_price"]) - (dec(f["fee"]) if f["fee_currency"] == "USD" else 0) for f in buys), Decimal(0))
        consumed = sum((-dec(f["exec_amount"]) - (dec(f["fee"]) if f["fee_currency"] == "BTC" else 0) for f in sells), Decimal(0))
        if net_qty <= 0 or cost <= 0 or consumed <= 0 or consumed > net_qty:
            for f in sells: bad_days[f["mts"]//DAY].add("invalid_fee_adjusted_quantity")
            continue
        pnl_sats, total_usd, fee_sats = Decimal(0), Decimal(0), Decimal(0)
        for f in buys+sells:
            fee_sats += -dec(f["fee"]) * (100000000 if f["fee_currency"] == "BTC" else Decimal(100000000)/dec(f["exec_price"]))
        for f in sells:
            key = (f["trade_id"], f["order_id"])
            if key in assigned: raise ValueError("execution_assigned_twice")
            assigned.add(key)
            qty, price, fee = -dec(f["exec_amount"]), dec(f["exec_price"]), dec(f["fee"])
            used = qty - (fee if f["fee_currency"] == "BTC" else 0)
            pnl = qty*price + (fee if f["fee_currency"] == "USD" else 0) - cost*used/net_qty
            sats = pnl/price*100000000
            daily[f["mts"]//DAY][0] += pnl; daily[f["mts"]//DAY][1] += sats
            total_usd += pnl; pnl_sats += sats
        if pid not in current and consumed != net_qty:
            for f in sells: bad_days[f["mts"]//DAY].add("closed_position_quantity_incomplete")
        if pid in current or consumed != net_qty: continue
        final = max(sells, key=lambda f: (f["mts"], f["trade_id"], f["order_id"]))
        quantity = sum((-dec(f["exec_amount"]) for f in sells), Decimal(0))
        price = sum((-dec(f["exec_amount"])*dec(f["exec_price"]) for f in sells), Decimal(0))/quantity
        trades.append(dict(pnl_sats=text(pnl_sats), ts=final["mts"]//1000, fill_price=text(price),
            vpin_at_close=0, side="Sell", qty=text(quantity), fee_sats=text(fee_sats),
            cid=str(final["cid"]), order_id=final["order_id"], trade_id=final["trade_id"],
            provenance=dict(position_id=pid, entry_order_id=group["order"], closed_mts=final["mts"],
                actual_entry_quantity_btc=text(net_qty), consumed_btc=text(consumed), pnl_usd=text(total_usd),
                entry_fills=[[f["trade_id"],f["order_id"]] for f in buys],
                exit_fills=[[f["trade_id"],f["order_id"]] for f in sells], vpin="UNAVAILABLE_NOT_MEASURED")))
    trades.sort(key=lambda t:(t["provenance"]["closed_mts"],t["provenance"]["position_id"]))
    return trades[-1000:], daily, bad_days, excluded_ids


def _equity_capture(path, maximum):
    """Bound one descriptor read to its initial length; append is not mutation."""
    if path.is_symlink(): raise ValueError("unsafe_equity_file")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as handle:
        before = os.fstat(handle.fileno())
        if not stat.S_ISREG(before.st_mode): raise ValueError("unsafe_equity_file")
        if before.st_size > maximum: raise ValueError("evidence_size_limit")
        raw = handle.read(before.st_size)
        after = os.fstat(handle.fileno())
    current = path.lstat()
    identity = (before.st_dev, before.st_ino)
    if (not stat.S_ISREG(current.st_mode) or identity != (current.st_dev,current.st_ino)
            or after.st_size < before.st_size or len(raw) != before.st_size):
        raise ValueError("equity_changed_during_read")
    if max(after.st_size,current.st_size) > maximum: raise ValueError("evidence_size_limit")
    return raw, identity


def _equity(directory, now):
    directory = Path(directory)
    if not directory.exists(): return [], [], ["equity_history_missing"]
    if directory.is_symlink() or not directory.is_dir(): raise ValueError("unsafe_equity_directory")
    paths = sorted(directory.glob("equity-*.jsonl"), reverse=True)
    samples, sources, used, limitations = [], [], 0, []
    earliest = (now//DAY-365)*DAY
    for path in paths[:367]:
        if used + path.stat().st_size > MAX_EQUITY_BYTES:
            limitations.append("equity_history_bounded"); break
        maximum = min(16*1024*1024, MAX_EQUITY_BYTES-used)
        raw, identity = _equity_capture(path, maximum)
        observed = raw
        deferred = 0
        if raw and not raw.endswith(b"\n"):
            # Only today's active append stream gets one bounded completion
            # retry. Historical truncation is corruption, never discarded.
            if path.name != f"equity-{now//DAY}.jsonl":
                raise ValueError("truncated_equity_stream")
            completed, retry_identity = _equity_capture(path, maximum)
            if retry_identity != identity or not completed.startswith(raw):
                raise ValueError("equity_changed_during_read")
            if not completed.endswith(b"\n"):
                raise ValueError("truncated_equity_stream")
            boundary = raw.rfind(b"\n") + 1
            deferred = len(completed)-boundary
            raw = raw[:boundary]
            observed = completed
        used += len(observed)
        digest = hashlib.sha256(raw).hexdigest()
        previous = None
        for line in raw.splitlines():
            if len(line)>4096: raise ValueError("equity_line_limit")
            s = strict_json(line)
            t = integer(s["observed_at_ms"], True)
            if previous is not None and t <= previous: raise ValueError("reversed_equity_stream")
            previous = t
            if t>now or type(s.get("schema_version")) is not int or s.get("schema_version") != 1 or s.get("source") != "authenticated_reconciled_wallet": raise ValueError("untrusted_equity_sample")
            if path.name != f"equity-{t//DAY}.jsonl": raise ValueError("equity_file_day_mismatch")
            if not isinstance(s.get("session_id"), str) or not s["session_id"] or not isinstance(s.get("boot_id"), str) or not s["boot_id"]: raise ValueError("equity_identity_missing")
            if any(not 0<=t-integer(s[k],True)<=30000 for k in ("wallet_at_ms","mark_at_ms")): raise ValueError("equity_sample_stale")
            btc, usd, price = (dec(s[k]) for k in ("btc_balance","usd_balance","btc_price"))
            if btc<0 or usd<0 or price<=0: raise ValueError("invalid_equity_value")
            equity = usd+btc*price
            if equity<=0: raise ValueError("nonpositive_equity")
            if t>=earliest: samples.append(dict(mts=t,usd=equity,sats=equity/price*100000000,session_id=s["session_id"],boot_id=s["boot_id"]))
        # Recheck precisely the bytes originally observed, including any
        # completed retry. Later appended samples are outside this snapshot.
        latest, latest_identity = _equity_capture(path, maximum)
        if latest_identity != identity or not latest.startswith(observed):
            raise ValueError("equity_changed_during_read")
        sources.append(dict(name=path.name, sha256=digest, prefix_bytes=len(raw),
            consistency="immutable_complete_prefix", snapshot_cutoff_ms=now,
            deferred_tail_bytes=deferred, appended_bytes_ignored=len(latest)-len(observed)))
    samples.sort(key=lambda s:s["mts"])
    if any(a["mts"]>=b["mts"] for a,b in zip(samples,samples[1:])): raise ValueError("duplicate_equity_time")
    return samples, sources, limitations


def _days(samples, daily, bad_days, now):
    buckets=defaultdict(list)
    for s in samples: buckets[s["mts"]//DAY].append(s)
    days, rejected = [], []
    for day in range(max(0,now//DAY-365),now//DAY):
        start,end=day*DAY,(day+1)*DAY
        values=buckets.get(day,[])
        endpoint=next(iter(buckets.get(day+1,[])),None)
        reasons=set(bad_days.get(day,[]))
        if not values or values[0]["mts"]-start>30000: reasons.add("opening_equity_missing")
        if endpoint is None or endpoint["mts"]-end>30000: reasons.add("closing_equity_missing")
        if values and endpoint and any(b["mts"]-a["mts"]>60000 for a,b in zip(values+[endpoint],(values+[endpoint])[1:])): reasons.add("equity_coverage_gap")
        if reasons:
            if values or day in daily or day in bad_days: rejected.append(dict(start_ms=start,reasons=sorted(reasons)))
            days=[]  # Keep only the consecutive validated suffix ending yesterday.
            continue
        first=values[0];usd,sats=daily.get(day,(Decimal(0),Decimal(0)))
        days.append(dict(date=datetime.fromtimestamp(start/1000,timezone.utc).date().isoformat(),
            start_ms=start,end_ms=end,return_value=text(sats/first["sats"]),realized_pnl_sats=text(sats),realized_pnl_usd=text(usd),
            opening_equity=dict(observed_at_ms=first["mts"],usd=text(first["usd"]),sats=text(first["sats"]),
                session_id=first["session_id"],boot_id=first["boot_id"]),sample_count=len(values),max_gap_ms=60000))
    return days[-365:], rejected[-365:]


def build_calibration(con, positions_path, equity_dir, now_ms):
    """Read-only bounded snapshot; caller persists the returned projection atomically."""
    report=dict(schema_version=1,source=SOURCE,generated_at_ms=now_ms,sync_cursor_ms=None,
        status="BLOCKED",reasons=[],trades=[],days=[],roundtrip_count=0,complete_day_count=0,
        scope="BTCUSD strategy realized sats PnL / observed account opening sats equity; not total-account return",
        thresholds=dict(min_roundtrips=50,min_complete_days=5),provenance={})
    try:
        integer(now_ms,True)
        with localcontext() as ctx:
            ctx.prec=80
            fills,cursor,fill_hash=_fills(con,now_ms)
            report["sync_cursor_ms"]=cursor
            raw,journal_hash=_read(positions_path,MAX_BYTES)
            journal=strict_json(raw)
            trades,daily,bad_days,excluded=_roundtrips(fills,journal,now_ms)
            samples,sources,limits=_equity(equity_dir,now_ms)
            days,rejected=_days(samples,daily,bad_days,now_ms)
            if _read(positions_path,MAX_BYTES)[1] != journal_hash: raise ValueError("position_journal_changed")
            reasons=[]
            if len(trades)<50: reasons.append("fewer_than_50_verified_roundtrips")
            if len(days)<5: reasons.append("fewer_than_5_consecutive_complete_UTC_days")
            report.update(status="WARMUP" if reasons else "READY",reasons=reasons,trades=trades,days=days,
                roundtrip_count=len(trades),complete_day_count=len(days),
                provenance=dict(fill_payloads_sha256=fill_hash,positions_sha256=journal_hash,equity_sources=sources,
                    excluded_synthetic_fill_identities=excluded,rejected_days=rejected,limitations=limits,
                    unverified_execution_days=[dict(start_ms=day*DAY,reasons=sorted(reasons)) for day,reasons in sorted(bad_days.items())[-366:]],
                    vpin="Unavailable: zero placeholder must not be treated as measured VPIN",
                    fees="Signed canonical BTC/USD fees included; no assumed zero fees",
                    daily_zero="Only complete observed days with no attributed realized PnL may be zero",
                    cashflows="Denominator is observed account equity, not a strategy allocation or cashflow-adjusted total return"))
    except (OSError,ValueError,TypeError,KeyError,ArithmeticError,sqlite3.Error) as error:
        # Reasons are static validation identifiers, never data or exception URLs.
        allowed=str(error)
        report["reasons"]=[allowed if allowed.isascii() and allowed.replace("_","").isalnum() and len(allowed)<100 else "invalid_source_evidence"]
    return report
