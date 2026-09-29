#!/usr/bin/env python3
"""Reviewed additive accounting reconciliation. Offline evidence, no venue actions."""
import argparse
import hashlib
import json
import re
import sqlite3
from pathlib import Path
try:
    from . import pirana_accounting as a
except ImportError:
    import pirana_accounting as a

REPAIRS_DDL="CREATE TABLE IF NOT EXISTS accounting_repairs(repair_id TEXT PRIMARY KEY, manifest_sha256 TEXT NOT NULL, evidence TEXT NOT NULL)"
def encode(x): return json.dumps(x,sort_keys=True,separators=(',',':'))
def digest(x): return hashlib.sha256(encode(x).encode()).hexdigest()

def validated(manifest):
    if not isinstance(manifest,dict) or set(manifest)!={'schema_version','repair_id','authority','venue_response','restored_fill','opening_lot','classified_rows'} or type(manifest['schema_version']) is not int or manifest['schema_version']!=1:
        raise ValueError('invalid repair manifest')
    for k in ('repair_id','authority'):
        if not isinstance(manifest[k],str) or not manifest[k].strip() or len(manifest[k])>1000: raise ValueError('missing repair authority/identity')
    a.validate_opening_lot(manifest['opening_lot'])
    restored=manifest['restored_fill']
    if not isinstance(restored,dict) or set(restored)!={'trade_id','order_id','mts','symbol','exec_amount','exec_price','fee','fee_currency','cid'}:
        raise ValueError('invalid restored fill fields')
    fill=a.canonical(restored)
    if a.execution_provenance([fill])['excluded_records']: raise ValueError('synthetic execution cannot be restored as authenticated')
    venue=manifest['venue_response']
    if not isinstance(venue,list) or len(venue)!=1 or not isinstance(venue[0],list) or len(venue[0])<12:raise ValueError('expected exact authenticated venue execution')
    v=venue[0]
    for i in (0,2,3):a.integer(v[i],1)
    if not isinstance(v[1],str) or not isinstance(v[10],str) or (v[11] is not None and type(v[11]) is not int):
        raise ValueError('invalid venue execution types')
    for i in (4,5,9):
        if isinstance(v[i],bool) or not isinstance(v[i],(str,int,float)):
            raise ValueError('invalid venue numeric types')
    for k,i in [('trade_id',0),('symbol',1),('mts',2),('order_id',3),('fee_currency',10)]:
        if fill[k]!=v[i]: raise ValueError('restoration differs from venue evidence')
    for k,i in [('exec_amount',4),('exec_price',5),('fee',9)]:
        if a.decimal(fill[k])!=a.decimal(str(v[i])):raise ValueError('restoration amount differs from venue')
    if fill['cid']!=(str(v[11]) if v[11] is not None else None):raise ValueError('restoration CID differs from venue')
    rows=manifest['classified_rows']
    if not isinstance(rows,list) or not rows or len(rows)>10:raise ValueError('invalid classification set')
    for row in rows:
        if not isinstance(row,dict) or set(row)!={'trade_id','order_id','payload_sha256'}:
            raise ValueError('invalid classified row fields')
        a.integer(row['trade_id'],1);a.integer(row['order_id'],1)
        if not isinstance(row['payload_sha256'],str) or not re.fullmatch('[0-9a-f]{64}',row['payload_sha256']):
            raise ValueError('invalid classified row digest')
    if len({(x['trade_id'],x['order_id']) for x in rows})!=len(rows):raise ValueError('duplicate classification')
    return fill

def apply_repair(con, manifest):
    """Single atomic additive transaction; caller owns stopped-writer/backup gate."""
    fill=validated(manifest);mid=manifest['repair_id'];md=digest(manifest)
    con.execute('BEGIN IMMEDIATE')
    try:
        before_sync=a.state(con)
        before=dict(((r[0],r[1]),r[2]) for r in con.execute('SELECT * FROM fills'))
        con.execute(REPAIRS_DDL);con.execute(a.OPENING_LOTS_DDL);con.execute(a.PROVENANCE_DDL)
        old=con.execute('SELECT manifest_sha256 FROM accounting_repairs WHERE repair_id=?',(mid,)).fetchone()
        if old and old[0]!=md:raise ValueError('repair ID already bound to different evidence')
        for row in manifest['classified_rows']:
            key=(row['trade_id'],row['order_id']);payload=before.get(key)
            if payload is None:raise ValueError('classified evidence row missing')
            item=json.loads(payload)
            if not a.execution_provenance([item])['excluded_records'] or digest(item)!=row['payload_sha256']:
                raise ValueError('classification identity or content changed')
            values=(*key,'operator_adjustment',row['payload_sha256'],manifest['authority'])
            existing=con.execute('SELECT * FROM fill_provenance WHERE trade_id=? AND order_id=?',key).fetchone()
            if existing and tuple(existing)!=values:raise ValueError('conflicting provenance')
            con.execute('INSERT OR IGNORE INTO fill_provenance VALUES(?,?,?,?,?)',values)
        oldfill=con.execute('SELECT payload FROM fills WHERE trade_id=? AND order_id=?',(fill['trade_id'],fill['order_id'])).fetchone()
        if oldfill and a.canonical(json.loads(oldfill[0]))!=fill:raise ValueError('conflicting authenticated fill')
        con.execute('INSERT OR IGNORE INTO fills VALUES(?,?,?)',(fill['trade_id'],fill['order_id'],encode(fill)))
        lot=manifest['opening_lot'];values=(lot['adjustment_id'],lot['epoch_id'],encode(lot))
        existing=con.execute('SELECT * FROM operational_opening_lots WHERE adjustment_id=?',(lot['adjustment_id'],)).fetchone()
        if existing and tuple(existing)!=values:raise ValueError('conflicting opening lot')
        con.execute('INSERT OR IGNORE INTO operational_opening_lots VALUES(?,?,?)',values)
        epoch=a.trading_epoch(con)
        if not epoch or lot['epoch_id']!=epoch['id']:raise ValueError('opening lot requires matching active epoch')
        adjustments=a.operational_opening_lots(con,epoch)
        raw,_,provenance=a.execution_fills(con)
        _,missing=a.operational_provenance(raw,provenance,epoch,adjustments)
        if missing:raise ValueError('excluded operational identities lack typed opening lots')
        after=dict(((r[0],r[1]),r[2]) for r in con.execute('SELECT * FROM fills'))
        if (fill['trade_id'],fill['order_id']) not in after:raise ValueError('restored fill not inserted')
        if any(after.get(k)!=v for k,v in before.items()) or a.state(con)!=before_sync:raise ValueError('existing execution or cursor modified')
        con.execute('INSERT OR IGNORE INTO accounting_repairs VALUES(?,?,?)',(mid,md,encode(manifest)))
        con.commit()
        return {'status':'APPLIED' if old is None else 'ALREADY_APPLIED','repair_id':mid,'manifest_sha256':md,'existing_rows_preserved':len(before),'added_fills':len(after)-len(before),'sync_unchanged':True}
    except BaseException:
        con.rollback();raise

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--db',required=True);p.add_argument('--evidence',required=True);p.add_argument('--apply',action='store_true');p.add_argument('--backup')
    args=p.parse_args();db=Path(args.db);ev=Path(args.evidence)
    if db.is_symlink() or ev.is_symlink() or not db.is_file() or ev.stat().st_size>100000:raise ValueError('unsafe database/evidence path')
    manifest=json.loads(ev.read_text());validated(manifest)
    source=a.connect(db,False)
    try:
        if not args.apply:
            candidate=sqlite3.connect(':memory:',isolation_level=None);source.backup(candidate)
            result=apply_repair(candidate,manifest);result['status']='PREVIEW';candidate.close()
        else:
            if not args.backup:raise ValueError('apply requires new protected SQLite backup')
            backup=Path(args.backup)
            with backup.open('xb') as f:
                __import__('os').fchmod(f.fileno(),0o600)
            saved=sqlite3.connect(backup);source.backup(saved);saved.close()
            source.close();source=None
            candidate=a.connect(db,True)
            try:result=apply_repair(candidate,manifest)
            finally:candidate.close()
        print(encode(result))
    finally:
        if source is not None:source.close()
    return 0
if __name__=='__main__':raise SystemExit(main())
