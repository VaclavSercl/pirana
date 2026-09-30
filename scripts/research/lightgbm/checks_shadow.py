import json, tempfile, unittest
from pathlib import Path
from unittest.mock import patch
from book_dataset import FEATURES
from shadow import bootstrap,Tail,Predictor,Store,sha

class FileTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.d=Path(self.tmp.name)
    def tearDown(self):self.tmp.cleanup()
    def write(self,name,records):
        p=self.d/name;p.write_bytes(b''.join(json.dumps(r).encode()+bytes([10]) for r in records));return p
    def test_latest_bootstrap_and_bound(self):
        p=self.write('1-a.jsonl',[{'kind':'session_start'},{'kind':'frame'},{'kind':'session_start'}])
        path,offset=bootstrap(self.d);self.assertEqual(path,p);self.assertGreater(offset,0)
        with self.assertRaises(ValueError):bootstrap(self.d,max_bytes=1)
    def test_rotation_and_partial_tail(self):
        p=self.write('1-a.jsonl',[{'kind':'session_start'}]);t=Tail(self.d,p,0)
        self.assertEqual(json.loads(t.read())['kind'],'session_start')
        with p.open('ab') as f:f.write(b'{"kind":')
        self.assertIsNone(t.read())
        with p.open('ab') as f:f.write(b'"frame"}'+bytes([10]))
        self.assertEqual(json.loads(t.read())['kind'],'frame')
        self.write('2-b.jsonl',[{'kind':'next'}]);self.assertIsNone(t.read());self.assertEqual(json.loads(t.read())['kind'],'next')
    def test_truncation(self):
        p=self.write('1-a.jsonl',[{'kind':'session_start'}]);t=Tail(self.d,p,0);t.read();p.write_bytes(b'')
        with self.assertRaises(ValueError):t.read()
    def test_bad_rotation_and_symlink(self):
        p=self.write('2-a.jsonl',[{'kind':'session_start'}]);t=Tail(self.d,p,0);t.read();self.write('1-b.jsonl',[])
        with self.assertRaises(ValueError):t.read()
        (self.d/'3-link.jsonl').symlink_to(p)
        with self.assertRaises(ValueError):bootstrap(self.d)
    def test_partial_rotated_segment(self):
        p=self.write('1-a.jsonl',[{'kind':'session_start'}]);t=Tail(self.d,p,0);t.read()
        with p.open('ab') as f:f.write(b'{')
        self.write('2-a.jsonl',[])
        with self.assertRaises(ValueError):t.read()
    def test_missing_start(self):
        self.write('1-a.jsonl',[{'kind':'frame'}])
        with self.assertRaises(ValueError):bootstrap(self.d)

class NativeModelTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import lightgbm as lgb,numpy as np
        cls.tmp=tempfile.TemporaryDirectory();cls.d=Path(cls.tmp.name)
        x=np.arange(2300,dtype=float).reshape(100,23)
        m=lgb.train(dict(objective='regression',num_threads=1,verbosity=-1),lgb.Dataset(x,label=x[:,0],feature_name=FEATURES),num_boost_round=3)
        m.save_model(str(cls.d/'model.txt'));cls.model_sha=sha(cls.d/'model.txt')
        cls.manifest=dict(features=FEATURES,config=dict(production_eligible=False),model_sha256=cls.model_sha,evaluation_end_ms=100)
        (cls.d/'manifest.json').write_text(json.dumps(cls.manifest));cls.manifest_sha=sha(cls.d/'manifest.json')
    @classmethod
    def tearDownClass(cls):cls.tmp.cleanup()
    def predictor(self):return Predictor(self.d,self.manifest_sha,self.model_sha)
    def row(self):return dict(schema=1,session='s',observed_ms=200,features=[0.]*23,ask_vwap=11.,bid_vwap=10.,quantity_btc=.00046)
    def test_native_prediction_and_staleness(self):
        m=self.predictor();r=self.row();p=m.predict(r,201);self.assertFalse(p['order_authority']);self.assertEqual(p['manifest_sha256'],self.manifest_sha)
        self.assertIsNone(m.predict(r,10201));self.assertIsNone(m.predict(r,199));r['observed_ms']=100;self.assertIsNone(m.predict(r,101))
    def test_digest_refusal(self):
        with self.assertRaises(ValueError):Predictor(self.d,'0'*64,self.model_sha)
        with self.assertRaises(ValueError):Predictor(self.d,self.manifest_sha,'0'*64)
    def test_nonfinite_and_dimension(self):
        m=self.predictor();r=self.row();r['features'][0]=float('nan')
        with self.assertRaises(ValueError):m.predict(r,201)
        r['features']=[0.]
        with self.assertRaises(ValueError):m.predict(r,201)
    def test_quantity_and_feature_order(self):
        m=self.predictor();r=self.row();r['quantity_btc']=1
        with self.assertRaises(ValueError):m.predict(r,201)
        with tempfile.TemporaryDirectory() as td:
            p=Path(td)/'manifest.json';doc=dict(self.manifest,features=list(reversed(FEATURES)));p.write_text(json.dumps(doc))
            with self.assertRaises(ValueError):Predictor(td,sha(p),self.model_sha)
    def test_store_duplicate_and_restart(self):
        with tempfile.TemporaryDirectory() as td:
            p=self.predictor().predict(self.row(),201);s=Store(td);self.assertTrue(s.add(p));self.assertFalse(s.add(p));s.status({'status':'OK'});s.close()
            s=Store(td);self.assertFalse(s.add(p));self.assertEqual(s.db.execute('SELECT COUNT(*) FROM observations').fetchone()[0],1);s.close()
    def test_store_exclusive_lock(self):
        with tempfile.TemporaryDirectory() as td:
            s=Store(td)
            with self.assertRaises(BlockingIOError):Store(td)
            s.close()

if __name__=='__main__':unittest.main()
