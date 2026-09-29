import importlib.util
import json
from pathlib import Path
import pytest

spec = importlib.util.spec_from_file_location('measurements', Path(__file__).parents[1] / 'scripts/pirana_measurements.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def fill(tid=1, qty='1', price='102', cid='7', mts=1010, order=8):
    return dict(trade_id=tid,order_id=order,mts=mts,cid=cid,symbol='tBTCUSD',exec_amount=qty,exec_price=price,fee='0',fee_currency='USD')


def benchmark(side='Buy', quantity=2):
    return dict(decision_mts=1000,reference_price=100,requested_quantity=quantity,side=side,benchmark_kind='signal_last_trade')


def sample(t, price=100, btc=1, usd=0, session='a'):
    return dict(schema_version=1,observed_at_ms=t,wallet_at_ms=t,mark_at_ms=t,btc_balance=btc,usd_balance=usd,btc_price=price,source='authenticated_reconciled_wallet',session_id=session,boot_id='boot')


def test_partial_fill_weighting_side_and_fees():
    fs=[fill(qty='0.5'),fill(2,qty='1.5',price='104')]
    r=m.slippage(fs,{'7':benchmark()},0,2000)
    assert r['status']=='VERIFIED'
    assert m.dec(r['verified_subset_weighted_bps'])==350
    fs=[fill(qty='-1',price='98')]
    assert m.dec(m.slippage(fs,{'7':benchmark('Sell')},0,2000)['verified_subset_weighted_bps'])==200
    fs[0]['exec_price']='102'
    assert m.dec(m.slippage(fs,{'7':benchmark('Sell')},0,2000)['verified_subset_weighted_bps'])==-200


def test_missing_legacy_is_unknown_not_zero():
    r=m.slippage([fill()],{},0,2000)
    assert r['status']=='INCOMPLETE' and r['verified_subset_weighted_bps'] is None
    assert m.slippage([],{},0,2000)['status']=='NO_FILLS'


@pytest.mark.parametrize('fs,b', [([fill(qty='3')],benchmark()),([fill(qty='-1')],benchmark()),([fill(mts=999)],benchmark()),([fill(),fill(2,order=9)],benchmark()),([fill()],dict(benchmark(),reference_price=float('nan')))])
def test_invalid_benchmark_or_join_not_verified(fs,b):
    r=m.slippage(fs,{'7':b},0,2000)
    assert r['invalid'] and r['verified_count']==0


def test_partial_boundary_counts_all_order_fills():
    fs=[fill(mts=1010),fill(2,mts=2000)]
    assert m.slippage(fs,{'7':benchmark(quantity=1)},1500,2100)['invalid']


def test_external_inventory_missing_not_zero_basis():
    r=m.historical_gaps([fill(qty='-0.005'),fill(2,qty='0.002'),fill(3,qty='-0.003')])
    assert [x['missing_sats'] for x in r['gaps']]==['500000.000','100000.000']
    assert r['status']=='INCOMPLETE'


def test_sampled_drawdown_two_numeraire():
    r=m.equity_report([sample(1000),sample(16000,price=80),sample(31000,price=110)],1000,31000)
    assert r['status']=='SAMPLED_COVERAGE'
    dd=r['segments'][0]['sampled_unadjusted_max_drawdown_pct']
    assert m.dec(dd['usd'])==20 and m.dec(dd['sats'])==0
    assert 'UNVERIFIED' in r['cashflow_adjusted_performance']


def test_gaps_restart_and_single_sample():
    r=m.equity_report([sample(1000),sample(16000),sample(80000)],1000,80000)
    assert r['status']=='INCOMPLETE' and len(r['segments'])==2
    r=m.equity_report([sample(1000),sample(16000,session='b')],1000,16000)
    assert r['gaps'] and r['segments'][1]['sampled_unadjusted_max_drawdown_pct']['usd'] is None
    assert m.equity_report([],1000,16000)['status']=='INCOMPLETE'
    assert m.equity_report([sample(1000),sample(16000)],1000,100000)['status']=='INCOMPLETE'


@pytest.mark.parametrize('field,value',[('mark_at_ms',1001),('wallet_at_ms',0),('btc_price','NaN'),('btc_balance',-1),('source','legacy'),('schema_version',2)])
def test_invalid_samples(field,value):
    x=sample(40000 if field=='wallet_at_ms' else 1000);x[field]=value
    with pytest.raises(ValueError):m.equity_report([x],0,50000)


def test_duplicate_and_truncated_evidence(tmp_path):
    with pytest.raises(ValueError):m.equity_report([sample(1000),sample(1000)],0,2000)
    path=tmp_path/'equity-1.jsonl';path.write_text(json.dumps(sample(1000)))
    with pytest.raises(ValueError):m.read_equity(tmp_path)
    path.write_text(json.dumps(sample(1000))+'\n')
    rows,sources=m.read_equity(tmp_path)
    assert len(rows)==1 and len(sources[0]['sha256'])==64


def test_benchmark_sidecar_missing_corrupt_valid(tmp_path):
    path=tmp_path/'positions.benchmarks.json'
    assert m.read_benchmarks(path)=={}
    path.write_text(json.dumps({'schema_version':1,'decision_benchmarks':{'7':benchmark()}}))
    assert m.read_benchmarks(path)['7']['side']=='Buy'
    path.write_text('{}')
    with pytest.raises(ValueError):m.read_benchmarks(path)


def test_canonical_typed_provenance_is_checked_before_measurement(tmp_path):
    import sqlite3
    from scripts import pirana_accounting as accounting
    db = tmp_path / "ledger.db"
    con = sqlite3.connect(db)
    con.execute("CREATE TABLE fills(payload TEXT)")
    con.execute("CREATE TABLE sync(id INTEGER,cursor_ms INTEGER,coverage_start_ms INTEGER,complete INTEGER)")
    con.execute("INSERT INTO sync VALUES(1,2000,0,1)")
    con.execute("INSERT INTO fills VALUES(?)", (json.dumps(fill()),))
    con.commit();con.close()
    fs, sync, provenance = m.canonical_fills(db)
    assert len(fs) == 1 and sync == (2000,0,1) and not provenance['excluded_records']
    con = sqlite3.connect(db)
    con.execute("CREATE TABLE fill_provenance(trade_id INTEGER,order_id INTEGER,kind TEXT,digest TEXT,reference TEXT)")
    con.execute("INSERT INTO fill_provenance VALUES(1,8,'operator_adjustment','wrong','fixture')")
    con.commit();con.close()
    with pytest.raises(ValueError): m.canonical_fills(db)


def test_owner_basis_report_never_clears_history(tmp_path, monkeypatch, capsys):
    owner = dict(schema_version=1, classification='owner_declared_cost_basis',
        actual_acquisition_basis='UNKNOWN', entries=[dict(schema_version=1,
        classification='owner_declared_cost_basis', actual_acquisition_basis='UNKNOWN',
        ledger_id=1, deposit_mts=1000, quantity_btc='0.001', price_usd='99999',
        owner_reference='fixture instruction', owner_approval='explicit fixture approval',
        declared_at='2026-09-29T20:00:00+00:00')])
    path = tmp_path / 'owner.json';path.write_text(json.dumps(owner))
    monkeypatch.setattr(m, 'canonical_fills', lambda _: ([fill(qty='-1')],(2000,0,1),dict(excluded_records=[])))
    monkeypatch.setattr(m, 'read_equity', lambda _: ([],[]))
    monkeypatch.setattr(m, 'read_benchmarks', lambda _: {})
    monkeypatch.setattr('sys.argv', ['measurement','--db','unused','--positions','unused',
        '--equity-dir','unused','--start-ms','0','--end-ms','2000','--funding-valuations',str(path)])
    assert m.main() == 0
    result=json.loads(capsys.readouterr().out)
    assert result['status']=='INCOMPLETE' and result['history']['gaps']
    assert result['funding_valuation_semantics']['kind']=='owner_declared_cost_basis'
    assert result['funding_valuations']['entries'][0]['owner_declared_cost_basis_usd']=='99.999'
    assert result['funding_valuation_semantics']['canonical_fifo']=='UNCHANGED'
