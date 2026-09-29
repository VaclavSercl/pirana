import datetime as dt
import json
from decimal import Decimal
from scripts import pirana_accounting as a

NOW=1790710400000
ADJ=dict(schema_version=1,adjustment_id="reserve-activation-20260919",epoch_id="epoch",kind="operational_opening_lot",order_id=244505000001,cid="28638000000001",mts=1789840720819,quantity_btc="0.00051",reference_price_usd="81500",basis="legacy_activation_reference_not_acquisition_cost",source_reference="preserved unlock_btc_reserve.py and position journal",approval_reference="owner authorized accounting reconciliation 2026-09-29")
def fill(tid,oid,qty,price="81573",mts=1789949204162):
 return dict(trade_id=tid,order_id=oid,cid=str(oid),mts=mts,symbol="tBTCUSD",exec_amount=qty,exec_price=price,fee="0",fee_currency="USD")
def setup(tmp_path):
 c=a.connect(tmp_path/"db.sqlite3",True)
 c.execute(a.EPOCH_DDL);c.execute("INSERT INTO trading_epoch VALUES(1,'epoch',1789677493253,'0')")
 # Actual residual lot follows the activation liquidation; it is not a fabricated dust adjustment.
 rows=[fill(10,20,"0.00000453","84471",1790695479670),fill(1978882098,244588979476,"-0.00051"),dict(fill(999999999999,244505000001,"0.00051","81500",1768687293000),cid="28638000000001")]
 a.ingest(c,dict(fills=rows,sync=dict(start_ms=0,end_ms=NOW,complete=True)))
 return c

def test_typed_operational_lot_preserves_rows_and_matches_wallet(tmp_path):
 c=setup(tmp_path); before=c.execute("SELECT * FROM fills ORDER BY trade_id").fetchall()
 c.execute(a.OPENING_LOTS_DDL);c.execute("INSERT INTO operational_opening_lots VALUES(?,?,?)",(ADJ['adjustment_id'],'epoch',json.dumps(ADJ)))
 s=a.snapshot(c,dt.datetime.fromtimestamp(NOW/1000,dt.timezone.utc));o=s['operational']
 assert o['status']=='complete'
 assert sum(Decimal(x['remaining_btc']) for x in o['open_lots'])==Decimal('.00000453')
 assert o['open_lots'][0]['order_id']==20
 assert s['fill_count']==2 and o['fill_count']==2
 assert next(x for x in o['orders'] if x['order_id']==244505000001)['source']=='operational_opening_lot'
 assert o['daily']['net_pnl_usd'] is None and o['lifetime']['net_pnl_usd'] is None
 assert s['status']=='incomplete'
 assert c.execute("SELECT * FROM fills ORDER BY trade_id").fetchall()==before
 c.close()

def test_missing_typed_lot_cannot_be_replaced_by_fake_buy(tmp_path):
 c=setup(tmp_path);s=a.snapshot(c,dt.datetime.fromtimestamp(NOW/1000,dt.timezone.utc))
 assert s['operational']['status']=='incomplete'
 assert s['fill_count']==2
 c.close()

def test_opening_lot_epoch_and_duplicate_order_validation(tmp_path):
 import pytest
 c=setup(tmp_path);c.execute(a.OPENING_LOTS_DDL)
 bad=dict(ADJ,epoch_id='wrong')
 c.execute("INSERT INTO operational_opening_lots VALUES(?,?,?)",(ADJ['adjustment_id'],'epoch',json.dumps(bad)))
 with pytest.raises(ValueError): a.snapshot(c,dt.datetime.fromtimestamp(NOW/1000,dt.timezone.utc))
 c.close()


def test_additive_repair_idempotence_conflict_and_existing_rows(tmp_path):
 from scripts import pirana_accounting_repair as repair
 import pytest
 c=setup(tmp_path)
 real=a.canonical(fill(1978882098,244588979476,"-0.00051"));real['cid']='123'
 c.execute("DELETE FROM fills WHERE trade_id=1978882098") # isolated incident fixture only
 raw=[1978882098,'tBTCUSD',real['mts'],244588979476,-0.00051,81573,'EXCHANGE MARKET',81573,0,0,'USD',123]
 fake=json.loads(c.execute("SELECT payload FROM fills WHERE trade_id=999999999999").fetchone()[0])
 manifest=dict(schema_version=1,repair_id='fixture',authority='owner test authorization',venue_response=[raw],restored_fill=real,opening_lot=ADJ,classified_rows=[dict(trade_id=fake['trade_id'],order_id=fake['order_id'],payload_sha256=repair.digest(fake))])
 before=c.execute('SELECT * FROM fills ORDER BY trade_id').fetchall();sync=a.state(c)
 assert repair.apply_repair(c,manifest)['added_fills']==1
 once=c.execute('SELECT * FROM fills ORDER BY trade_id').fetchall()
 assert repair.apply_repair(c,manifest)['status']=='ALREADY_APPLIED'
 assert c.execute('SELECT * FROM fills ORDER BY trade_id').fetchall()==once
 assert all(r in once for r in before) and a.state(c)==sync
 bad=json.loads(json.dumps(manifest));bad['restored_fill']['exec_price']='1'
 with pytest.raises(ValueError):repair.apply_repair(c,bad)
 assert c.execute('SELECT * FROM fills ORDER BY trade_id').fetchall()==once
 c.close()


def repair_fixture(c):
 from scripts import pirana_accounting_repair as repair
 real=a.canonical(fill(1978882098,244588979476,"-0.00051"));real['cid']='123'
 c.execute("DELETE FROM fills WHERE trade_id=1978882098")
 fake=json.loads(c.execute("SELECT payload FROM fills WHERE trade_id=999999999999").fetchone()[0])
 return dict(schema_version=1,repair_id='strict-fixture',authority='owner fixture approval',
   venue_response=[[1978882098,'tBTCUSD',real['mts'],244588979476,-0.00051,81573,'EXCHANGE MARKET',81573,0,0,'USD',123]],
   restored_fill=real,opening_lot=dict(ADJ),classified_rows=[dict(trade_id=fake['trade_id'],order_id=fake['order_id'],payload_sha256=repair.digest(fake))])


def test_transaction_precedes_reads_and_explicit_commit_works_in_autocommit(tmp_path):
 from scripts import pirana_accounting_repair as repair
 c=setup(tmp_path);m=repair_fixture(c);trace=[];c.set_trace_callback(trace.append)
 assert c.isolation_level is None
 repair.apply_repair(c,m)
 assert trace[0]=='BEGIN IMMEDIATE'
 assert not c.in_transaction and 'COMMIT' in trace
 assert c.execute('SELECT COUNT(*) FROM accounting_repairs').fetchone()[0]==1
 c.close()


def test_epoch_isolation_keeps_old_lots_and_old_exclusions_out(tmp_path):
 c=setup(tmp_path);c.execute(a.OPENING_LOTS_DDL)
 c.execute('INSERT INTO operational_opening_lots VALUES(?,?,?)',(ADJ['adjustment_id'],'epoch',json.dumps(ADJ)))
 c.execute("UPDATE trading_epoch SET name='later',start_ms=1790600000000 WHERE id=1")
 s=a.snapshot(c,dt.datetime.fromtimestamp(NOW/1000,dt.timezone.utc));o=s['operational']
 assert o['status']=='complete' and not o['execution_provenance']['excluded_records']
 assert not o['opening_lot_adjustments'] and o['financial_status']=='AUTHENTICATED_EXECUTIONS'
 assert o['open_lots'][0]['order_id']==20
 c.close()


def test_every_excluded_operational_identity_requires_exact_lot(tmp_path):
 c=setup(tmp_path);c.execute(a.OPENING_LOTS_DDL)
 c.execute('INSERT INTO operational_opening_lots VALUES(?,?,?)',(ADJ['adjustment_id'],'epoch',json.dumps(ADJ)))
 # Distinct reserved-ID collision cannot be covered by an unrelated valid lot.
 extra=fill(1978200001,987,'0.0001',mts=1789840721000)
 a.ingest(c,dict(fills=[extra],sync=dict(start_ms=0,end_ms=NOW,complete=True)))
 s=a.snapshot(c,dt.datetime.fromtimestamp(NOW/1000,dt.timezone.utc))
 assert s['operational']['status']=='incomplete'
 assert 'operational_opening_lot_missing' in s['operational']['issues']
 assert len(s['operational']['execution_provenance']['excluded_records'])==2
 c.close()


def test_strict_lot_shapes_bounds_and_duplicates(tmp_path):
 import pytest
 c=setup(tmp_path);c.execute(a.OPENING_LOTS_DDL)
 for changed in [dict(ADJ,basis='verified'),dict(ADJ,mts=1),dict(ADJ,mts=NOW+1),dict(ADJ,quantity_btc='NaN')]:
  c.execute('DELETE FROM operational_opening_lots')
  c.execute('INSERT INTO operational_opening_lots VALUES(?,?,?)',(ADJ['adjustment_id'],'epoch',json.dumps(changed)))
  with pytest.raises(ValueError):a.operational_opening_lots(c,a.trading_epoch(c))
 c.execute('DELETE FROM operational_opening_lots')
 for lot in [ADJ,dict(ADJ,adjustment_id='duplicate')]:
  c.execute('INSERT INTO operational_opening_lots VALUES(?,?,?)',(lot['adjustment_id'],'epoch',json.dumps(lot)))
 with pytest.raises(ValueError):a.operational_opening_lots(c,a.trading_epoch(c))
 c.close()


def test_invalid_manifest_and_venue_types_rejected_before_writes(tmp_path):
 import copy,pytest
 from scripts import pirana_accounting_repair as repair
 c=setup(tmp_path);m=repair_fixture(c);before=c.iterdump();before=list(before)
 variants=[]
 for key,value in [('schema_version',True),('opening_lot',{}),('classified_rows',[{}]),('restored_fill',{})]:
  x=copy.deepcopy(m);x[key]=value;variants.append(x)
 for index,value in [(0,True),(2,'wrong'),(4,True),(11,{})]:
  x=copy.deepcopy(m);x['venue_response'][0][index]=value;variants.append(x)
 x=copy.deepcopy(m);x['classified_rows'][0]['payload_sha256']='bad';variants.append(x)
 for x in variants:
  with pytest.raises(ValueError):repair.apply_repair(c,x)
  assert list(c.iterdump())==before and not c.in_transaction
 c.close()


def test_missing_epoch_and_unique_collision_roll_back_entire_repair(tmp_path):
 import pytest
 from scripts import pirana_accounting_repair as repair
 c=setup(tmp_path);m=repair_fixture(c)
 c.execute('DELETE FROM trading_epoch')
 before=list(c.iterdump())
 with pytest.raises(ValueError):repair.apply_repair(c,m)
 assert list(c.iterdump())==before and not c.in_transaction
 c.execute("INSERT INTO trading_epoch VALUES(1,'epoch',1789677493253,'0')")
 collision=fill(1978882098,999,'0.001')
 a.ingest(c,dict(fills=[collision],sync=dict(start_ms=0,end_ms=NOW,complete=True)))
 c.execute('CREATE UNIQUE INDEX fixture_trade_identity ON fills(trade_id)')
 before=list(c.iterdump())
 with pytest.raises(ValueError,match='not inserted'):repair.apply_repair(c,m)
 assert list(c.iterdump())==before and not c.in_transaction
 c.close()


def test_provenance_nonexistent_or_bad_digest_fails_closed(tmp_path):
 import pytest
 c=setup(tmp_path);c.execute(a.PROVENANCE_DDL)
 for tid in [123,999999999999]:
  c.execute('DELETE FROM fill_provenance')
  c.execute('INSERT INTO fill_provenance VALUES(?,?,?,?,?)',(tid,244505000001,'operator_adjustment','0'*64,'fixture'))
  with pytest.raises(ValueError):a.execution_fills(c)
 c.close()


def test_manual_financials_remain_unknown_in_all_public_scopes(tmp_path):
 c=setup(tmp_path);c.execute(a.OPENING_LOTS_DDL)
 c.execute('INSERT INTO operational_opening_lots VALUES(?,?,?)',(ADJ['adjustment_id'],'epoch',json.dumps(ADJ)))
 a.start_period(c,'fixture',1789677493253)
 s=a.snapshot(c,dt.datetime.fromtimestamp(NOW/1000,dt.timezone.utc))
 for total in [s['daily'],s['lifetime'],s['active_period'],s['operational']['daily'],s['operational']['lifetime']]:
  for key in ('gross_pnl_usd','net_pnl_usd','fees_usd','closed_count','win_count','loss_count'):
   assert total[key] is None
 assert s['operational']['status']=='complete' # inventory recovery contract preserved
 c.close()


def test_empty_fill_table_cannot_apply_missing_classified_evidence(tmp_path):
 import pytest
 from scripts import pirana_accounting_repair as repair
 c=setup(tmp_path);m=repair_fixture(c);c.execute('DELETE FROM fills');before=list(c.iterdump())
 with pytest.raises(ValueError,match='classified evidence row missing'):repair.apply_repair(c,m)
 assert list(c.iterdump())==before and not c.in_transaction
 c.close()


def test_multiple_valid_opening_lots_and_cid_collision(tmp_path):
 import pytest
 c=setup(tmp_path);c.execute(a.OPENING_LOTS_DDL)
 second=dict(ADJ,adjustment_id='second',order_id=900,cid='901')
 for lot in (ADJ,second):
  c.execute('INSERT INTO operational_opening_lots VALUES(?,?,?)',(lot['adjustment_id'],'epoch',json.dumps(lot)))
 assert len(a.operational_opening_lots(c,a.trading_epoch(c)))==2
 second['cid']=ADJ['cid']
 c.execute('UPDATE operational_opening_lots SET payload=? WHERE adjustment_id=?',(json.dumps(second),'second'))
 with pytest.raises(ValueError,match='duplicate'):a.operational_opening_lots(c,a.trading_epoch(c))
 c.close()
