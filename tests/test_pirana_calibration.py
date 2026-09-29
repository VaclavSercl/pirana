"""Real temporary canonical SQLite fixtures; no live exchange or legacy fallback."""
import copy
import json
from decimal import Decimal
from pathlib import Path
import sqlite3

import pytest
from scripts import pirana_accounting as accounting
from scripts import pirana_calibration as cal

DAY=cal.DAY
NOW=20000*DAY+1000


def pos(pid=1,order=101):
    return dict(position_id=pid,exchange_order_id=order,entry_mts=NOW-10*DAY,entry_price=100,side="Buy",is_paper=False,is_shadow=False,is_rebalance=False)


def fill(tid,oid,amount,price,mts,cid=None,fee="0",currency="USD"):
    return dict(trade_id=tid,order_id=oid,exec_amount=amount,exec_price=price,mts=mts,cid=cid,
                fee=fee,fee_currency=currency,symbol="tBTCUSD")


def sources(tmp_path, fills=None, positions=None, now=NOW):
    con=accounting.connect(tmp_path/"accounting.sqlite3",True)
    default=[fill(1,101,"1","100",now-2000,"100"),fill(2,201,"-1","110",now-1000,"200")]
    accounting.ingest(con,dict(fills=default if fills is None else fills,sync=dict(start_ms=0,end_ms=now,complete=True)))
    journal=dict(schema_version=2,positions=[],recovery_candidates=[],exit_intents={"200":pos()},settled_exit_cids=["200"])
    if positions is not None:journal.update(positions)
    path=tmp_path/"positions.json";path.write_text(json.dumps(journal))
    equity=tmp_path/"equity";equity.mkdir()
    return con,path,equity


def sample(t):
    return dict(schema_version=1,source="authenticated_reconciled_wallet",observed_at_ms=t,
                wallet_at_ms=t,mark_at_ms=t,btc_balance="1",usd_balance="0",btc_price="100",
                session_id="fixture",boot_id="fixture")


def write_days(directory, days=5, omit=None):
    for day in range(NOW//DAY-days,NOW//DAY+1):
        times=range(day*DAY,(day+1)*DAY,60000) if day<NOW//DAY else [day*DAY]
        rows=[sample(t) for t in times if t!=omit]
        (directory/f"equity-{day}.jsonl").write_text("".join(json.dumps(s)+"\n" for s in rows))


def test_actual_roundtrip_survives_restart_without_legacy(tmp_path):
    con,path,equity=sources(tmp_path)
    try:
        first=cal.build_calibration(con,path,equity,NOW)
        second=cal.build_calibration(con,path,equity,NOW)
        assert first==second
        assert first["status"]=="WARMUP" and first["roundtrip_count"]==1
        trade=first["trades"][0]
        assert trade["ts"]==(NOW-1000)//1000
        assert trade["provenance"]["pnl_usd"]=="10"
        assert trade["provenance"]["exit_fills"]==[[2,201]]
        assert first["complete_day_count"]==0
        assert con.execute("SELECT count(*) FROM fills").fetchone()[0]==2
    finally:con.close()


def test_partial_exits_and_base_quote_fees_count_one_completed_position(tmp_path):
    rows=[fill(1,101,"1","100",NOW-6000,"100", "-0.01","BTC"),
          fill(2,201,"-0.4","110",NOW-5000,"201", "-1","USD"),
          fill(2,202,"-0.59","120",NOW-4000,"202", "-2","USD")]
    journal=dict(exit_intents={"201":pos(),"202":pos()},settled_exit_cids=["201","202"])
    con,path,equity=sources(tmp_path,rows,journal)
    try:
        result=cal.build_calibration(con,path,equity,NOW)
        assert result["roundtrip_count"]==1
        trade=result["trades"][0]
        assert abs(Decimal(trade["provenance"]["pnl_usd"])-Decimal("11.8"))<Decimal("1e-60")
        assert trade["provenance"]["exit_fills"]==[[2,201],[2,202]]
        assert Decimal(trade["fee_sats"])>1000000
        raw=json.loads(path.read_text());raw["positions"]=[pos()];path.write_text(json.dumps(raw))
        assert cal.build_calibration(con,path,equity,NOW)["roundtrip_count"]==0
    finally:con.close()


def test_missing_synthetic_basis_and_shadow_never_qualify(tmp_path):
    rows=[fill(999999999999,101,"1","100",NOW-2000,"100"),fill(2,201,"-1","110",NOW-1000,"200")]
    con,path,equity=sources(tmp_path,rows)
    try:
        result=cal.build_calibration(con,path,equity,NOW)
        assert result["trades"]==[]
        assert result["provenance"]["excluded_synthetic_fill_identities"]==[[999999999999,101]]
        assert "unknown_actual_entry_basis" in str(result["provenance"]["unverified_execution_days"])
    finally:con.close()


def test_ready_requires_real_five_complete_days_and_fifty_roundtrips(tmp_path):
    rows=[];intents={};settled=[]
    for i in range(50):
        cid=str(10000+i);oid=1000+i;exit_order=2000+i;t=NOW-DAY-5000-i*100
        rows.extend([fill(i*2+1,oid,"1","100",t,cid+"202"),fill(i*2+2,exit_order,"-1","101",t+10,cid)])
        intents[cid]=pos(i+1,oid);settled.append(cid)
    con,path,equity=sources(tmp_path,rows,dict(exit_intents=intents,settled_exit_cids=settled))
    try:
        write_days(equity)
        result=cal.build_calibration(con,path,equity,NOW)
        assert result["status"]=="READY",result["reasons"]
        assert result["roundtrip_count"]==50 and result["complete_day_count"]==5
        assert result["days"][-1]["end_ms"]==NOW//DAY*DAY
        # Removing a single minute creates a120s gap, not a zero-return day.
        write_days(equity,omit=(NOW//DAY-2)*DAY+60000)
        gap=cal.build_calibration(con,path,equity,NOW)
        assert gap["status"]=="WARMUP" and gap["complete_day_count"]==1
        assert any("equity_coverage_gap" in d["reasons"] for d in gap["provenance"]["rejected_days"])
    finally:con.close()


def test_unmapped_sell_blocks_otherwise_complete_day(tmp_path):
    rows=[fill(1,101,"1","100",NOW-DAY-2000,"100"),fill(2,201,"-1","110",NOW-DAY-1000,"999")]
    con,path,equity=sources(tmp_path,rows)
    try:
        write_days(equity)
        result=cal.build_calibration(con,path,equity,NOW)
        assert result["complete_day_count"]==0
        assert "unattributed_or_unsettled_sell" in str(result["provenance"]["rejected_days"])
    finally:con.close()


def test_stale_sync_malformed_journal_and_future_equity_fail_closed(tmp_path):
    con,path,equity=sources(tmp_path)
    try:
        assert cal.build_calibration(con,path,equity,NOW+120001)["status"]=="BLOCKED"
        path.write_text('{"schema_version":2,"schema_version":2}')
        assert cal.build_calibration(con,path,equity,NOW)["status"]=="BLOCKED"
    finally:con.close()


def test_no_fixture_jsonl_fallback_or_synthetic_zero_days(tmp_path):
    con,path,equity=sources(tmp_path,[])
    try:
        (tmp_path/"trade_ledger.jsonl").write_text('{"pnl_sats":999999,"ts":1}\n'*1000)
        (equity/"legacy.json").write_text('{"daily_returns":[1,2,3,4,5]}')
        result=cal.build_calibration(con,path,equity,NOW)
        assert result["trades"]==[] and result["days"]==[] and result["status"]=="WARMUP"
    finally:con.close()


def test_canonical_compound_payload_identity_mismatch_blocks(tmp_path):
    con,path,equity=sources(tmp_path)
    try:
        raw=json.loads(con.execute("SELECT payload FROM fills WHERE trade_id=1").fetchone()[0]);raw["order_id"]=999
        con.execute("UPDATE fills SET payload=? WHERE trade_id=1",(json.dumps(raw),))
        assert cal.build_calibration(con,path,equity,NOW)["status"]=="BLOCKED"
    finally:con.close()


@pytest.mark.parametrize("mode",["shadow","paper","rebalance","unsupported_fee","unsettled"])
def test_untrusted_attribution_cannot_enter_calibration(tmp_path,mode):
    rows=[fill(1,101,"1","100",NOW-4000,"100"),fill(2,201,"-1","110",NOW-3000,"200")]
    position=pos();journal={"exit_intents":{"200":position},"settled_exit_cids":["200"]}
    if mode in ("shadow","paper","rebalance"):position["is_"+mode]=True
    if mode=="unsupported_fee":rows[0]["fee_currency"]="UST";rows[0]["fee"]="-0.01"
    if mode=="unsettled":journal["settled_exit_cids"]=[]
    con,path,equity=sources(tmp_path,rows,journal)
    try:
        result=cal.build_calibration(con,path,equity,NOW)
        assert result["trades"]==[]
        assert result["provenance"]["unverified_execution_days"]
    finally:con.close()


@pytest.mark.parametrize("mode",["future","stale_mark","truncated","backward","wrong_source"])
def test_invalid_equity_cannot_create_complete_day(tmp_path,mode):
    con,path,equity=sources(tmp_path)
    try:
        s=sample(NOW)
        if mode=="future":s["observed_at_ms"]+=1
        if mode=="stale_mark":s["mark_at_ms"]-=30001
        if mode=="wrong_source":s["source"]="legacy_fixture"
        payload=json.dumps(s)+"\n"
        if mode=="truncated":payload=payload.rstrip()
        if mode=="backward":payload+=json.dumps(sample(NOW-1))+"\n"
        (equity/f"equity-{NOW//DAY}.jsonl").write_text(payload)
        result=cal.build_calibration(con,path,equity,NOW)
        assert result["status"]=="BLOCKED" and result["days"]==[]
    finally:con.close()


def test_partial_first_day_is_not_a_complete_daily_return(tmp_path):
    con,path,equity=sources(tmp_path,[])
    try:
        write_days(equity)
        first=equity/f"equity-{NOW//DAY-5}.jsonl"
        lines=first.read_text().splitlines();first.write_text("\n".join(lines[1:])+"\n")
        result=cal.build_calibration(con,path,equity,NOW)
        assert result["complete_day_count"]==4
        assert result["days"][0]["start_ms"]==(NOW//DAY-4)*DAY
    finally:con.close()


def test_equity_append_retains_exact_original_prefix_and_cutoff(tmp_path, monkeypatch):
    import hashlib
    path=tmp_path/f"equity-{NOW//DAY}.jsonl"
    original=(json.dumps(sample(NOW))+"\n").encode();path.write_bytes(original)
    capture=cal._equity_capture;calls=0
    def appending(p,maximum):
        nonlocal calls
        calls+=1
        if calls==2:
            with p.open('ab') as stream:stream.write((json.dumps(sample(NOW+15000))+"\n").encode())
        return capture(p,maximum)
    monkeypatch.setattr(cal,'_equity_capture',appending)
    samples, sources, _=cal._equity(tmp_path,NOW)
    assert len(samples)==1 and samples[0]['mts']==NOW
    evidence=sources[0]
    assert evidence['prefix_bytes']==len(original)
    assert evidence['sha256']==hashlib.sha256(original).hexdigest()
    assert evidence['appended_bytes_ignored']>0 and evidence['snapshot_cutoff_ms']==NOW
    assert evidence['sha256']!=hashlib.sha256(path.read_bytes()).hexdigest()


@pytest.mark.parametrize('change',['mutation','truncate','replace','symlink','oversize'])
def test_equity_prefix_changes_fail_closed(tmp_path,monkeypatch,change):
    path=tmp_path/f"equity-{NOW//DAY}.jsonl";path.write_text(json.dumps(sample(NOW))+"\n")
    capture=cal._equity_capture;calls=0
    def changing(p,maximum):
        nonlocal calls
        calls+=1
        if calls==2:
            if change=='mutation':p.write_text(p.read_text().replace('fixture','changed'))
            if change=='truncate':p.write_bytes(b'')
            if change=='replace':
                other=tmp_path/'replacement';other.write_bytes(p.read_bytes());other.replace(p)
            if change=='symlink':
                other=tmp_path/'target';other.write_bytes(p.read_bytes());p.unlink();p.symlink_to(other)
            if change=='oversize':
                with p.open('ab') as stream:stream.truncate(maximum+1)
        return capture(p,maximum)
    monkeypatch.setattr(cal,'_equity_capture',changing)
    with pytest.raises((ValueError,OSError)):cal._equity(tmp_path,NOW)


def test_current_partial_tail_completion_defers_new_record(tmp_path,monkeypatch):
    import hashlib
    path=tmp_path/f"equity-{NOW//DAY}.jsonl"
    first=(json.dumps(sample(NOW))+"\n").encode()
    later=(json.dumps(sample(NOW+15000))+"\n").encode()
    path.write_bytes(first+later[:20]);capture=cal._equity_capture;calls=0
    def completing(p,maximum):
        nonlocal calls
        calls+=1
        if calls==2:
            with p.open('ab') as stream:stream.write(later[20:])
        return capture(p,maximum)
    monkeypatch.setattr(cal,'_equity_capture',completing)
    samples,sources,_=cal._equity(tmp_path,NOW)
    assert [s['mts'] for s in samples]==[NOW]
    assert sources[0]['sha256']==hashlib.sha256(first).hexdigest()
    assert sources[0]['deferred_tail_bytes']==len(later)


def test_historical_partial_tail_never_silently_dropped(tmp_path):
    path=tmp_path/f"equity-{NOW//DAY-1}.jsonl"
    path.write_text(json.dumps(sample(NOW-DAY)))
    with pytest.raises(ValueError,match='truncated_equity_stream'):cal._equity(tmp_path,NOW)


def test_initial_oversized_equity_rejected(tmp_path):
    path=tmp_path/f"equity-{NOW//DAY}.jsonl"
    with path.open('wb') as stream:stream.truncate(16*1024*1024+1)
    with pytest.raises(ValueError,match='evidence_size_limit'):cal._equity(tmp_path,NOW)
