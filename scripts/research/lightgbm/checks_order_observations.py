from pathlib import Path
import tempfile,json,unittest
from book_dataset import FEATURES
from order_observations import order
from experiment import digest,make_labels

class ChronologyTests(unittest.TestCase):
    def source(self,root,rows):
        p=root/"source";p.mkdir()
        (p/"audit.json").write_text(json.dumps(dict(feature_contract=2,features=FEATURES)))
        (p/"observations.jsonl").write_text("".join(json.dumps(x)+"\n" for x in rows))
        return p
    def row(self,t,session):return dict(schema=1,observed_ms=t,session=session,features=[0.]*len(FEATURES),ask_vwap=101.,bid_vwap=100.,quantity_btc=.00046)
    def test_reordered_files_preserve_all_rows_and_original(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);rows=[self.row(90000,"late"),self.row(10000,"early"),self.row(70000,"early")]
            p=self.source(root,rows);before=digest(p/"observations.jsonl");result=order(p,root/"view")
            copied=[json.loads(x) for x in (root/"view/observations.jsonl").read_text().splitlines()]
            self.assertEqual(copied,sorted(rows,key=lambda x:x["observed_ms"]))
            self.assertEqual(before,digest(p/"observations.jsonl"));self.assertEqual(result["chronological_view"]["reversed_boundaries"],1)
            self.assertEqual(make_labels(copied),[]) #60s gap exceeds permitted15s; never fabricate missing coverage
    def test_modified_ordered_dataset_blocks_before_any_ml_import(self):
        from experiment import train
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);p=self.source(root,[self.row(10000,"a")]);order(p,root/"view")
            with (root/"view/observations.jsonl").open("a") as f:f.write(json.dumps(self.row(20000,"a"))+"\n")
            with self.assertRaisesRegex(ValueError,"digest mismatch"):train(root/"view",root/"model")
            self.assertFalse((root/"model").exists())
    def test_duplicate_timestamps_are_not_silently_deduplicated(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);p=self.source(root,[self.row(10000,"a"),self.row(10000,"b")])
            with self.assertRaisesRegex(ValueError,"duplicate"):order(p,root/"view")
            self.assertFalse((root/"view/audit.json").exists())
    def test_labels_never_cross_sessions_after_sort(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);rows=[self.row(t,"a" if t<30000 else "b") for t in range(0,90001,5000)]
            p=self.source(root,list(reversed(rows)));order(p,root/"view")
            copied=[json.loads(x) for x in (root/"view/observations.jsonl").read_text().splitlines()]
            labels=make_labels(copied)
            self.assertTrue(labels);self.assertTrue(all(x["session"]=="b" and x["observed_ms"]>=30000 for x in labels))
if __name__=="__main__":unittest.main()
