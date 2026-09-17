#!/usr/bin/env python3
"""Durable account-wide Bitfinex BTC/USD ledger. No exchange/network access."""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import sys
from decimal import Decimal, localcontext
from zoneinfo import ZoneInfo

SCHEMA = 1  # Public projection contract.
DB_SCHEMA = 2
EPOCH_DDL = 'CREATE TABLE IF NOT EXISTS trading_epoch(id INTEGER PRIMARY KEY CHECK(id=1), name TEXT NOT NULL, start_ms INTEGER NOT NULL, opening_reserved_btc TEXT NOT NULL)'
PERIOD_DDL = 'CREATE TABLE IF NOT EXISTS reporting_period(id INTEGER PRIMARY KEY CHECK(id=1), period_id TEXT NOT NULL, start_ms INTEGER NOT NULL)'
SCAN_DDL = 'CREATE TABLE IF NOT EXISTS scan(id INTEGER PRIMARY KEY CHECK(id=1), start_ms INTEGER NOT NULL, next_ms INTEGER NOT NULL, target_ms INTEGER NOT NULL)'
DDL = '''
CREATE TABLE fills(trade_id INTEGER NOT NULL, order_id INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(trade_id,order_id));
CREATE TABLE sync(id INTEGER PRIMARY KEY CHECK(id=1), cursor_ms INTEGER NOT NULL, coverage_start_ms INTEGER, complete INTEGER NOT NULL);
INSERT INTO sync VALUES(1,0,NULL,0);
CREATE TABLE legacy(hash TEXT PRIMARY KEY, source TEXT NOT NULL, raw BLOB NOT NULL, records TEXT NOT NULL);
PRAGMA user_version=2;
'''

def decimal(value):
    if not isinstance(value, str) or len(value)>128:
        raise ValueError('decimal must be a bounded string')
    d = Decimal(value)
    if not d.is_finite() or abs(d.adjusted()) > 100:
        raise ValueError('invalid decimal')
    return d

def number(d):
    value = format(d, 'f') if d else '0'
    return value.rstrip('0').rstrip('.') if '.' in value else value

def validate_schema(con, version):
    if version not in (1, DB_SCHEMA):
        raise ValueError('unsupported database schema')
    tables = {
        'fills': (['trade_id','payload'], [1,0]) if version == 1 else (['trade_id','order_id','payload'], [1,2,0]),
        'sync': (['id','cursor_ms','coverage_start_ms','complete'], [1,0,0,0]),
        'legacy': (['hash','source','raw','records'], [1,0,0,0]),
    }
    for table, (columns, primary_key) in tables.items():
        info = list(con.execute('PRAGMA table_info('+table+')'))
        if [r[1] for r in info] != columns or [r[5] for r in info] != primary_key:
            raise ValueError('invalid database schema')
    if con.execute('SELECT count(*) FROM sync WHERE id=1').fetchone()[0] != 1:
        raise ValueError('invalid sync state')
    if con.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='scan'").fetchone():
        info=list(con.execute('PRAGMA table_info(scan)'))
        if [r[1] for r in info]!=['id','start_ms','next_ms','target_ms'] or [r[5] for r in info]!=[1,0,0,0]:
            raise ValueError('invalid scan schema')
    state(con)
    reporting_period(con)
    trading_epoch(con)


def reporting_period(con):
    if con is None or not con.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='reporting_period'").fetchone():
        return None
    info=list(con.execute('PRAGMA table_info(reporting_period)'))
    if [r[1] for r in info]!=['id','period_id','start_ms'] or [r[5] for r in info]!=[1,0,0]:
        raise ValueError('invalid reporting period schema')
    rows=con.execute('SELECT id,period_id,start_ms FROM reporting_period').fetchall()
    if not rows:
        return None
    if len(rows)!=1 or rows[0][0]!=1:
        raise ValueError('invalid reporting period state')
    _,period_id,start_ms=rows[0]
    validate_period(period_id,start_ms)
    return dict(id=period_id,start_ms=start_ms)


def validate_period(period_id,start_ms):
    integer(start_ms)
    if not isinstance(period_id,str) or not period_id.strip() or len(period_id)>128:
        raise ValueError('invalid reporting period ID')


def start_period(con,period_id,start_ms):
    """Establish one immutable reporting boundary without resetting accounting."""
    validate_period(period_id,start_ms)
    con.execute('BEGIN IMMEDIATE')
    try:
        old=reporting_period(con)
        if old is not None and old!=dict(id=period_id,start_ms=start_ms):
            raise ValueError('reporting period already established with different boundary or ID')
        con.execute(PERIOD_DDL)
        if old is None:
            con.execute('INSERT INTO reporting_period VALUES(1,?,?)',(period_id,start_ms))
        con.commit()
    except BaseException:
        con.rollback()
        raise


def trading_epoch(con):
    if con is None or not con.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='trading_epoch'").fetchone():
        return None
    info=list(con.execute('PRAGMA table_info(trading_epoch)'))
    if [r[1] for r in info]!=['id','name','start_ms','opening_reserved_btc'] or [r[5] for r in info]!=[1,0,0,0]:
        raise ValueError('invalid trading epoch schema')
    rows=con.execute('SELECT id,name,start_ms,opening_reserved_btc FROM trading_epoch').fetchall()
    if not rows:
        return None
    if len(rows)!=1 or rows[0][0]!=1:
        raise ValueError('invalid trading epoch state')
    _,name,start_ms,reserve=rows[0]
    validate_period(name,start_ms)
    if decimal(reserve)<0 or number(decimal(reserve))!=reserve:
        raise ValueError('invalid opening BTC reserve')
    return dict(id=name,start_ms=start_ms,reserved_btc=reserve)


def start_trading_epoch(con,name,start_ms,reserved_btc,now=None):
    """Caller must quiesce trading and authenticate wallets/no open orders first.

    Opening reserve has unknown basis and is never operational FIFO inventory.
    A synchronized exact boundary prevents reclassifying known executions.
    """
    validate_period(name,start_ms)
    reserve=decimal(reserved_btc)
    if reserve<0:
        raise ValueError('negative opening BTC reserve')
    requested=dict(id=name,start_ms=start_ms,reserved_btc=number(reserve))
    now=now or dt.datetime.now(dt.timezone.utc)
    if now.tzinfo is None:
        raise ValueError('epoch time needs timezone')
    con.execute('BEGIN IMMEDIATE')
    try:
        old=trading_epoch(con)
        if old is not None:
            if old!=requested:
                raise ValueError('trading epoch already established with different parameters')
        else:
            sync=state(con)
            now_ms=int(now.timestamp()*1000)
            if (not sync['complete'] or sync['coverage_start_ms']!=0 or sync['scan'] is not None
                    or sync['cursor_ms']!=start_ms or not 0<=now_ms-start_ms<=120000):
                raise ValueError('trading epoch requires fresh complete sync at exact boundary')
            if any(json.loads(r[0])['mts']>=start_ms for r in con.execute('SELECT payload FROM fills')):
                raise ValueError('trading epoch boundary already contains executions')
            con.execute(EPOCH_DDL)
            con.execute('INSERT INTO trading_epoch VALUES(1,?,?,?)',(name,start_ms,requested['reserved_btc']))
        con.commit()
    except BaseException:
        con.rollback()
        raise


def migrate_schema1(con):
    # Caller holds BEGIN IMMEDIATE: schema, payloads, and version commit together.
    con.execute('CREATE TABLE fills_v2(trade_id INTEGER NOT NULL, order_id INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(trade_id,order_id))')
    for trade_id, payload in con.execute('SELECT trade_id,payload FROM fills'):
        fill = canonical(json.loads(payload))
        if fill['trade_id'] != trade_id:
            raise ValueError('persisted trade ID disagrees with payload')
        con.execute('INSERT INTO fills_v2 VALUES(?,?,?)', (trade_id,fill['order_id'],payload))
    con.execute('DROP TABLE fills')
    con.execute('ALTER TABLE fills_v2 RENAME TO fills')
    con.execute('PRAGMA user_version=2')


def connect(path, write=False):
    path = Path(path)
    if not path.exists() and not write:
        return None
    con = sqlite3.connect(str(path) if write else path.resolve().as_uri()+'?mode=ro', uri=not write, timeout=30, isolation_level=None)
    try:
        if con.execute('PRAGMA integrity_check').fetchall() != [('ok',)]:
            raise ValueError('database integrity check failed')
        if write:
            con.execute('PRAGMA journal_mode=WAL')
            con.execute('PRAGMA synchronous=FULL')
            con.execute('BEGIN IMMEDIATE')
            version = con.execute('PRAGMA user_version').fetchone()[0]
            if version == 0:
                if con.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall():
                    raise ValueError('unknown database schema')
                for statement in DDL.split(';'):
                    if statement.strip():
                        con.execute(statement)
            else:
                validate_schema(con, version)
                if version == 1:
                    migrate_schema1(con)
            con.execute(SCAN_DDL)
            validate_schema(con, DB_SCHEMA)
            con.commit()
        else:
            validate_schema(con, con.execute('PRAGMA user_version').fetchone()[0])
        return con
    except BaseException:
        con.rollback()
        con.close()
        raise

def state(con):
    if con is None:
        return dict(cursor_ms=0, coverage_start_ms=None, complete=False, scan=None)
    row = con.execute('SELECT cursor_ms,coverage_start_ms,complete FROM sync WHERE id=1').fetchone()
    if (row is None or type(row[0]) is not int or row[0]<0
            or row[2] not in (0,1) or type(row[2]) is not int
            or (row[1] is None and (row[0]!=0 or row[2]!=0))
            or (row[1] is not None and (type(row[1]) is not int or not 0<=row[1]<=row[0]))):
        raise ValueError('invalid persisted sync state')
    scan=None
    if con.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='scan'").fetchone():
        scans=con.execute('SELECT id,start_ms,next_ms,target_ms FROM scan').fetchall()
        if len(scans)>1:
            raise ValueError('invalid persisted scan state')
        if scans:
            r=scans[0]
            if (r[0]!=1 or any(type(v) is not int or v<0 for v in r)
                    or not r[1]<r[2]<=r[3] or row[2]!=0 or row[0]<r[2]):
                raise ValueError('invalid persisted scan state')
            scan=dict(start_ms=r[1],next_ms=r[2],target_ms=r[3])
    return dict(cursor_ms=row[0], coverage_start_ms=row[1], complete=bool(row[2]),scan=scan)

def integer(value, minimum=0):
    if type(value) is not int or value < minimum or value > 9223372036854775807:
        raise ValueError('invalid integer')
    return value

def canonical(fill):
    out = {k:integer(fill[k], 1) for k in ('trade_id','order_id','mts')}
    if fill['symbol'] != 'tBTCUSD':
        raise ValueError('unsupported symbol')
    out['symbol'] = fill['symbol']
    for k in ('exec_amount','exec_price','fee'):
        out[k] = number(decimal(fill[k]))
    if decimal(out['exec_amount']) == 0 or decimal(out['exec_price']) <= 0:
        raise ValueError('invalid execution')
    currency = fill['fee_currency']
    if not isinstance(currency,str) or len(currency)>32:
        raise ValueError('invalid fee currency')
    out['fee_currency'] = currency.upper()
    cid = fill.get('cid')
    if cid is not None and not isinstance(cid,str):
        raise ValueError('cid must be string or null')
    out['cid'] = cid
    return out

def ingest(con, batch):
    fills = [canonical(f) for f in batch['fills']]
    sync = batch['sync']
    start, end = integer(sync['start_ms']), integer(sync['end_ms'])
    if start>end or type(sync['complete']) is not bool:
        raise ValueError('invalid coverage')
    if any(not start<=f['mts']<=end for f in fills):
        raise ValueError('fill outside scanned interval')
    con.execute('BEGIN IMMEDIATE')
    try:
        old = state(con)
        scan=batch.get('scan')
        if old['scan'] is not None and scan is None:
            raise ValueError('active scan requires continuation')
        if scan is not None:
            scan={k:integer(scan[k]) for k in ('start_ms','next_ms','target_ms')} | {'done':scan['done']}
            if (type(scan['done']) is not bool or not scan['start_ms']<=start<=scan['next_ms']<=scan['target_ms']
                    or (scan['done'] and scan['next_ms']!=scan['target_ms'])
                    or (not scan['done'] and scan['next_ms']<=start)
                    or sync['complete']!=scan['done']
                    or end!=max(old['cursor_ms'],scan['next_ms'])
                    or any(f['mts']>scan['next_ms'] for f in fills)):
                raise ValueError('invalid scan page')
            if old['scan'] is None:
                if scan['start_ms']!=start:
                    raise ValueError('invalid scan start')
            elif (start!=old['scan']['next_ms'] or scan['start_ms']!=old['scan']['start_ms'] or scan['target_ms']!=old['scan']['target_ms']):
                raise ValueError('noncontiguous scan continuation')
        if end < old['cursor_ms']:
            raise ValueError('regressive history cursor')
        if old['coverage_start_ms'] is not None and (start>old['cursor_ms']+1 or end<old['coverage_start_ms']-1):
            raise ValueError('disconnected history coverage')
        for f in fills:
            payload = json.dumps(f, sort_keys=True, separators=(',',':'))
            row = con.execute('SELECT payload FROM fills WHERE trade_id=? AND order_id=?',(f['trade_id'],f['order_id'])).fetchone()
            if row and row[0]!=payload:
                raise ValueError('conflicting authenticated trade/order ID '+str((f['trade_id'],f['order_id'])))
            con.execute('INSERT OR IGNORE INTO fills VALUES(?,?,?)',(f['trade_id'],f['order_id'],payload))
        coverage = min(start,old['coverage_start_ms']) if old['coverage_start_ms'] is not None else start
        complete = sync['complete'] if scan is not None else (sync['complete'] or (old['complete'] and end==old['cursor_ms'] and coverage==old['coverage_start_ms']))
        if scan is not None:
            if scan['done']:
                con.execute('DELETE FROM scan')
            else:
                con.execute('INSERT OR REPLACE INTO scan VALUES(1,?,?,?)',(scan['start_ms'],scan['next_ms'],scan['target_ms']))
        con.execute('UPDATE sync SET cursor_ms=?,coverage_start_ms=?,complete=? WHERE id=1',(end,coverage,int(complete)))
        con.commit()
    except BaseException:
        con.rollback()
        raise

def snapshot(con, now=None):
    now = now or dt.datetime.now(dt.timezone.utc)
    if now.tzinfo is None:
        raise ValueError('--now needs timezone')
    with localcontext() as ctx:
        ctx.prec = 256
        return _snapshot(con, now)

def _snapshot(con, now):
    fills = [] if con is None else [json.loads(r[0]) for r in con.execute('SELECT payload FROM fills')]
    fills.sort(key=lambda f:(f['mts'],f['trade_id'],decimal(f['exec_amount']) < 0,f['order_id']))
    period=reporting_period(con)
    result=_projection(con,now,fills,period)
    epoch=trading_epoch(con)
    result['operational']=None
    if epoch:
        operational_fills=[f for f in fills if f['mts']>=epoch['start_ms']]
        scoped=_projection(con,now,operational_fills,dict(period) if period else None)
        if scoped['sync']['cursor_ms']<epoch['start_ms']:
            scoped['issues'].append('epoch_history_coverage_incomplete')
            scoped['status']='incomplete'
            for total in (scoped['daily'],scoped['lifetime']):
                for key in ('gross_pnl_usd','net_pnl_usd','fees_usd'):
                    total[key]=None
        operational={key:scoped[key] for key in ('status','issues','sync','daily','lifetime','open_lots','orders','fill_count')}
        operational.update(epoch,scope='operational:tBTCUSD:excludes_opening_reserve')
        result['operational']=operational
        # Reporting may adopt this basis only if no executions fall into the
        # gap excluded by the operational epoch. The original boundary persists.
        if period and not any(period['start_ms']<=f['mts']<epoch['start_ms'] for f in fills):
            candidate=scoped['active_period']
            if operational['status']!='complete':
                candidate['issues']=sorted(set(candidate['issues']+operational['issues']))
                candidate['status']='incomplete'
                for key in ('gross_pnl_usd','net_pnl_usd','fees_usd','closed_count','win_count','loss_count'):
                    candidate[key]=None
            result['active_period']=candidate
    return result


def _projection(con, now, fills, period):
    # Both scopes use the same FIFO implementation with explicitly selected fills.
    sync = state(con)
    period = dict(period) if period else None
    period_issues = []
    issues = [] if sync['complete'] else ['history_sync_incomplete']
    if sync['coverage_start_ms'] != 0:
        issues.append('partial_history_coverage')
    if sync['cursor_ms'] > int(now.timestamp()*1000):
        issues.append('history_sync_future')
    if int(now.timestamp()*1000)-sync['cursor_ms']>120000:
        issues.append('history_sync_stale')
    period_orders = {f['order_id'] for f in fills if period and f['mts']>=period['start_ms']}
    orders = {}
    for f in fills:
        qty, price, fee = (decimal(f[k]) for k in ('exec_amount','exec_price','fee'))
        oid = f['order_id']
        order = orders.setdefault(oid, dict(order_id=oid,cid=f.get('cid'),exec_amount=Decimal(0),base_fee=Decimal(0),cost=Decimal(0),volume=Decimal(0),mts=f['mts']))
        if order['cid'] != f.get('cid') or order['exec_amount'] * qty < 0:
            issues.append('ambiguous_order:'+str(oid))
            if oid in period_orders:
                period_issues.append('ambiguous_order:'+str(oid))
        order['exec_amount'] += qty
        order['base_fee'] += fee if f['fee_currency']=='BTC' else Decimal(0)
        order['cost'] += abs(qty)*price
        order['volume'] += abs(qty)
    order_views=[]
    for order in orders.values():
        order_views.append(dict(order_id=order['order_id'],cid=order['cid'],mts=order['mts'],
            exec_amount=number(order['exec_amount']),base_fee=number(order['base_fee']),
            entry_price=number(order['cost']/order['volume'])))
    lots=[]
    today=now.astimezone(ZoneInfo('Europe/Prague')).date()
    def totals():
        return dict(gross_pnl_usd=Decimal(0),net_pnl_usd=Decimal(0),fees_usd=Decimal(0),closed_count=0,win_count=0,loss_count=0)
    lifetime,daily,period_totals=totals(),totals(),totals()
    period_sells=False
    def fill_issue(message, f):
        issues.append(message)
        if period and f['mts']>=period['start_ms']:
            period_issues.append(message)
    for f in fills:
        qty,price,fee=(decimal(f[k]) for k in ('exec_amount','exec_price','fee'))
        in_period=bool(period and f['mts']>=period['start_ms'])
        period_sells=period_sells or (in_period and qty<0)
        currency=f['fee_currency']
        unknown=bool(fee and currency not in ('USD','BTC')) or not currency
        if unknown:
            fill_issue('unresolved_fee:'+str(f['trade_id']), f)
        basefee=fee if currency=='BTC' else Decimal(0)
        quotefee=fee if currency=='USD' else Decimal(0)
        if qty>0:
            effective=qty+basefee
            if effective<=0:
                fill_issue('invalid_net_buy_quantity:'+str(f['trade_id']), f)
                continue
            lots.append(dict(trade_id=f['trade_id'],order_id=f['order_id'],cid=f.get('cid'),remaining_btc=effective,cost_basis_usd=qty*price-quotefee,gross_basis=effective*price,entry_price=price,mts=f['mts']))
            continue
        consumed=-qty-basefee
        if consumed<=0:
            fill_issue('invalid_net_sell_quantity:'+str(f['trade_id']), f)
            continue
        remaining=consumed
        cost=grosscost=Decimal(0)
        while remaining and lots:
            lot=lots[0]
            take=min(remaining,lot['remaining_btc'])
            allocation=lot['cost_basis_usd'] if take==lot['remaining_btc'] else lot['cost_basis_usd']*take/lot['remaining_btc']
            grossallocation=lot['gross_basis'] if take==lot['remaining_btc'] else lot['gross_basis']*take/lot['remaining_btc']
            cost+=allocation
            grosscost+=grossallocation
            lot['remaining_btc']-=take
            lot['cost_basis_usd']-=allocation
            lot['gross_basis']-=grossallocation
            remaining-=take
            if not lot['remaining_btc']:
                lots.pop(0)
        if remaining:
            fill_issue('unmatched_sell_cost:'+str(f['trade_id']), f)
            continue
        net=-qty*price+quotefee-cost
        gross=consumed*price-grosscost
        trade_day=(dt.datetime(1970,1,1,tzinfo=dt.timezone.utc)+dt.timedelta(milliseconds=f['mts'])).astimezone(ZoneInfo('Europe/Prague')).date()
        selected_totals=([lifetime,daily] if trade_day==today else [lifetime])
        if in_period:
            selected_totals.append(period_totals)
        for total in selected_totals:
            total['gross_pnl_usd']+=gross
            total['net_pnl_usd']+=net
            total['fees_usd']+=gross-net
            total['closed_count']+=1
            total['win_count']+=int(net>0)
            total['loss_count']+=int(net<0)
    for total in (daily,lifetime):
        for key in ('gross_pnl_usd','net_pnl_usd','fees_usd'):
            total[key]=None if issues else number(total[key])
    if period:
        # A reporting boundary is not inventory cost basis. Once a sell occurs,
        # unresolved historical accounting also makes its FIFO result unknown.
        if period_sells:
            period_issues.extend(issues)
        else:
            period_issues.extend(i for i in issues if i.startswith('history_sync_'))
        if (sync['coverage_start_ms'] != 0
                or sync['cursor_ms']<period['start_ms']):
            period_issues.append('period_history_coverage_incomplete')
        period.update(status='incomplete' if period_issues else 'complete',issues=sorted(set(period_issues)))
        for key,value in period_totals.items():
            period[key]=None if period_issues else (number(value) if isinstance(value,Decimal) else value)
    legacy=dict(record_count=0,unverified_count=0,shadow_count=0,duplicate_count=0)
    if con:
        for row in con.execute('SELECT records FROM legacy'):
            for record in json.loads(row[0]):
                legacy['record_count']+=1
                classification = record if isinstance(record,str) else record['classification']
                legacy[classification+'_count']+=1
    return dict(active_period=period,schema_version=SCHEMA,source='authenticated_bitfinex_fills',scope='account:tBTCUSD',generated_at_ms=int(now.timestamp()*1000),sync=sync,status='incomplete' if issues else 'complete',issues=sorted(set(issues)),daily=daily,lifetime=lifetime,open_lots=[{k:number(v) if isinstance(v,Decimal) else v for k,v in lot.items() if k!='gross_basis'} for lot in lots],fill_count=len(fills),legacy=legacy,orders=order_views)

def import_legacy(con,path):
    raw=Path(path).read_bytes()
    classifications=[]
    seen=set()
    decoder=json.JSONDecoder()
    text=raw.decode('utf-8',errors='replace')
    position=0
    while position<len(text):
        if text[position].isspace():
            position+=1
            continue
        offset=position
        try:
            record,position=decoder.raw_decode(text,position)
            normalized=json.dumps(record,sort_keys=True)
            cid=str(record.get('cid','')).lower() if isinstance(record,dict) else ''
            shadow=isinstance(record,dict) and (record.get('shadow') is True or record.get('mode')=='shadow' or record.get('source')=='shadow' or cid.startswith('shadow'))
            diagnostic='shadow' if shadow else ('rebalance' if cid.startswith('rebalance') else 'unverified')
            kind='duplicate' if normalized in seen else ('shadow' if shadow else 'unverified')
            seen.add(normalized)
            classifications.append(dict(classification=kind,diagnostic=diagnostic,record=record,offset=offset))
        except (ValueError,TypeError):
            # Preserve malformed material separately, resuming at the next line.
            end=text.find('\n',position)
            position=len(text) if end<0 else end+1
            classifications.append(dict(classification='unverified',diagnostic='malformed',raw_text=text[offset:position],offset=offset))
    con.execute('BEGIN IMMEDIATE')
    try:
        con.execute('INSERT OR IGNORE INTO legacy VALUES(?,?,?,?)',(hashlib.sha256(raw).hexdigest(),str(path),raw,json.dumps(classifications)))
        con.commit()
    except BaseException:
        con.rollback()
        raise

def backup(con,path):
    if con is None:
        raise ValueError('database does not exist')
    target=Path(path)
    # Exclusive destination prevents overwriting an active database or prior backup.
    fd=os.open(target,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600)
    os.close(fd)
    destination=None
    try:
        destination=sqlite3.connect(target)
        con.backup(destination)
        if destination.execute('PRAGMA integrity_check').fetchall()!=[('ok',)]:
            raise ValueError('backup integrity failure')
        destination.close()
        destination=None
        with target.open('rb') as handle:
            os.fsync(handle.fileno())
        fd=os.open(target.parent,os.O_RDONLY)
        try: os.fsync(fd)
        finally: os.close(fd)
    except BaseException:
        if destination: destination.close()
        target.unlink(missing_ok=True)
        raise

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--db',default='/var/lib/pirana/accounting.sqlite3')
    commands=parser.add_subparsers(dest='command',required=True)
    commands.add_parser('ingest')
    report=commands.add_parser('report')
    report.add_argument('--now')
    commands.add_parser('state')
    period=commands.add_parser('start-period')
    period.add_argument('--start-ms',type=int,required=True)
    period.add_argument('--id',required=True)
    epoch=commands.add_parser('start-trading-epoch')
    epoch.add_argument('--start-ms',type=int,required=True)
    epoch.add_argument('--id',required=True)
    epoch.add_argument('--reserved-btc',required=True)
    commands.add_parser('import-legacy').add_argument('path')
    commands.add_parser('backup').add_argument('path')
    args=parser.parse_args()
    con=None
    try:
        if args.command=='start-period':
            validate_period(args.id,args.start_ms)
            if args.start_ms>int(dt.datetime.now(dt.timezone.utc).timestamp()*1000):
                raise ValueError('reporting period cannot start in the future')
        con=connect(args.db,args.command in ('ingest','import-legacy','start-period','start-trading-epoch'))
        if args.command=='ingest': ingest(con,json.load(sys.stdin))
        elif args.command=='import-legacy': import_legacy(con,args.path)
        elif args.command=='start-period': start_period(con,args.id,args.start_ms)
        elif args.command=='start-trading-epoch': start_trading_epoch(con,args.id,args.start_ms,args.reserved_btc)
        elif args.command=='backup': backup(con,args.path)
        if con: con.execute('BEGIN')  # coherent projection across concurrent writers
        result=state(con) if args.command=='state' else snapshot(con,dt.datetime.fromisoformat(args.now.replace('Z','+00:00')) if getattr(args,'now',None) else None)
        print(json.dumps(result,sort_keys=True))
        return 0
    except (ValueError,KeyError,TypeError,sqlite3.Error,OSError,ArithmeticError) as exc:
        print('accounting error: '+str(exc),file=sys.stderr)
        return 1
    finally:
        if con: con.close()

if __name__=='__main__':
    sys.exit(main())
