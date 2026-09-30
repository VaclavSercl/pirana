import copy
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import decision_dataset as d

SHA='a'*64

def row(outcome='inventory_limit'):
    return dict(trade_id=123,exchange_ms=1000,received_ms=1010,observed_ms=1020,
        price=100.,signed_quantity=.1,ofi=.2,l2=.2,composite=.2,flow=.2,flow_hwm=101.,atr=1.,
        vpin=.2,buy_vpin=.1,sell_vpin=.2,vpin_threshold=.3,best_bid=99.9,best_ask=100.1,
        raw_baseline=True,sell_cascade=False,ask_wall=False,route='live_pullback_flow',
        outcome=outcome,cid=None,quantity=None,ioc_limit=None,intent_ms=None,handoff_ms=None)

def events(records=None):
    records=records or [row()]
    header=dict(schema_version=1,event='header',session='test',part=0,
        provenance=dict(contract='pirana-entry-decision-v1',binary_sha256=SHA,replay_qualified=False))
    decisions=[dict(schema_version=1,event='decision',session='test',sequence=i+1,record=r) for i,r in enumerate(records)]
    coverage=dict(schema_version=1,event='coverage',session='test',observed_ms=2000,status='RUNNING',
        written_sequence=len(records),evaluated=len(records),candidates=len(records),lost=0,
        skipped_execution=3,skipped_cooldown=9,skipped_vpin_emergency=4,skipped_vpin_sell=1)
    return [header,*decisions,coverage]

class Decisions(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name);self.path=self.root/'decisions-test-000000.jsonl'
    def tearDown(self): self.tmp.cleanup()
    def write(self,items=None): self.path.write_text(''.join(json.dumps(x,allow_nan=False)+'\n' for x in (items or events())))
    def audit(self, fills=None): return d.audit(self.root,dict(binary_sha256=SHA),fills)
    def test_rejects_no_signal_and_shadow_are_retained_separately_from_skips(self):
        no=row('no_signal');no.update(trade_id=124,flow=0.,raw_baseline=False,route='none')
        shadow=row('shadow_only');shadow.update(trade_id=125,flow=0.,raw_baseline=False,route='shadow_candidate')
        self.write(events([row(),no,shadow]));out=io.StringIO();report=d.audit(self.root,dict(binary_sha256=SHA),output=out)
        self.assertEqual(report['rows'],3);self.assertEqual(len(out.getvalue().splitlines()),3)
        self.assertEqual(report['sessions']['test']['coverage']['skipped_cooldown'],9)
        self.assertFalse(report['training_qualified']);self.assertFalse(report['full_strategy_replay_qualified'])
    def test_sequence_loss_unknown_outcome_and_missing_header_are_blocked(self):
        for modify in (lambda x:x[1].update(sequence=2),lambda x:x[-1].update(lost=1),
            lambda x:x[1]['record'].update(outcome='evaluation_incomplete'),lambda x:x[0]['provenance'].update(binary_sha256='b'*64)):
            with self.subTest(modify=modify):
                data=events();modify(data);self.write(data)
                with self.assertRaises(ValueError): self.audit()
    def test_strict_json_truncation_and_symlink_are_not_repaired(self):
        self.path.write_bytes(b'{"schema_version":1,"schema_version":1}\n')
        with self.assertRaises(ValueError): self.audit()
        self.write();original=self.path.read_bytes();self.path.write_bytes(original[:-1])
        with self.assertRaises(ValueError): self.audit()
        self.assertEqual(self.path.read_bytes(),original[:-1])
        other=self.root/'owned';self.path.rename(other);self.path.symlink_to(other)
        with self.assertRaises(ValueError): self.audit()
    def test_false_baseline_and_clock_or_route_contradictions_block(self):
        for change in (dict(raw_baseline=False),dict(received_ms=1021),dict(sell_cascade=True),dict(price=True),dict(best_bid=101.),dict(intent_ms=1000)):
            with self.subTest(change=change):
                x=row();x.update(change);self.write(events([x]))
                with self.assertRaises(ValueError): self.audit()
    def test_handoff_is_not_a_fill_and_partial_base_fees_are_preserved(self):
        r=row('intent_handoff');r.update(cid=77,quantity=.02,ioc_limit=101.,intent_ms=1050,handoff_ms=1060)
        self.write(events([r]));report=self.audit([])
        self.assertEqual(report['execution_matches'],{'NOT_YET_MATCHED':1})
        f=dict(trade_id=1,order_id=88,cid=77,mts=1080,symbol='tBTCUSD',exec_amount='.01',exec_price='100',fee='-0.00001',fee_currency='BTC')
        out=d.execution_join(r,{'77':[f]})
        self.assertEqual(out['status'],'PARTIAL_FILL_MATCHED');self.assertEqual(out['net_btc'],'0.00999');self.assertEqual(out['acquisition_cost_usd'],'1.00')
    def test_cid_clock_fee_side_and_overfill_fail_closed(self):
        r=row('intent_handoff');r.update(cid=77,quantity=.01,ioc_limit=101.,intent_ms=1050,handoff_ms=1060)
        base=dict(trade_id=1,order_id=88,cid=77,mts=1080,symbol='tBTCUSD',exec_amount='.01',exec_price='100',fee='0',fee_currency='USD')
        for change in (dict(mts=1049),dict(fee_currency='EUR'),dict(exec_amount='-.01'),dict(exec_amount='.02')):
            with self.subTest(change=change),self.assertRaises(ValueError): d.execution_join(r,{'77':[base|change]})
        with self.assertRaises(ValueError):d.execution_join(r,{'77':[base,base|dict(order_id=89)]})
    def test_duplicate_cids_and_canonical_fills_are_not_double_counted(self):
        r=row('intent_handoff');r.update(cid=77,quantity=.01,ioc_limit=101.,intent_ms=1050,handoff_ms=1060)
        self.write(events([r,r|dict(trade_id=124)]))
        with self.assertRaises(ValueError):self.audit()
        self.write();f=dict(trade_id=1,order_id=88,cid=77)
        with self.assertRaises(ValueError):self.audit([f,f])
    def test_append_is_outside_snapshot_but_rewrite_invalidates_it(self):
        self.write();sources=[];it=d.frozen_events(self.path,sources);next(it)
        with self.path.open('a') as f:f.write(json.dumps(events()[-1])+'\n')
        list(it);self.assertGreater(sources[0]['appended_bytes_deferred'],0)
        self.write();it=d.frozen_events(self.path,[]);next(it)
        content=self.path.read_bytes();self.path.write_bytes(content.replace(b'100.0',b'101.0',1))
        with self.assertRaises(ValueError):list(it)
    def test_duplicate_market_trade_identity_blocks_training_bias(self):
        self.write(events([row(),row()]))
        with self.assertRaises(ValueError):self.audit()

    def test_missing_or_corrupt_canonical_database_is_explicitly_blocked(self):
        self.write();p=self.root/'manifest.json';value=dict(schema_version=1,verification_status='PASS',binary_sha256=SHA,
            verification_evidence_sha256='b'*64,source_commit='c'*40,source_tree='d'*40)
        p.write_text(json.dumps(value));digest=hashlib.sha256(p.read_bytes()).hexdigest()
        for kind in ('missing','corrupt'):
            with self.subTest(kind=kind):
                db=self.root/(kind+'.sqlite');output=self.root/(kind+'-out')
                if kind=='corrupt':db.write_bytes(b'not a SQLite database')
                argv=['audit','--directory',str(self.root),'--manifest',str(p),'--manifest-sha256',digest,
                      '--canonical-db',str(db),'--output',str(output)]
                with patch('sys.argv',argv),patch('sys.stderr',io.StringIO()):self.assertEqual(d.main(),2)
                self.assertEqual(json.loads((output/'BLOCKED.json').read_text())['status'],'BLOCKED')
                self.assertFalse((output/'audit.json').exists())
                if kind=='missing':self.assertFalse(db.exists())
                else:self.assertEqual(db.read_bytes(),b'not a SQLite database')

    def test_pinned_manifest_and_source_identity_required(self):
        p=self.root/'manifest.json';value=dict(schema_version=1,verification_status='PASS',binary_sha256=SHA,
            verification_evidence_sha256='b'*64,source_commit='c'*40,source_tree='d'*40)
        p.write_text(json.dumps(value));h=hashlib.sha256(p.read_bytes()).hexdigest();self.assertEqual(d.manifest(p,h),value)
        with self.assertRaises(ValueError):d.manifest(p,'f'*64)
        value['verification_status']='BLOCKED';p.write_text(json.dumps(value));h=hashlib.sha256(p.read_bytes()).hexdigest()
        with self.assertRaises(ValueError):d.manifest(p,h)
    def test_oversize_and_missing_rotated_segment_block(self):
        self.write();data=events();data[0]['part']=1;self.write(data);self.path.rename(self.root/'decisions-test-000001.jsonl')
        with self.assertRaises(ValueError):self.audit()
        (self.root/'decisions-test-000001.jsonl').unlink();self.write()
        with patch.object(d,'MAX_FILE',1),self.assertRaises(ValueError):self.audit()
    def test_empty_incomplete_or_failed_coverage_cannot_qualify(self):
        with self.assertRaises(ValueError):self.audit()
        self.write(events()[:-1])
        with self.assertRaises(ValueError):self.audit()
        data=events();data[-1]['status']='FAILED';self.write(data)
        with self.assertRaises(ValueError):self.audit()

if __name__=='__main__':unittest.main()
