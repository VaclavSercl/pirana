#!/usr/bin/env python3
"""Validate private Pirana decision evidence; never train, trade or invent fills.

Only complete observed prefixes are validated. The reader cannot observe the
writer's in-flight fsync completion. A live/ungraceful session has an
unverified tail. Early pre-candidate guards are counters, not rejected samples.
The output is a measurement dataset, NEVER a qualified inventory/exit replay.
"""
import argparse
from collections import Counter, defaultdict
from decimal import Decimal
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
import sqlite3
import sys
import time

MAX_FILE = 64 * 1024 * 1024
MAX_TOTAL = 2 * 1024 * 1024 * 1024
OUTCOMES = set('no_signal sell_cascade ask_wall shadow_only execution_busy position_persistence '
    'equity_uninitialized equity_mark_failed execution_recovery inventory_limit active_impulse '
    'minimum_spacing max_positions entry_gate_no_signal validator_rejected validator_error '
    'governance_denied governance_error paper_router_rejected paper_created router_rejected '
    'sizing_below_minimum insufficient_usd invalid_sizing slippage_guard intent_not_durable '
    'intent_handoff risk_rejected risk_error'.split())
FLOATS = 'price signed_quantity ofi l2 composite flow flow_hwm atr vpin buy_vpin sell_vpin vpin_threshold'.split()


def strict(raw):
    def pairs(items):
        out = {}
        for key, value in items:
            if key in out: raise ValueError('duplicate JSON key')
            out[key] = value
        return out
    return json.loads(raw, object_pairs_hook=pairs, parse_constant=lambda _: (_ for _ in ()).throw(ValueError('nonfinite JSON')))


def integer(value, positive=False):
    if type(value) is not int or value < (1 if positive else 0): raise ValueError('invalid integer')
    return value


def numeric(value, positive=False):
    if type(value) not in (int, float) or not math.isfinite(value) or (positive and value <= 0):
        raise ValueError('invalid numeric feature')
    return value


def safe_path(path):
    path = Path(path).absolute()
    for ancestor in [path, *path.parents]:
        if ancestor.is_symlink(): raise ValueError('symlink evidence path')
    return path


def manifest(path, expected):
    path = safe_path(path)
    if path.stat().st_size > 1024 * 1024: raise ValueError('oversized manifest')
    raw = path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != expected: raise ValueError('manifest hash mismatch')
    value = strict(raw)
    if value.get('schema_version') != 1 or value.get('verification_status') != 'PASS':
        raise ValueError('unverified deployment manifest')
    for key in ('binary_sha256', 'verification_evidence_sha256'):
        if not re.fullmatch('[a-f0-9]{64}', value.get(key, '')): raise ValueError('invalid manifest digest')
    for key in ('source_commit', 'source_tree'):
        if not isinstance(value.get(key), str) or not re.fullmatch('[a-f0-9]+', value[key]):
            raise ValueError('missing immutable source identity')
    return value


def frozen_events(path, sources):
    """Pin length/inode, verify observed prefix after parsing; appends are outside it."""
    path = safe_path(path)
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as f:
        before = os.fstat(f.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_FILE: raise ValueError('unsafe evidence file')
        raw = f.read(before.st_size)
        if len(raw) != before.st_size or not raw.endswith(b'\n'): raise ValueError('truncated evidence prefix')
        digest = hashlib.sha256(raw).hexdigest()
        for line in raw.splitlines():
            if not line or len(line) > 16384: raise ValueError('invalid evidence line')
            yield strict(line)
        f.seek(0)
        after_digest = hashlib.sha256()
        remaining = before.st_size
        while remaining:
            chunk = f.read(min(1024 * 1024, remaining))
            if not chunk: raise ValueError('evidence truncated during read')
            after_digest.update(chunk); remaining -= len(chunk)
        after = path.lstat()
        if ((before.st_dev, before.st_ino) != (after.st_dev, after.st_ino)
                or not stat.S_ISREG(after.st_mode) or after.st_size < before.st_size
                or after_digest.hexdigest() != digest): raise ValueError('evidence changed during read')
        sources.append(dict(name=path.name, sha256=digest, prefix_bytes=before.st_size,
                            appended_bytes_deferred=after.st_size-before.st_size))


def validate_record(r):
    for key in ('trade_id', 'exchange_ms', 'received_ms', 'observed_ms'): integer(r[key], True)
    if r['received_ms'] > r['observed_ms']: raise ValueError('future receipt')
    for key in FLOATS: numeric(r[key])
    if r['price'] <= 0 or not r['signed_quantity']: raise ValueError('invalid trade')
    for key in ('best_bid', 'best_ask', 'quantity', 'ioc_limit'):
        if r[key] is not None: numeric(r[key], True)
    if r['best_bid'] is None or r['best_ask'] is None or r['best_bid'] >= r['best_ask']:
        raise ValueError('missing or crossed admitted book')
    for key in ('raw_baseline', 'sell_cascade', 'ask_wall'):
        if type(r[key]) is not bool: raise ValueError('invalid boolean')
    # Same fixed baseline predicate, using the actual captured IEEE float inputs.
    baseline = r['flow'] > 0.08 and r['flow_hwm'] > 0 and r['price'] < r['flow_hwm'] * 0.9992
    if r['raw_baseline'] != baseline: raise ValueError('baseline replay mismatch')
    if r['route'] not in ('none', 'live_pullback_flow', 'shadow_candidate'): raise ValueError('unknown route')
    if r['outcome'] not in OUTCOMES: raise ValueError('unknown or unfinished outcome')
    if r['route'] == 'live_pullback_flow' and (not baseline or r['sell_cascade'] or r['ask_wall']):
        raise ValueError('live route contradicts recorded gates')
    for key in ('cid', 'intent_ms', 'handoff_ms'):
        if r[key] is not None: integer(r[key], True)
    if r['intent_ms'] is not None and r['intent_ms'] < r['observed_ms']: raise ValueError('intent before features')
    if r['handoff_ms'] is not None and (r['intent_ms'] is None or r['handoff_ms'] < r['intent_ms']):
        raise ValueError('handoff before intent')
    if r['outcome'] == 'intent_handoff' and any(r[k] is None for k in ('cid','quantity','ioc_limit','intent_ms','handoff_ms')):
        raise ValueError('missing handed-off intent identity')
    if r['outcome'] != 'intent_handoff' and r['handoff_ms'] is not None: raise ValueError('unexpected handoff')


def execution_join(r, by_cid):
    if r['outcome'] != 'intent_handoff': return dict(status='NO_LIVE_HANDOFF')
    if by_cid is None: return dict(status='NOT_CHECKED')
    rows = by_cid.get(str(r['cid']), [])
    if not rows: return dict(status='NOT_YET_MATCHED', limitation='No fill is not proof of cancellation or rejection')
    if len({f['order_id'] for f in rows}) != 1: raise ValueError('ambiguous intent across orders')
    qty = Decimal(0); net = Decimal(0); cost = Decimal(0); fees = defaultdict(Decimal)
    for f in rows:
        amount, price, fee = (Decimal(str(f[k])) for k in ('exec_amount','exec_price','fee'))
        if not all(v.is_finite() for v in (amount,price,fee)) or amount <= 0 or price <= 0:
            raise ValueError('invalid canonical buy fill')
        # This checks exchange clocks, not an invented order of ack receipt and venue fill.
        if f['symbol'] != 'tBTCUSD' or f['mts'] < r['intent_ms'] or f['fee_currency'] not in ('BTC','USD'):
            raise ValueError('canonical identity, clock or fee mismatch')
        qty += amount; net += amount + (fee if f['fee_currency']=='BTC' else 0)
        cost += amount*price - (fee if f['fee_currency']=='USD' else 0); fees[f['fee_currency']] += fee
    requested = Decimal(str(r['quantity']))
    if qty > requested + Decimal('0.000000000001') or net <= 0 or cost <= 0:
        raise ValueError('invalid fee-adjusted execution quantity')
    return dict(status='FULL_FILL_MATCHED' if abs(qty-requested)<=Decimal('0.000000000001') else 'PARTIAL_FILL_MATCHED',
        exchange_order_id=rows[0]['order_id'], fill_count=len(rows), gross_btc=str(qty), net_btc=str(net),
        acquisition_cost_usd=str(cost), signed_fees={k:str(v) for k,v in fees.items()},
        limitation='Entry acquisition only; not realized strategy profit')


def audit(directory, deployment, fills=None, output=None):
    directory = safe_path(directory)
    if not directory.is_dir(): raise ValueError('missing decision directory')
    paths = sorted(directory.glob('decisions-*.jsonl'))
    if not paths or len(paths)>65536 or sum(p.lstat().st_size for p in paths)>MAX_TOTAL:
        raise ValueError('missing or oversized evidence inventory')
    by_cid = None if fills is None else defaultdict(list)
    if fills is not None:
        seen_fills=set()
        for fill in fills:
            identity=(fill['trade_id'],fill['order_id'])
            if identity in seen_fills: raise ValueError('duplicate canonical execution')
            seen_fills.add(identity);by_cid[str(fill.get('cid'))].append(fill)
    sessions = {}; sources=[]; outcomes=Counter(); routes=Counter(); joins=Counter(); cids=set(); trade_ids=set(); total=0
    for path in paths:
        header=None
        for event in frozen_events(path, sources):
            if type(event.get('schema_version')) is not int or event['schema_version']!=1:
                raise ValueError('unknown event schema')
            session=event.get('session')
            if not isinstance(session,str) or not re.fullmatch('[A-Za-z0-9-]{1,96}',session): raise ValueError('invalid session')
            if header is None:
                provenance=event.get('provenance',{})
                if (event.get('event')!='header' or provenance.get('contract')!='pirana-entry-decision-v1'
                        or provenance.get('binary_sha256')!=deployment['binary_sha256']
                        or provenance.get('replay_qualified') is not False): raise ValueError('unbound source header')
                part=integer(event['part']);state=sessions.setdefault(session,dict(next_part=0,sequence=0,rows=0,last_ms=0,coverage=None))
                if part!=state['next_part'] or path.name!=f'decisions-{session}-{part:06d}.jsonl':
                    raise ValueError('missing or reordered segment')
                state['next_part']+=1;header=event;continue
            if session!=header['session']: raise ValueError('mixed session')
            state=sessions[session]
            if event['event']=='decision':
                seq=integer(event['sequence'],True)
                if seq!=state['sequence']+1: raise ValueError('lost or duplicated sequence')
                r=event['record'];validate_record(r)
                if r['trade_id'] in trade_ids: raise ValueError('duplicate economic market trade')
                trade_ids.add(r['trade_id'])
                if r['observed_ms']<state['last_ms']: raise ValueError('reversed decision clock')
                state.update(sequence=seq,last_ms=r['observed_ms'],rows=state['rows']+1)
                if r['cid'] is not None:
                    if r['cid'] in cids: raise ValueError('duplicate intent CID')
                    cids.add(r['cid'])
                joined=execution_join(r,by_cid)
                if r['outcome']!='intent_handoff' and r['cid'] is not None and by_cid and by_cid.get(str(r['cid'])):
                    raise ValueError('fill without recorded live handoff')
                outcomes[r['outcome']]+=1;routes[r['route']]+=1;joins[joined['status']]+=1;total+=1
                if output is not None:
                    output.write(json.dumps(dict(session=session,sequence=seq,decision=r,execution=joined,training_qualified=False),allow_nan=False)+'\n')
            elif event['event']=='coverage':
                for key in ('written_sequence','evaluated','candidates','lost','skipped_execution','skipped_cooldown','skipped_vpin_emergency','skipped_vpin_sell'):
                    integer(event[key])
                if event['lost'] or event['status'] not in ('RUNNING','STOPPED'): raise ValueError('writer loss or failure')
                if event['written_sequence']!=state['sequence'] or event['evaluated']<state['sequence'] or event['candidates']>event['evaluated']:
                    raise ValueError('coverage counter mismatch')
                if integer(event['observed_ms'],True)<state['last_ms']: raise ValueError('coverage before decision')
                previous=state['coverage']
                if previous and any(event[k]<previous[k] for k in ('observed_ms','evaluated','candidates','lost','skipped_execution','skipped_cooldown','skipped_vpin_emergency','skipped_vpin_sell')):
                    raise ValueError('reversed coverage counters')
                state['coverage']=event
            else: raise ValueError('unknown event kind')
    if any(s['coverage'] is None for s in sessions.values()): raise ValueError('missing coverage evidence')
    return dict(status='VERIFIED_RECORDED_PREFIX', rows=total,outcomes=dict(outcomes),routes=dict(routes),execution_matches=dict(joins),
        sessions=sessions,sources=sources,training_qualified=False,full_strategy_replay_qualified=False,
        limitations=['Observed bytes do not attest the completion of an in-flight writer fsync',
            'Live or abruptly stopped sessions have an unverified tail; recorded prefixes do not prove zero final loss',
            'Earlier guards are aggregate counts, not candidate feature samples',
            'Raw trades rejected before market admission are outside this dataset',
            'No counterfactual inventory/exit replay or 4-8 week holdout qualification',
            'Execution match proves entry acquisition, never total-account or strategy PnL'])


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory',required=True);parser.add_argument('--manifest',required=True)
    parser.add_argument('--manifest-sha256',required=True);parser.add_argument('--output',required=True)
    parser.add_argument('--canonical-db')
    args=parser.parse_args();destination=safe_path(args.output)
    destination.mkdir(mode=0o700)  # Never overwrite a prior assessment.
    try:
        deployment=manifest(args.manifest,args.manifest_sha256);fills=None;sync=None
        if args.canonical_db:
            sys.path.insert(0,str(Path(__file__).resolve().parents[3]))
            from scripts.pirana_measurements import canonical_fills
            fills,sync,_=canonical_fills(args.canonical_db)
            if not sync or sync[1]!=0 or sync[2]!=1 or not 0<=int(time.time()*1000)-sync[0]<=120000:
                raise ValueError('canonical history not fresh complete')
        with (destination/'rows.unqualified.jsonl').open('x') as output:
            os.fchmod(output.fileno(),0o600)
            result=audit(args.directory,deployment,fills,output);output.flush();os.fsync(output.fileno())
        result.update(manifest_sha256=args.manifest_sha256,canonical_sync=sync,generated_ms=int(time.time()*1000))
        with (destination/'audit.json').open('x') as f:
            os.fchmod(f.fileno(),0o600);json.dump(result,f,indent=2);f.flush();os.fsync(f.fileno())
        print(json.dumps({k:result[k] for k in ('status','rows','outcomes','execution_matches','training_qualified')}));return 0
    except (OSError,ValueError,KeyError,TypeError,ArithmeticError,sqlite3.Error) as error:
        # Preserve partial output explicitly unqualified. Never reuse it as a successful dataset.
        with (destination/'BLOCKED.json').open('x') as f:
            os.fchmod(f.fileno(),0o600);json.dump(dict(status='BLOCKED',reason=type(error).__name__,training_qualified=False),f)
        print('BLOCKED: invalid, unavailable or incomplete decision evidence',file=sys.stderr);return 2


if __name__=='__main__': raise SystemExit(main())
