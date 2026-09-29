#!/usr/bin/env python3
"""Read-only measurement report. Never backfill missing prices or change trading state."""
import argparse
from collections import defaultdict
from decimal import Decimal
import hashlib
import json
from pathlib import Path
import sqlite3
import time


def dec(value):
    d = Decimal(str(value))
    if not d.is_finite() or abs(d.adjusted()) > 100:
        raise ValueError('invalid finite measurement')
    return d


def num(value):
    return format(value, 'f')


def integer(value):
    if type(value) is not int or value < 0:
        raise ValueError('invalid timestamp or identifier')
    return value


def canonical_fills(db):
    with sqlite3.connect(Path(db).resolve().as_uri() + '?mode=ro', uri=True) as con:
        con.execute('BEGIN')
        try:
            from scripts.pirana_accounting import execution_fills
        except ImportError:
            from pirana_accounting import execution_fills
        raw, fills, provenance = execution_fills(con)
        actual_ids = {(f['trade_id'], f['order_id']) for f in fills}
        excluded_indexes = {x['fill_index'] for x in provenance['excluded_records']}
        for index, f in enumerate(raw):
            if (f['trade_id'], f['order_id']) not in actual_ids and index not in excluded_indexes:
                provenance['excluded_records'].append(dict(fill_index=index,
                    trade_id=f['trade_id'], order_id=f['order_id'], cid=f.get('cid'),
                    classification='typed_operator_adjustment',
                    reason='Immutable fill provenance identifies a non-execution adjustment'))
        if provenance['excluded_records']:
            provenance.update(status='MIXED_UNVERIFIED', source='mixed_execution_and_operator_adjustments')
        sync = con.execute('SELECT cursor_ms,coverage_start_ms,complete FROM sync WHERE id=1').fetchone()
    seen = set()
    for f in fills:
        key = (integer(f['trade_id']), integer(f['order_id']))
        if key in seen:
            raise ValueError('duplicate canonical execution identity')
        seen.add(key)
        integer(f['mts'])
        if f['symbol'] != 'tBTCUSD' or not dec(f['exec_amount']) or dec(f['exec_price']) <= 0:
            raise ValueError('invalid execution')
        dec(f['fee'])
    return sorted(fills, key=lambda f: (f['mts'], f['trade_id'], f['order_id'])), sync, provenance


def historical_gaps(fills):
    """Identify unmatched inventory, NOT invent cost basis for external deposits."""
    balance = Decimal(0)
    gaps = []
    for f in fills:
        qty, fee = dec(f['exec_amount']), dec(f['fee'])
        if fee and f['fee_currency'] not in ('USD', 'BTC'):
            gaps.append(dict(trade_id=f['trade_id'], order_id=f['order_id'], mts=f['mts'], reason='unsupported_fee_currency'))
        change = qty + (fee if f['fee_currency'] == 'BTC' else Decimal(0))
        balance += change
        if balance < 0:
            gaps.append(dict(trade_id=f['trade_id'], order_id=f['order_id'], mts=f['mts'], reason='missing_acquisition_basis', missing_btc=num(-balance), missing_sats=num(-balance * 100000000)))
            balance = Decimal(0)
    return dict(status='INCOMPLETE' if gaps else 'NO_INVENTORY_DEFICIT', scope='account:tBTCUSD_fills_only', gaps=gaps,
                limitation='Other pairs, deposits and withdrawals require separate acquisition/ledger evidence; absence of a deficit does not prove full account performance.')


def slippage(fills, benchmarks, start_ms, end_ms):
    selected = [f for f in fills if start_ms <= f['mts'] <= end_ms]
    all_by_cid = defaultdict(list)
    for f in fills:
        all_by_cid[str(f.get('cid'))].append(f)
    verified, missing, invalid = [], [], []
    for f in selected:
        identity = dict(trade_id=f['trade_id'], order_id=f['order_id'])
        cid = str(f.get('cid'))
        b = benchmarks.get(cid)
        if b is None:
            missing.append(identity)
            continue
        try:
            ref, requested = dec(b['reference_price']), dec(b['requested_quantity'])
            decision = integer(b['decision_mts'])
            if ref <= 0 or requested <= 0 or b['benchmark_kind'] != 'signal_last_trade' or b['side'] not in ('Buy', 'Sell'):
                raise ValueError('invalid benchmark')
            related = all_by_cid[cid]
            buy = b['side'] == 'Buy'
            if len({x['order_id'] for x in related}) != 1:
                raise ValueError('ambiguous CID across orders')
            if any((dec(x['exec_amount']) > 0) != buy or x['mts'] < decision for x in related):
                raise ValueError('side or clock mismatch')
            if sum(abs(dec(x['exec_amount'])) for x in related) > requested + Decimal('0.000000000001'):
                raise ValueError('fills exceed benchmark quantity')
            qty, price = abs(dec(f['exec_amount'])), dec(f['exec_price'])
            signed_cost = (price - ref) * (1 if buy else -1) * qty
            verified.append(dict(**identity, cid=cid, quantity_btc=num(qty), reference_price=num(ref), fill_price=num(price), adverse_cost_usd=num(signed_cost), adverse_bps=num(signed_cost / (qty * ref) * 10000)))
        except (ValueError, KeyError, ArithmeticError):
            invalid.append(identity)
    notional = sum((dec(x['quantity_btc']) * dec(x['reference_price']) for x in verified), Decimal(0))
    cost = sum((dec(x['adverse_cost_usd']) for x in verified), Decimal(0))
    return dict(status='NO_FILLS' if not selected else ('VERIFIED' if len(verified) == len(selected) else 'INCOMPLETE'),
                scope='fills_with_durable_signal_last_trade_benchmark', benchmark='signal trade price, not executable BBO; fees excluded; positive is adverse',
                fill_count=len(selected), verified_count=len(verified), missing=missing, invalid=invalid, fills=verified,
                verified_subset_weighted_bps=num(cost / notional * 10000) if notional else None,
                verified_subset_cost_usd=num(cost) if notional else None)


def read_equity(directory):
    samples, sources = [], []
    directory = Path(directory)
    if not directory.is_dir():
        return [], []
    for path in sorted(directory.glob('equity-*.jsonl')):
        if path.is_symlink() or path.stat().st_size > 16 * 1024 * 1024:
            raise ValueError('unsafe or oversized equity evidence')
        raw = path.read_bytes()
        if raw and not raw.endswith(b'\n'):
            raise ValueError('truncated equity evidence')
        sources.append(dict(name=path.name, sha256=hashlib.sha256(raw).hexdigest()))
        for line in raw.splitlines():
            samples.append(json.loads(line, parse_float=str))
    return samples, sources


def equity_report(samples, start_ms, end_ms, max_gap_ms=60000):
    integer(start_ms); integer(end_ms)
    if end_ms < start_ms or max_gap_ms <= 0:
        raise ValueError('invalid measurement interval')
    chosen = []
    last = None
    for s in samples:
        t = integer(s['observed_at_ms'])
        if last is not None and t <= last:
            raise ValueError('duplicate or reversed equity time')
        last = t
        if s['schema_version'] != 1 or s['source'] != 'authenticated_reconciled_wallet':
            raise ValueError('untrusted equity source')
        if not s['session_id'] or not s['boot_id']:
            raise ValueError('missing continuity identity')
        if not 0 <= t - integer(s['wallet_at_ms']) <= 30000 or not 0 <= t - integer(s['mark_at_ms']) <= 30000:
            raise ValueError('stale or future mark/wallet')
        btc, usd, price = (dec(s[k]) for k in ('btc_balance', 'usd_balance', 'btc_price'))
        if btc < 0 or usd < 0 or price <= 0:
            raise ValueError('invalid spot equity')
        equity = usd + btc * price
        if equity <= 0:
            raise ValueError('nonpositive equity')
        if start_ms <= t <= end_ms:
            chosen.append(dict(mts=t, usd=equity, sats=equity / price * 100000000, session=(s['session_id'], s['boot_id'])))
    segments, gaps = [], []
    for x in chosen:
        if not segments or x['mts'] - segments[-1][-1]['mts'] > max_gap_ms or x['session'] != segments[-1][-1]['session']:
            if segments:
                gaps.append(dict(start_ms=segments[-1][-1]['mts'], end_ms=x['mts'], reason='gap_or_session_change'))
            segments.append([])
        segments[-1].append(x)
    results = []
    for segment in segments:
        peak = {k: segment[0][k] for k in ('usd', 'sats')}
        dd = {k: Decimal(0) for k in peak}
        for x in segment:
            for k in peak:
                peak[k] = max(peak[k], x[k])
                dd[k] = max(dd[k], (peak[k] - x[k]) / peak[k] * 100)
        results.append(dict(start_ms=segment[0]['mts'], end_ms=segment[-1]['mts'], count=len(segment),
                            sampled_unadjusted_max_drawdown_pct={k: num(v) if len(segment) >= 2 else None for k, v in dd.items()}))
    complete = (len(chosen) >= 2 and not gaps and chosen[0]['mts'] - start_ms <= max_gap_ms and end_ms - chosen[-1]['mts'] <= max_gap_ms)
    return dict(status='SAMPLED_COVERAGE' if complete else 'INCOMPLETE', start_ms=start_ms, end_ms=end_ms, max_gap_ms=max_gap_ms,
                sample_count=len(chosen), segments=results, gaps=gaps, scope='exchange BTC+USD balances only',
                cashflow_adjusted_performance='UNVERIFIED: deposits, withdrawals and other-pair flows require authenticated attribution',
                limitation='Observed sampled balance drawdown, not continuous intraperiod maximum or strategy return; cash flows can alter balances.')


def read_benchmarks(path):
    path = Path(path)
    if path.is_symlink():
        raise ValueError('unsafe benchmark evidence path')
    if not path.exists():
        return {}
    if path.stat().st_size > 64 * 1024 * 1024:
        raise ValueError('benchmark evidence oversized')
    envelope = json.loads(path.read_text(), parse_float=str)
    if envelope.get('schema_version') != 1 or not isinstance(envelope.get('decision_benchmarks'), dict):
        raise ValueError('invalid benchmark envelope')
    return envelope['decision_benchmarks']


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--db', required=True); p.add_argument('--positions', required=True)
    p.add_argument('--benchmarks', help='default: sibling positions.benchmarks.json')
    p.add_argument('--equity-dir', required=True); p.add_argument('--start-ms', type=int, required=True)
    p.add_argument('--end-ms', type=int, default=None)
    p.add_argument('--funding-valuations', help='optional funding-date market marks or owner-declared basis; never changes FIFO')
    args = p.parse_args()
    end = args.end_ms if args.end_ms is not None else int(time.time() * 1000)
    if args.start_ms < 0 or end < args.start_ms:
        p.error('invalid interval')
    fills, sync, provenance = canonical_fills(args.db)
    excluded = provenance['excluded_records']
    benchmarks = read_benchmarks(args.benchmarks or Path(args.positions).with_suffix('.benchmarks.json'))
    samples, sources = read_equity(args.equity_dir)
    fresh = bool(sync and sync[2] == 1 and sync[1] == 0 and 0 <= end - sync[0] <= 120000)
    report = dict(schema_version=1, generated_at_ms=int(time.time() * 1000), start_ms=args.start_ms, end_ms=end,
                  canonical_sync=list(sync) if sync else None, canonical_fresh_complete=fresh, equity_sources=sources,
                  execution_provenance=provenance,
                  history=historical_gaps(fills), slippage=slippage(fills, benchmarks, args.start_ms, end),
                  equity=equity_report(samples, args.start_ms, end))
    report['status'] = 'INCOMPLETE' if excluded or not fresh or any(report[k]['status'] == 'INCOMPLETE' for k in ('history', 'slippage', 'equity')) else 'PASS_WITH_LIMITATIONS'
    if args.funding_valuations:
        try:
            try:
                from pirana_funding_valuations import load_valuations
            except ImportError:
                from scripts.pirana_funding_valuations import load_valuations
            report['funding_valuations'] = load_valuations(args.funding_valuations)
            owner_basis = report['funding_valuations']['classification'] == 'owner_declared_cost_basis'
            report['funding_valuation_semantics'] = dict(
                kind='owner_declared_cost_basis' if owner_basis else 'funding_date_market_valuation',
                label='Owner-declared purchase basis, not independently verified' if owner_basis else 'Owner-approved funding-date market valuation, not purchase cost',
                historical_performance='UNVERIFIED: declaration or mark does not establish complete flows or strategy attribution',
                canonical_fifo='UNCHANGED')
        except (OSError, ValueError, KeyError, TypeError, ArithmeticError):
            report['funding_valuations'] = dict(status='UNVERIFIED', actual_acquisition_basis='UNKNOWN',
                                               reason='invalid funding valuation evidence')
            report['status'] = 'INCOMPLETE'
            print(json.dumps(report, sort_keys=True))
            return 2
    print(json.dumps(report, sort_keys=True))
    return 0 if fresh else 2


if __name__ == '__main__':
    raise SystemExit(main())
