"""Read-only Bitfinex P0 archive audit. No credentials, orders, or runtime imports.
CRC contract: https://docs.bitfinex.com/docs/ws-websocket-checksum
Only checksum-confirmed, sequential books become observations; corrupt sessions are excluded.
"""
from collections import Counter, deque
from decimal import Decimal
from pathlib import Path
import argparse, hashlib, json, math, os, time, zlib

FLAGS = 196608
FEATURES = ["spread_bps", "micro_offset_bps", "imbalance_1", "imbalance_5", "imbalance_10",
            "bid_depth_5", "ask_depth_5", "bid_depth_10", "ask_depth_10", "bid_slope_bps", "ask_slope_bps",
            "flow_15", "flow_60", "volume_15", "volume_60", "trades_15", "trades_60",
            "return_5_bps", "return_30_bps", "return_60_bps", "volatility_65_bps", "utc_sin", "utc_cos"]

def number(value):
    d = Decimal(value)
    if not d.is_finite(): raise ValueError("non-finite number")
    if not d: return "0"
    d = d.normalize()
    if Decimal("0.000001") <= abs(d) < Decimal("1e21"):
        return format(d, "f")
    mantissa, exponent = format(d, "e").split("e")
    e = int(exponent)
    return mantissa + "e" + ("+" if e >= 0 else "-") + str(abs(e))

def checksum(bids, asks):
    b, a = sorted(bids, reverse=True)[:25], sorted(asks)[:25]
    values = []
    for i in range(max(len(b), len(a))):
        if i < len(b): values.extend((number(b[i]), number(bids[b[i]])))
        if i < len(a): values.extend((number(a[i]), number(-asks[a[i]])))
    crc = zlib.crc32(":".join(values).encode())
    return crc if crc < 2**31 else crc - 2**32

def vwap(levels, quantity):
    amount = quantity; cost = 0.
    for price, size in levels:
        taken = min(amount, size); cost += taken * price; amount -= taken
        if amount <= 1e-15: return cost / quantity
    return None

class Replay:
    def __init__(self, interval_ms=5000, quantity=.00046):
        self.interval_ms=interval_ms; self.quantity=quantity; self.stats=Counter(); self.session=None
        self.reset()
    def reset(self):
        self.channels={};self.flags=False;self.seq=None;self.bids={};self.asks={};self.snapshot=False
        self.broken=False;self.last_time=None;self.first_cs=None;self.emitted=-1;self.trades=deque();self.history=deque()
    def break_session(self, reason):
        self.stats[reason]+=1;self.broken=True;self.history.clear();self.trades.clear()
    def record(self, record):
        if record.get("schema") != 1: self.stats["unsupported_schema"]+=1;return
        session=record.get("session")
        if not isinstance(session,str) or not session:
            self.break_session("invalid_session");return
        if session != self.session:
            self.session=session;self.reset();self.stats["sessions"]+=1
        if record.get("kind") in ("disconnect", "capture_stop"):
            self.break_session("disconnect");return
        if record.get("kind") != "frame": return
        if record.get("encoding") != "utf8": self.break_session("non_utf8");return
        now=record["wall_ns"]//1_000_000;mono=record["monotonic_ns"]//1_000_000
        if self.last_time:
            dw=now-self.last_time[0];dm=mono-self.last_time[1]
            if dw<0 or dm<0 or abs(dw-dm)>1000: self.break_session("clock_discontinuity")
        self.last_time=(now,mono)
        frame=json.loads(record["raw"],parse_float=Decimal)
        if isinstance(frame,dict):
            if frame.get("event")=="conf": self.flags=frame.get("status")=="OK" and frame.get("flags")==FLAGS
            if frame.get("event")=="subscribed":
                self.channels[frame["chanId"]]=frame
                if frame.get("channel")=="book" and (frame.get("symbol")!="tBTCUSD" or frame.get("prec")!="P0" or str(frame.get("len"))!="25"):
                    self.break_session("unsupported_book")
            if frame.get("event")=="info" and frame.get("code") in (20051,20060):self.break_session("venue_maintenance")
            if frame.get("event") in ("error","unsubscribed"):self.break_session("protocol_event")
            return
        if not self.flags: self.stats["frames_without_verified_flags"]+=1;return
        if not isinstance(frame,list) or len(frame)<3 or type(frame[-1]) is not int:
            self.break_session("missing_sequence");return
        seq=frame[-1]
        if self.seq is not None and seq != self.seq+1: self.break_session("sequence_gap")
        self.seq=seq
        if self.broken: return
        channel=self.channels.get(frame[0],{}).get("channel")
        payload=frame[1]
        if channel=="trades" and self.channels[frame[0]].get("symbol")=="tBTCUSD" and payload=="te":
            trade=frame[2];amount=float(trade[2]);self.trades.append((now,amount))
        if channel!="book" or payload=="hb":return
        if payload=="cs":
            if not self.snapshot:self.stats["checksum_without_snapshot"]+=1;return
            if checksum(self.bids,self.asks)!=frame[2]:self.break_session("checksum_mismatch");return
            self.stats["checksums_ok"]+=1
            return self.observation(now)
        if isinstance(payload,list):
            entries=payload if payload and isinstance(payload[0],list) else [payload]
            if payload and isinstance(payload[0],list):
                self.bids.clear();self.asks.clear();self.snapshot=True;self.stats["snapshots"]+=1
            if not self.snapshot: self.stats["updates_without_snapshot"]+=1;return
            for p,count,amount in entries:
                p=Decimal(p);amount=Decimal(amount)
                if p<=0 or not p.is_finite() or not amount.is_finite() or not amount or type(count) is not int or count<0:
                    self.break_session("invalid_book_entry");return
                book=self.bids if amount>0 else self.asks
                if count==0:
                    if abs(amount)!=1:self.break_session("invalid_delete");return
                    book.pop(p,None)
                else: book[p]=abs(amount)
        else:self.break_session("unknown_book_frame")
    def observation(self, now):
        if not self.bids or not self.asks:return
        if not any(c.get("channel")=="trades" and c.get("symbol")=="tBTCUSD" for c in self.channels.values()):
            self.stats["missing_trade_subscription"]+=1;return
        b=sorted(((float(p),float(v)) for p,v in self.bids.items()),reverse=True)[:25]
        a=sorted((float(p),float(v)) for p,v in self.asks.items())[:25]
        if b[0][0]>=a[0][0]:self.break_session("crossed_book_at_checksum");return
        if len(b)<10 or len(a)<10:self.stats["insufficient_depth"]+=1;return
        if self.first_cs is None:self.first_cs=now
        mid=(b[0][0]+a[0][0])/2
        self.history.append((now,mid))
        while self.history and self.history[0][0]<now-65000:self.history.popleft()
        while self.trades and self.trades[0][0]<now-60000:self.trades.popleft()
        if now-self.first_cs<65000 or now-self.emitted<self.interval_ms:return
        if not self.history or any(y[0]-x[0]>15000 for x,y in zip(self.history,list(self.history)[1:])):
            self.stats["feature_history_gap"]+=1;return
        refs=[]
        for seconds in [5,30,60]:
            values=[x for x in self.history if x[0]<=now-seconds*1000]
            if not values or now-seconds*1000-values[-1][0]>5000:self.stats["missing_return_history"]+=1;return
            refs.append(10000*(mid/values[-1][1]-1))
        ask=vwap(a,self.quantity);bid=vwap(b,self.quantity)
        if ask is None or bid is None:self.stats["insufficient_executable_depth"]+=1;return
        micro=(a[0][0]*b[0][1]+b[0][0]*a[0][1])/(b[0][1]+a[0][1])
        imbalance=[]
        for depth in [1,5,10]:
            bs=sum(x[1] for x in b[:depth]);az=sum(x[1] for x in a[:depth]);imbalance.append((bs-az)/(bs+az))
        trades15=[v for t,v in self.trades if t>=now-15000];trades60=[v for t,v in self.trades]
        def flow(values):return sum(values)/sum(map(abs,values)) if values else 0.
        returns=[math.log(y[1]/x[1])*10000 for x,y in zip(self.history,list(self.history)[1:])]
        values=[(a[0][0]-b[0][0])/mid*10000,(micro-mid)/mid*10000]+imbalance
        values += [sum(v for _,v in b[:5]),sum(v for _,v in a[:5]),sum(v for _,v in b[:10]),sum(v for _,v in a[:10]),(b[0][0]-b[9][0])/mid*10000,(a[9][0]-a[0][0])/mid*10000]
        values += [flow(trades15),flow(trades60),sum(map(abs,trades15)),sum(map(abs,trades60)),len(trades15),len(trades60)]+refs
        values += [math.sqrt(sum(x*x for x in returns)),math.sin(2*math.pi*(now%86400000)/86400000),math.cos(2*math.pi*(now%86400000)/86400000)]
        if not all(math.isfinite(v) for v in values):self.break_session("nonfinite_features");return
        assert len(values)==len(FEATURES)
        self.emitted=now;self.stats["observations"]+=1
        return dict(schema=1,session=self.session,observed_ms=now,features=values,ask_vwap=ask,bid_vwap=bid,quantity_btc=self.quantity)

def audit(directory, output):
    output=Path(output);output.mkdir(mode=0o700,parents=True,exist_ok=False)
    parser_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    files=sorted(Path(directory).glob("*.jsonl"));manifest=[];parser=Replay();days=Counter();start=time.monotonic()
    cutoffs=[(p,p.stat().st_size,p.stat().st_ino) for p in files if p.is_file() and not p.is_symlink()]
    with (output/"observations.jsonl").open("x") as out:
        for index,(path,limit,inode) in enumerate(cutoffs):
            h=hashlib.sha256();count=0;remaining=limit
            with path.open("rb") as source:
                while remaining:
                    raw=source.readline(min(remaining,1048577));remaining-=len(raw);h.update(raw)
                    if not raw:raise ValueError("source truncated during audit")
                    if len(raw)>1048576:raise ValueError("oversized capture record")
                    if not raw.endswith(b"\n"):
                        parser.break_session("truncated_record");continue
                    count+=1
                    try:
                        record=json.loads(raw);row=parser.record(record)
                    except (ValueError,TypeError,KeyError,IndexError,ArithmeticError):
                        parser.break_session("invalid_record");continue
                    if row:
                        out.write(json.dumps(row,separators=(",",":"))+"\n");days[str(row["observed_ms"]//86400000)]+=1
            st=path.stat()
            if st.st_ino!=inode or st.st_size<limit:raise ValueError("capture identity changed")
            manifest.append(dict(path=str(path),bytes=limit,sha256=h.hexdigest(),records=count))
            print(json.dumps(dict(file=index+1,total=len(files),observations=parser.stats["observations"])),flush=True)
    result=dict(schema=1,feature_contract=2,parser_sha256=parser_sha256,status="AUDITED_WITH_EXCLUSIONS",features=FEATURES,stats=dict(parser.stats),observations_by_utc_epoch_day=dict(days),source_manifest=manifest,seconds=time.monotonic()-start,limitations=["Public market observations, not Pirana signals or fills", "Receive-time features; no measurement of venue-to-host latency", "No continuity inferred from first/last archive date", "Broken sessions excluded until next session", "Live file read only through pinned prefix; incomplete tail excluded"])
    (output/"audit.json").write_text(json.dumps(result,indent=2)+"\n")
    return result
if __name__=="__main__":
    ap=argparse.ArgumentParser();ap.add_argument("--directory",required=True);ap.add_argument("--output",required=True);args=ap.parse_args();audit(args.directory,args.output)
