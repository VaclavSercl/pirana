import unittest
from book_dataset import FEATURES
from experiment import make_labels,split

def rows(start=0,session="s",count=50):
    return [dict(schema=1,session=session,observed_ms=start+i*5000,features=[0.]*len(FEATURES),ask_vwap=101.,bid_vwap=100.,quantity_btc=.00046) for i in range(count)]
class LabelTests(unittest.TestCase):
    def test_future_bid_includes_spread_and_keeps_losers(self):
        data=rows();labels=make_labels(data)
        self.assertEqual(len(labels),38);self.assertLess(labels[0]["markout_bps"],0)
        self.assertEqual(labels[0]["label_end_ms"],60000)
    def test_no_labels_across_session_or_data_gap(self):
        self.assertEqual(make_labels(rows(count=5)+rows(25000,"other",count=5)),[])
        self.assertEqual(make_labels(rows(count=5)+rows(100000,count=5)),[])
    def test_nonmonotonic_or_missing_features_fail(self):
        data=rows();data[1]["observed_ms"]=0
        with self.assertRaises(ValueError):make_labels(data)
        data=rows();data[1]["features"]=[]
        with self.assertRaises(ValueError):make_labels(data)
    def test_split_purges_future_labels_and_keeps_test_unseen(self):
        data=[]
        for day in range(10):data+=rows(day*86400000,count=400)
        tr,va,te,b=split(make_labels(data))
        self.assertLess(max(x["label_end_ms"] for x in tr),min(x["observed_ms"] for x in va)-300000)
        self.assertLess(max(x["label_end_ms"] for x in va),min(x["observed_ms"] for x in te)-300000)
    def test_invalid_execution_prices_and_sizes_are_not_labels(self):
        for field in ["ask_vwap","bid_vwap","quantity_btc"]:
            for value in [float("nan"),float("inf"),0,-1,True]:
                data=rows();data[0][field]=value
                with self.assertRaises(ValueError):make_labels(data)
    def test_short_history_blocks_training(self):
        with self.assertRaises(ValueError):split(make_labels(rows()))
if __name__=="__main__":unittest.main()
