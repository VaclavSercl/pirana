import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

SCRIPT=Path(__file__).resolve().parents[1]/'scripts/pirana_accounting.py'
spec=importlib.util.spec_from_file_location('accounting',SCRIPT)
a=importlib.util.module_from_spec(spec)
spec.loader.exec_module(a)
T=1789682400000
NOW=dt.datetime.fromtimestamp(T/1000,dt.timezone.utc)

def fill(tid,qty,price,fee='0',currency='USD',mts=None):
    return dict(trade_id=tid,order_id=tid+100,symbol='tBTCUSD',mts=mts or T-100+tid,exec_amount=qty,exec_price=price,fee=fee,fee_currency=currency,cid=None)

def batch(fills,start=0,end=T,complete=True):
    return dict(fills=fills,sync=dict(start_ms=start,end_ms=end,complete=complete))

class LedgerTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory()
        self.path=Path(self.tmp.name)/'ledger.sqlite3'
        self.con=a.connect(self.path,True)
    def tearDown(self):
        self.con.close()
        self.tmp.cleanup()
    def report(self): return a.snapshot(self.con,NOW)
    def ingest(self,fills): a.ingest(self.con,batch(fills))
    def test_period_zero_preserves_historical_unknown_and_delayed_old_fill(self):
        self.ingest([fill(1,'-1','100',mts=T-1000)])
        old=self.report()
        a.start_period(self.con,'new',T-50)
        r=self.report()
        self.assertEqual(r['active_period']['net_pnl_usd'],'0')
        self.assertEqual(r['active_period']['closed_count'],0)
        self.assertEqual(r['active_period']['status'],'complete')
        for key in ('lifetime','daily','issues','status','sync'):
            self.assertEqual(r[key],old[key])
        self.ingest([fill(2,'-1','100','-1','ETH',mts=T-900)])
        self.assertEqual(self.report()['active_period'],r['active_period'])
        self.assertIsNone(self.report()['lifetime']['net_pnl_usd'])

    def test_period_valid_fifo_fees_and_boundary(self):
        self.ingest([fill(1,'1','100','-1',mts=T-200),fill(2,'-1','110','-1',mts=T-100),
                     fill(3,'1','100','-2',mts=T-60),fill(4,'-1','120','-3',mts=T-50)])
        a.start_period(self.con,'new',T-50)
        p=self.report()['active_period']
        self.assertEqual((p['gross_pnl_usd'],p['net_pnl_usd'],p['fees_usd']),('20','15','5'))
        self.assertEqual((p['closed_count'],p['win_count'],p['loss_count']),(1,1,0))
        self.assertEqual(self.report()['lifetime']['net_pnl_usd'],'23')

    def test_period_new_executions_recompute_after_reset(self):
        self.ingest([])
        a.start_period(self.con,'new',T-50)
        self.assertEqual(self.report()['active_period']['net_pnl_usd'],'0')
        self.ingest([fill(1,'1','100','-1',mts=T-40)])
        self.assertEqual(self.report()['active_period']['net_pnl_usd'],'0')
        self.ingest([fill(2,'-1','110','-2',mts=T-30)])
        p=self.report()['active_period']
        self.assertEqual(p['status'],'complete')
        self.assertEqual((p['gross_pnl_usd'],p['net_pnl_usd'],p['fees_usd']),('10','7','3'))
        self.assertEqual(p['closed_count'],1)

    def test_period_new_missing_cost_and_unknown_fee_never_zero(self):
        a.start_period(self.con,'new',T-50)
        self.ingest([fill(1,'-1','120',mts=T-50)])
        p=self.report()['active_period']
        self.assertEqual(p['status'],'incomplete')
        self.assertIsNone(p['net_pnl_usd'])
        self.assertIsNone(p['closed_count'])
        self.assertIn('unmatched_sell_cost:1',p['issues'])

    def test_period_historical_problem_blocks_later_sell_even_with_new_buy(self):
        self.ingest([fill(1,'-1','100',mts=T-200),fill(2,'1','100',mts=T-40),fill(3,'-1','120',mts=T-30)])
        a.start_period(self.con,'new',T-50)
        self.assertEqual(self.report()['active_period']['status'],'incomplete')
        self.assertIsNone(self.report()['active_period']['net_pnl_usd'])

    def test_period_new_buy_unknown_fee_and_invalid_quantity(self):
        a.start_period(self.con,'new',T-50)
        self.ingest([fill(1,'1','100','-1','ETH',mts=T-40)])
        self.assertIn('unresolved_fee:1',self.report()['active_period']['issues'])
        self.assertIsNone(self.report()['active_period']['net_pnl_usd'])
        self.ingest([fill(2,'1','100','-2','BTC',mts=T-30)])
        self.assertIn('invalid_net_buy_quantity:2',self.report()['active_period']['issues'])

    def test_period_zero_requires_fresh_complete_covering_scan(self):
        a.start_period(self.con,'new',T-50)
        self.assertEqual(self.report()['active_period']['status'],'incomplete')
        a.ingest(self.con,batch([],start=T-40))
        self.assertIn('period_history_coverage_incomplete',self.report()['active_period']['issues'])
        a.ingest(self.con,batch([],start=T-50))
        self.assertEqual(self.report()['active_period']['status'],'incomplete')
        a.ingest(self.con,batch([],start=0))
        self.assertEqual(self.report()['active_period']['status'],'complete')
        self.assertEqual(self.report()['status'],'complete')
        self.assertEqual(a.snapshot(self.con,NOW+dt.timedelta(minutes=3))['active_period']['status'],'incomplete')
        a.ingest(self.con,batch([],start=T,end=T+1,complete=False))
        self.assertEqual(self.report()['active_period']['status'],'incomplete')

    def test_period_replay_restart_backup_and_reject_other_reset(self):
        self.ingest([fill(1,'1','100')])
        legacy=Path(self.tmp.name)/'legacy.json';legacy.write_text('{"pnl":123}')
        a.import_legacy(self.con,legacy)
        saved={table:self.con.execute('SELECT * FROM '+table).fetchall() for table in ('fills','sync','legacy','scan')}
        a.start_period(self.con,'new',T-50)
        expected=self.report()
        a.start_period(self.con,'new',T-50)
        for ident,start in [('new',T-49),('different',T-50)]:
            with self.assertRaisesRegex(ValueError,'already established'):
                a.start_period(self.con,ident,start)
        self.assertEqual(self.report(),expected)
        for table,rows in saved.items():
            self.assertEqual(self.con.execute('SELECT * FROM '+table).fetchall(),rows)
        target=Path(self.tmp.name)/'backup.sqlite3'
        a.backup(self.con,target)
        self.con.close();self.con=a.connect(self.path)
        self.assertEqual(self.report(),expected)
        restored=a.connect(target)
        self.assertEqual(a.snapshot(restored,NOW),expected)
        restored.close()

    def test_period_optional_schema_and_cli_replay(self):
        self.assertIsNone(self.report()['active_period'])
        cmd=[sys.executable,str(SCRIPT),'--db',str(self.path),'start-period','--start-ms','1','--id','new']
        for _ in range(2):
            result=subprocess.run(cmd,capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(json.loads(result.stdout)['active_period']['start_ms'],1)
        changed=subprocess.run(cmd[:-1]+['other'],capture_output=True,text=True)
        self.assertNotEqual(changed.returncode,0)
        self.assertEqual(self.con.execute('PRAGMA user_version').fetchone()[0],2)

    def test_period_cli_rejects_future_and_invalid_id_without_mutation(self):
        cmd=[sys.executable,str(SCRIPT),'--db',str(self.path),'start-period']
        future=int(dt.datetime.now(dt.timezone.utc).timestamp()*1000)+60000
        for start,ident in [(future,'new'),(T-50,''),(T-50,' '*3),(T-50,'a'*129)]:
            result=subprocess.run(cmd+['--start-ms',str(start),'--id',ident],capture_output=True,text=True)
            self.assertNotEqual(result.returncode,0)
            self.assertIsNone(self.report()['active_period'])

    def test_orders_preserve_non_fifo_strategy_attribution(self):
        a_fill=dict(fill(1,'1','100'),cid='11')
        b_fill=dict(fill(2,'1','200','-0.01','BTC'),cid='22')
        sell_b=dict(fill(3,'-0.99','300'),cid='33')
        self.ingest([a_fill,b_fill,sell_b])
        report=self.report()
        self.assertEqual(len(report['orders']),3)
        self.assertEqual(report['orders'][1]['exec_amount'],'1')
        self.assertEqual(report['orders'][1]['base_fee'],'-0.01')
        self.assertEqual(report['orders'][2]['cid'],'33')
        self.assertEqual(report['orders'][2]['exec_amount'],'-0.99')
        self.assertEqual(sum(a.Decimal(l['remaining_btc']) for l in report['open_lots']),a.Decimal('1'))

    def test_fifo_partial_multiple_and_restart(self):
        self.ingest([fill(1,'2','100','-2'),fill(2,'1','200','-1'),fill(3,'-2.5','300','-3')])
        r=self.report()
        self.assertEqual(r['lifetime']['gross_pnl_usd'],'450')
        self.assertEqual(r['lifetime']['net_pnl_usd'],'444.5')
        self.assertEqual(r['lifetime']['fees_usd'],'5.5')
        self.assertEqual(r['open_lots'][0]['cost_basis_usd'],'100.5')
        self.con.close(); self.con=a.connect(self.path)
        self.assertEqual(self.report(),r)
    def test_base_fee_and_rebate(self):
        self.ingest([fill(1,'1','100','-0.1','BTC'),fill(2,'-0.8','200','-0.1','BTC')])
        r=self.report()
        self.assertEqual(r['open_lots'],[])
        self.assertEqual(r['lifetime']['gross_pnl_usd'],'90')
        self.assertEqual(r['lifetime']['net_pnl_usd'],'60')
        self.assertEqual(r['lifetime']['fees_usd'],'30')
    def test_quote_rebates(self):
        self.ingest([fill(1,'1','100','1'),fill(2,'-1','110','2')])
        self.assertEqual(self.report()['lifetime']['net_pnl_usd'],'13')
        self.assertEqual(self.report()['lifetime']['fees_usd'],'-3')
    def test_partial_base_fee_full_residual(self):
        self.ingest([fill(1,'1','100','-0.1','BTC'),fill(2,'-0.3','110'),fill(3,'-0.6','110')])
        self.assertEqual(self.report()['open_lots'],[])
        self.assertEqual(a.Decimal(self.report()['lifetime']['net_pnl_usd']),a.Decimal('-1'))
    def test_unknown_and_unmatched(self):
        self.ingest([fill(1,'-1','100'),fill(2,'1','100','-1','ETH')])
        r=self.report()
        self.assertIsNone(r['lifetime']['net_pnl_usd'])
        self.assertIn('unmatched_sell_cost:1',r['issues'])
        self.assertIn('unresolved_fee:2',r['issues'])
    def test_replay_and_conflict_atomic(self):
        f=fill(1,'1','100')
        self.ingest([f]); self.ingest([f]); self.ingest([dict(f,exec_amount='1.00',exec_price='100.0')])
        self.assertEqual(self.report()['fill_count'],1)
        before=a.state(self.con)
        with self.assertRaises(ValueError):
            a.ingest(self.con,batch([fill(2,'1','100'),fill(1,'1','101')],end=T+100))
        self.assertEqual(a.state(self.con),before)
        self.assertEqual(self.report()['fill_count'],1)
    def test_sync_gaps_and_staleness(self):
        self.ingest([])
        with self.assertRaises(ValueError): a.ingest(self.con,batch([],start=T+2,end=T+5))
        self.assertIn('history_sync_stale',a.snapshot(self.con,NOW+dt.timedelta(minutes=3))['issues'])
        a.ingest(self.con,batch([],start=T,end=T+1,complete=False))
        self.assertIsNone(self.report()['lifetime']['net_pnl_usd'])
    def test_prague_calendar(self):
        midnight=dt.datetime(2026,9,17,22,0,tzinfo=dt.timezone.utc)
        stamp=int(midnight.timestamp()*1000)
        a.ingest(self.con,batch([fill(1,'2','100',mts=stamp-100),fill(2,'-1','110',mts=stamp-1),fill(3,'-1','120',mts=stamp)],end=stamp))
        r=a.snapshot(self.con,midnight)
        self.assertEqual(r['lifetime']['net_pnl_usd'],'30')
        self.assertEqual(r['daily']['net_pnl_usd'],'20')
    def test_legacy_raw_idempotent_quarantine(self):
        path=Path(self.tmp.name)/'old.jsonl'
        raw=b'{"pnl":100}\n{"pnl":100}\n{"shadow":true}\nbad\n'
        path.write_bytes(raw)
        a.import_legacy(self.con,path); a.import_legacy(self.con,path)
        self.assertEqual(self.con.execute('SELECT raw FROM legacy').fetchone()[0],raw)
        r=self.report()
        self.assertEqual(r['fill_count'],0)
        self.assertEqual(r['legacy'],dict(record_count=4,unverified_count=2,shadow_count=1,duplicate_count=1))
    def test_backup_recovery_and_corruption(self):
        self.ingest([fill(1,'1','100')])
        target=Path(self.tmp.name)/'backup.sqlite3'
        a.backup(self.con,target)
        with self.assertRaises(FileExistsError): a.backup(self.con,target)
        restored=a.connect(target)
        self.assertEqual(a.snapshot(restored,NOW),self.report())
        restored.close()
        broken=Path(self.tmp.name)/'corrupt.sqlite3'; broken.write_bytes(b'not sqlite')
        with self.assertRaises(sqlite3.DatabaseError): a.connect(broken)
    def test_missing_read_no_mutation_and_schema(self):
        missing=Path(self.tmp.name)/'missing'
        self.assertIsNone(a.connect(missing))
        self.assertFalse(missing.exists())
        self.assertEqual(a.state(None),dict(cursor_ms=0,coverage_start_ms=None,complete=False,scan=None))
        self.con.execute('PRAGMA user_version=99')
        with self.assertRaises(ValueError): a.connect(self.path)
    def test_concurrent_writers(self):
        content=json.dumps(batch([fill(1,'1','100')]))
        procs=[subprocess.Popen([sys.executable,str(SCRIPT),'--db',str(self.path),'ingest'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True) for _ in range(4)]
        for p in procs: p.stdin.write(content); p.stdin.close(); p.stdin=None
        for p in procs:
            out,err=p.communicate(timeout=40)
            self.assertEqual(p.returncode,0,err)
            self.assertEqual(json.loads(out)['fill_count'],1)
        self.assertEqual(self.report()['fill_count'],1)
    def test_kill_before_and_after_commit(self):
        # Kill a separate actual SQLite writer after it signals the precise boundary.
        code='''import sqlite3,sys,time
c=sqlite3.connect(sys.argv[1],isolation_level=None)
c.execute('PRAGMA synchronous=FULL')
c.execute('BEGIN IMMEDIATE')
c.execute('INSERT INTO fills VALUES(?,?,?)',(1,101,sys.argv[2]))
c.execute('UPDATE sync SET cursor_ms=?,coverage_start_ms=0,complete=1',(int(sys.argv[3]),))
if sys.argv[4]=='commit': c.commit()
print('boundary',flush=True)
time.sleep(60)
'''
        payload=json.dumps(a.canonical(fill(1,'1','100')),sort_keys=True,separators=(',',':'))
        for mode,expected in [('uncommitted',0),('commit',1)]:
            p=subprocess.Popen([sys.executable,'-c',code,str(self.path),payload,str(T),mode],stdout=subprocess.PIPE,text=True)
            self.assertEqual(p.stdout.readline().strip(),'boundary')
            p.kill(); p.wait(timeout=5); p.stdout.close()
            self.con.close(); self.con=a.connect(self.path,True)
            self.assertEqual(self.report()['fill_count'],expected)
            self.assertEqual(a.state(self.con)['cursor_ms'],T if expected else 0)
        self.ingest([fill(1,'1','100')])
        self.assertEqual(self.report()['fill_count'],1)
    def test_partial_history_cannot_claim_complete(self):
        a.ingest(self.con,batch([],start=T-100,end=T,complete=True))
        r=self.report()
        self.assertEqual(r['status'],'incomplete')
        self.assertIn('partial_history_coverage',r['issues'])
        self.assertIsNone(r['lifetime']['net_pnl_usd'])
    def test_persisted_sync_invariants(self):
        for cursor,start,complete in [(10,11,1),(10,0,7),(-1,0,1),(10,None,1),(0,None,1)]:
            self.con.execute('UPDATE sync SET cursor_ms=?,coverage_start_ms=?,complete=?',(cursor,start,complete))
            with self.assertRaisesRegex(ValueError,'persisted sync'):
                a.connect(self.path)
        self.con.execute('UPDATE sync SET cursor_ms=0,coverage_start_ms=NULL,complete=0')
    def test_regressive_cursor_and_completed_replay(self):
        self.ingest([])
        with self.assertRaisesRegex(ValueError,'regressive'):
            a.ingest(self.con,batch([],end=T-1,complete=False))
        a.ingest(self.con,batch([],end=T,complete=False))
        self.assertTrue(a.state(self.con)['complete'])
        self.assertEqual(a.state(self.con)['cursor_ms'],T)
    def test_future_cursor_is_not_fresh(self):
        a.ingest(self.con,batch([],end=T+1))
        self.assertIn('history_sync_future',self.report()['issues'])
        self.assertIsNone(self.report()['lifetime']['net_pnl_usd'])
    def test_legacy_concatenations_and_cid_diagnostics(self):
        path=Path(self.tmp.name)/'concatenated.jsonl'
        raw=b'{"cid":"shadow-123"}{"cid":"rebalance-123"}\n{"x":1}{"x":1}\n'
        path.write_bytes(raw)
        a.import_legacy(self.con,path)
        saved,records=self.con.execute('SELECT raw,records FROM legacy').fetchone()
        self.assertEqual(saved,raw)
        records=json.loads(records)
        self.assertEqual([r['classification'] for r in records],['shadow','unverified','unverified','duplicate'])
        self.assertEqual(records[1]['diagnostic'],'rebalance')
        self.assertEqual(self.report()['legacy']['record_count'],4)
        self.assertEqual(self.report()['fill_count'],0)
    def test_dense_overlap_scan_resumes_beyond_four_pages(self):
        a.ingest(self.con,batch([],end=1000))
        for page in range(7):
            start=100+page*100
            request=batch([fill(page+1,'1','100',mts=start)],start=start,end=1000,complete=False)
            request['scan']=dict(start_ms=100,next_ms=start+100,target_ms=1100,done=False)
            a.ingest(self.con,request)
            self.con.close(); self.con=a.connect(self.path,True)
            self.assertEqual(a.state(self.con)['scan']['next_ms'],start+100)
            self.assertFalse(a.state(self.con)['complete'])
        last=batch([],start=800,end=1100,complete=True)
        last['scan']=dict(start_ms=100,next_ms=1100,target_ms=1100,done=True)
        a.ingest(self.con,last)
        self.assertEqual(a.state(self.con)['cursor_ms'],1100)
        self.assertIsNone(a.state(self.con)['scan'])
        self.assertTrue(a.state(self.con)['complete'])
        self.assertEqual(self.report()['fill_count'],7)
    def test_scan_conflict_and_invalid_continuation_rollback(self):
        a.ingest(self.con,batch([fill(1,'1','100',mts=150)],end=1000))
        request=batch([],start=100,end=1000,complete=False)
        request['scan']=dict(start_ms=100,next_ms=200,target_ms=1100,done=False)
        a.ingest(self.con,request)
        before=a.state(self.con)
        request=batch([fill(1,'1','999',mts=250)],start=200,end=1000,complete=False)
        request['scan']=dict(start_ms=100,next_ms=300,target_ms=1100,done=False)
        with self.assertRaisesRegex(ValueError,'conflicting'): a.ingest(self.con,request)
        self.assertEqual(a.state(self.con),before)
        for altered in [dict(start_ms=101,next_ms=300,target_ms=1100,done=False),dict(start_ms=100,next_ms=300,target_ms=1101,done=False)]:
            request['fills']=[]; request['scan']=altered
            with self.assertRaisesRegex(ValueError,'noncontiguous'): a.ingest(self.con,request)
            self.assertEqual(a.state(self.con),before)
    def test_scan_inclusive_boundary_and_final_single_timestamp(self):
        boundary=fill(1,'1','100',mts=200)
        request=batch([boundary],start=0,end=200,complete=False)
        request['scan']=dict(start_ms=0,next_ms=200,target_ms=200,done=False)
        a.ingest(self.con,request)
        request=batch([boundary],start=200,end=200,complete=True)
        request['scan']=dict(start_ms=0,next_ms=200,target_ms=200,done=True)
        a.ingest(self.con,request)
        self.assertIsNone(a.state(self.con)['scan'])
        self.assertEqual(self.report()['fill_count'],1)
    def test_schema1_optional_scan_migration_and_cid(self):
        self.con.execute('DROP TABLE scan')
        self.con.close(); self.con=a.connect(self.path)
        self.assertIsNone(a.state(self.con)['scan'])
        self.con.close(); self.con=a.connect(self.path,True)
        self.assertEqual([r[1] for r in self.con.execute('PRAGMA table_info(scan)')],['id','start_ms','next_ms','target_ms'])
        self.ingest([dict(fill(1,'1','100'),cid='pirana-123')])
        self.assertEqual(self.report()['open_lots'][0]['cid'],'pirana-123')
    def test_selftrade_composite_identity_fees_and_replay(self):
        buy=dict(fill(1,'1','100','-1'),order_id=999,cid='buy')
        sell=dict(fill(1,'-1','100','-2'),order_id=100,cid='sell')
        # Reverse insertion and order IDs exercise explicit buy-before-sell ordering.
        self.ingest([sell,buy,sell,buy])
        self.ingest([buy,sell])
        report=self.report()
        self.assertEqual(report['schema_version'],1)
        self.assertEqual(self.con.execute('PRAGMA user_version').fetchone()[0],2)
        self.assertEqual(report['fill_count'],2)
        self.assertEqual(report['status'],'complete')
        self.assertEqual(report['open_lots'],[])
        self.assertEqual(report['lifetime']['gross_pnl_usd'],'0')
        self.assertEqual(report['lifetime']['net_pnl_usd'],'-3')
        self.assertEqual(report['lifetime']['fees_usd'],'3')
        self.assertEqual([o['cid'] for o in report['orders']],['buy','sell'])
        before=a.state(self.con)
        with self.assertRaisesRegex(ValueError,'conflicting'):
            a.ingest(self.con,batch([fill(2,'1','100'),dict(sell,fee='-9')],end=T+1))
        self.assertEqual(self.report(),report)
        self.assertEqual(a.state(self.con),before)
        self.con.close(); self.con=a.connect(self.path)
        self.assertEqual(self.report(),report)

    def old_database(self, path, scan=False, corrupt=False):
        con=sqlite3.connect(path,isolation_level=None)
        con.executescript("""
            CREATE TABLE fills(trade_id INTEGER PRIMARY KEY,payload TEXT NOT NULL);
            CREATE TABLE sync(id INTEGER PRIMARY KEY CHECK(id=1),cursor_ms INTEGER NOT NULL,coverage_start_ms INTEGER,complete INTEGER NOT NULL);
            CREATE TABLE legacy(hash TEXT PRIMARY KEY,source TEXT NOT NULL,raw BLOB NOT NULL,records TEXT NOT NULL);
            PRAGMA user_version=1;
        """)
        payload=json.dumps(a.canonical(fill(1,'1','100')),sort_keys=True,separators=(',',':'))
        con.execute('INSERT INTO fills VALUES(?,?)',(2 if corrupt else 1,payload))
        con.execute('INSERT INTO sync VALUES(1,?,0,?)',(T,0 if scan else 1))
        con.execute('INSERT INTO legacy VALUES(?,?,?,?)',('hash','source',b'raw\x00data','["unverified"]'))
        if scan:
            con.execute(a.SCAN_DDL)
            con.execute('INSERT INTO scan VALUES(1,0,?,?)',(T-1,T+1))
        con.close()
        return payload

    def test_schema1_migration_preserves_payload_state_legacy_and_scan(self):
        for has_scan in (False,True):
            with self.subTest(scan=has_scan):
                path=Path(self.tmp.name)/('old-'+str(has_scan)+'.sqlite3')
                payload=self.old_database(path,has_scan)
                reader=a.connect(path)
                before=a.snapshot(reader,NOW)
                saved_legacy=reader.execute('SELECT * FROM legacy').fetchall()
                self.assertEqual(reader.execute('PRAGMA user_version').fetchone()[0],1)
                backup=path.with_suffix('.backup')
                a.backup(reader,backup)
                reader.close()
                writer=a.connect(path,True)
                self.assertEqual(writer.execute('PRAGMA user_version').fetchone()[0],2)
                self.assertEqual(writer.execute('SELECT * FROM fills').fetchall(),[(1,101,payload)])
                self.assertEqual(writer.execute('SELECT * FROM legacy').fetchall(),saved_legacy)
                self.assertEqual(a.snapshot(writer,NOW),before)
                self.assertEqual([r[5] for r in writer.execute('PRAGMA table_info(fills)')],[1,2,0])
                writer.close()
                reader=a.connect(backup)
                self.assertEqual(reader.execute('PRAGMA user_version').fetchone()[0],1)
                self.assertEqual(a.snapshot(reader,NOW),before)
                reader.close()
                writer=a.connect(path,True)
                self.assertEqual(a.snapshot(writer,NOW),before)
                writer.close()

    def test_schema1_failed_migration_rolls_back(self):
        path=Path(self.tmp.name)/'bad-old.sqlite3'
        payload=self.old_database(path,corrupt=True)
        with self.assertRaisesRegex(ValueError,'disagrees'):
            a.connect(path,True)
        con=sqlite3.connect(path)
        self.assertEqual(con.execute('PRAGMA user_version').fetchone()[0],1)
        self.assertEqual(con.execute('SELECT * FROM fills').fetchall(),[(2,payload)])
        self.assertIsNone(con.execute("SELECT 1 FROM sqlite_master WHERE name='fills_v2'").fetchone())
        self.assertIsNone(con.execute("SELECT 1 FROM sqlite_master WHERE name='scan'").fetchone())
        self.assertEqual(con.execute('SELECT * FROM sync').fetchall(),[(1,T,0,1)])
        self.assertEqual(con.execute('SELECT raw FROM legacy').fetchone()[0],b'raw\x00data')
        con.close()

    def test_schema2_rejects_reversed_composite_primary_key(self):
        self.con.execute('DROP TABLE fills')
        self.con.execute('CREATE TABLE fills(trade_id INTEGER,order_id INTEGER,payload TEXT,PRIMARY KEY(order_id,trade_id))')
        for write in (False,True):
            with self.assertRaisesRegex(ValueError,'invalid database schema'):
                a.connect(self.path,write)

    def test_invalid_decimal_and_out_of_coverage(self):
        for value in ('NaN','Infinity','1e999',1.2):
            with self.assertRaises((ValueError,TypeError)): a.canonical(fill(1,value,'100'))
        with self.assertRaises(ValueError): a.ingest(self.con,batch([fill(1,'1','100',mts=T+1)]))
        self.assertEqual(self.report()['fill_count'],0)

if __name__=='__main__': unittest.main()
