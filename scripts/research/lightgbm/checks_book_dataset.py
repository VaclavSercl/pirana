import unittest,json,zlib
from decimal import Decimal as D
from book_dataset import Replay,checksum,number,FEATURES
from collections import deque
import math
class IntegrityTests(unittest.TestCase):
    def record(self, r, frame, ms=1000):
        return r.record(dict(schema=1,kind="frame",session="fixture",wall_ns=ms*1000000,monotonic_ns=ms*1000000,encoding="utf8",raw=json.dumps(frame)))
    def configured(self):
        r=Replay()
        self.record(r,dict(event="conf",status="OK",flags=196608))
        self.record(r,dict(event="subscribed",channel="book",chanId=1,symbol="tBTCUSD",prec="P0",len="25"))
        self.record(r,[1,[[100,2,3],[101,1,-4]],1])
        return r
    def test_signed_checksum_independent_literal(self):
        n=zlib.crc32(b"100:3:101:-4");expected=n if n<2**31 else n-2**32
        self.assertEqual(checksum({D(100):D(3)},{D(101):D(4)}),expected)
        self.assertEqual(number(D("1.0000000")),"1")
        self.assertEqual(number(D("0.00000001")),"1e-8")
    def test_snapshot_updates_delete_and_crc(self):
        r=self.configured();self.record(r,[1,[100,0,1],2]);self.record(r,[1,[99,1,2],3])
        crc=checksum({D(99):D(2)},{D(101):D(4)})
        self.record(r,[1,"cs",crc,4]);self.assertEqual(r.stats["checksums_ok"],1)
        self.assertEqual(r.bids,{D(99):D(2)})
    def test_gap_latches_and_later_checksum_cannot_hide_it(self):
        r=self.configured();self.record(r,[1,"hb",3]);self.record(r,[1,"cs",checksum(r.bids,r.asks),4])
        self.assertTrue(r.broken);self.assertEqual(r.stats["checksums_ok"],0)
    def test_bad_crc_is_excluded(self):
        r=self.configured();self.record(r,[1,"cs",0,2]);self.assertTrue(r.broken)
    def test_unconfigured_legacy_is_not_trusted(self):
        r=Replay();self.record(r,[1,[[100,2,3],[101,1,-4]],1]);self.assertFalse(r.snapshot)
    def test_volatility_contract_is_65s_and_excludes_older_prices(self):
        r=Replay();r.channels={1:dict(channel="trades",symbol="tBTCUSD")};r.first_cs=0
        r.bids={D(100-i):D(1) for i in range(10)};r.asks={D(101+i):D(1) for i in range(10)}
        r.history=deque([(34000,10000.)]+[(t,90. if t==35000 else 100.5) for t in range(35000,100000,5000)])
        row=r.observation(100000)
        self.assertIsNotNone(row)
        self.assertAlmostEqual(row["features"][FEATURES.index("volatility_65_bps")],math.log(100.5/90.)*10000)
        self.assertNotIn("volatility_60_bps",FEATURES)
    def test_clock_reversal_invalidates_session(self):
        r=self.configured();self.record(r,[1,"hb",2],500);self.assertTrue(r.broken)
if __name__=="__main__":unittest.main()
