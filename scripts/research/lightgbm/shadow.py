"""Bounded public-file shadow inference. No network, credentials, or orders.
Outputs are public opportunities, never Pirana decisions or trading instructions.
"""
import argparse, fcntl, hashlib, json, math, os, sqlite3, time
from pathlib import Path
from book_dataset import FEATURES, Replay

LIMIT = 1048576

def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def files(directory):
    paths = list(Path(directory).glob('*.jsonl'))
    if any(p.is_symlink() or not p.is_file() for p in paths):
        raise ValueError('unsafe source file')
    return sorted(paths, key=lambda p: (int(p.name.split('-')[0]), p.name))

def bootstrap(directory, max_bytes=256*1024*1024):
    """Find latest complete session_start in bounded recent append-only segments.
    Prefixes are read without parsing all raw frames. No session boundary => block.
    """
    paths = files(directory); remaining = max_bytes
    for path in reversed(paths):
        size = path.stat().st_size
        if size > remaining:
            break
        remaining -= size; found = None
        with path.open('rb') as stream:
            while stream.tell() < size:
                offset = stream.tell(); line = stream.readline(min(LIMIT+1, size-offset))
                if not line: raise ValueError('capture truncated during bootstrap')
                if len(line)>LIMIT: raise ValueError('oversized capture record')
                if not line.endswith(b'\n'): break
                if b'"session_start"' in line:
                    record = json.loads(line)
                    if record.get('kind') == 'session_start': found = offset
        if found is not None: return path, found
    raise ValueError('BLOCKED: no verified session start in bounded bootstrap')

class Tail:
    def __init__(self, directory, path, offset):
        self.directory=Path(directory); self.path=path; self.offset=offset
        self.inode=path.stat().st_ino; self.known={p.name for p in files(directory)}
    def read(self):
        st=self.path.stat()
        if self.path.is_symlink() or st.st_ino!=self.inode or st.st_size<self.offset:
            raise ValueError('capture replacement or truncation')
        with self.path.open('rb') as stream:
            stream.seek(self.offset); line=stream.readline(LIMIT+1)
        if len(line)>LIMIT: raise ValueError('oversized capture record')
        if line.endswith(b'\n'):
            self.offset += len(line)
            return line
        paths=files(self.directory); names={p.name for p in paths}
        if any(p.name not in self.known and p.name<=self.path.name for p in paths):
            raise ValueError('nonmonotonic capture rotation')
        self.known=names
        successors=[p for p in paths if p.name>self.path.name]
        if successors:
            if line: raise ValueError('incomplete rotated segment')
            self.path=successors[0]; self.offset=0; self.inode=self.path.stat().st_ino
        return None

class Predictor:
    def __init__(self, directory, manifest_sha, model_sha):
        import lightgbm as lgb
        import numpy as np
        import scipy
        d=Path(directory)
        if sha(d/'manifest.json')!=manifest_sha: raise ValueError('manifest digest mismatch')
        self.manifest_sha=manifest_sha
        self.manifest=json.loads((d/'manifest.json').read_text())
        if self.manifest['features']!=FEATURES or self.manifest['config']['production_eligible'] is not False:
            raise ValueError('feature or authority contract mismatch')
        if self.manifest['model_sha256']!=model_sha or sha(d/'model.txt')!=model_sha:
            raise ValueError('model digest mismatch')
        if (lgb.__version__,np.__version__,scipy.__version__)!=('4.7.0','2.5.3','1.18.1'):
            raise ValueError('runtime mismatch')
        self.model=lgb.Booster(model_file=str(d/'model.txt')); self.np=np; self.digest=model_sha
        if self.model.feature_name()!=FEATURES or self.model.num_feature()!=len(FEATURES):
            raise ValueError('native model feature mismatch')
    def predict(self,row,now_ms):
        if row.get('schema')!=1 or type(row.get('observed_ms')) is not int:
            raise ValueError('observation schema')
        age=now_ms-row['observed_ms']
        if age<0 or age>10000: return None
        if row['observed_ms']<=self.manifest['evaluation_end_ms']: return None
        vector=row['features']
        if len(vector)!=len(FEATURES) or not all(type(v) in (float,int) and math.isfinite(v) for v in vector):
            raise ValueError('invalid features')
        for k in ['ask_vwap','bid_vwap','quantity_btc']:
            if type(row[k]) not in (float,int) or not math.isfinite(row[k]) or row[k]<=0: raise ValueError('invalid prices')
        if row['quantity_btc']!=.00046 or row['bid_vwap']>row['ask_vwap']:raise ValueError('benchmark mismatch')
        began=time.perf_counter_ns()
        pred=float(self.model.predict(self.np.asarray([vector]),num_threads=1)[0])
        elapsed=(time.perf_counter_ns()-began)/1e6
        if not math.isfinite(pred): raise ValueError('nonfinite prediction')
        return dict(model_sha256=self.digest,manifest_sha256=self.manifest_sha,
                    observation=row,prediction_bps=pred,inference_ms=elapsed,processed_ms=now_ms,
                    age_ms=age,mode='PUBLIC_SHADOW_ONLY',order_authority=False)

class Store:
    def __init__(self,directory):
        self.directory=Path(directory); self.directory.mkdir(mode=0o700,exist_ok=True)
        self.lock=(self.directory/'shadow.lock').open('a')
        try: fcntl.flock(self.lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BaseException:
            self.lock.close(); raise
        self.db=sqlite3.connect(self.directory/'observations.sqlite')
        self.db.execute('PRAGMA synchronous=FULL')
        self.db.execute('CREATE TABLE IF NOT EXISTS observations (model TEXT, session TEXT, observed_ms INTEGER, payload TEXT NOT NULL, PRIMARY KEY(model,session,observed_ms))')
        self.db.commit()
    def add(self,result):
        if sum(p.stat().st_size for p in self.directory.iterdir() if p.is_file())>128*1024*1024:
            raise ValueError('shadow storage limit; no deletion')
        r=result['observation']
        with self.db:
            cursor=self.db.execute('INSERT OR IGNORE INTO observations VALUES (?,?,?,?)',
                (result['model_sha256'],r['session'],r['observed_ms'],json.dumps(result,separators=(',',':'))))
        return cursor.rowcount==1
    def status(self,value):
        tmp=self.directory/'status.tmp'
        with tmp.open('w') as stream:
            json.dump(value,stream,indent=2);stream.write('\n');stream.flush();os.fsync(stream.fileno())
        os.replace(tmp,self.directory/'status.json')
        fd=os.open(self.directory,os.O_RDONLY|os.O_DIRECTORY)
        try:os.fsync(fd)
        finally:os.close(fd)
    def close(self):
        self.db.close();self.lock.close()

def run(args):
    store=Store(args.output);began=time.monotonic();parser=Replay();count=0;stale=0;invalid=0;last_save=0;last_record=None
    status=dict(status='STARTING',order_authority=False,model_sha256=args.model_sha,manifest_sha256=args.manifest_sha)
    store.status(status)
    try:
        model=Predictor(args.model,args.manifest_sha,args.model_sha)
        path,offset=bootstrap(args.source);tail=Tail(args.source,path,offset)
        while time.monotonic()-began<args.duration:
            raw=tail.read()
            if raw:
                try:
                    record=json.loads(raw);row=parser.record(record);last_record=record.get('wall_ns',0)//1000000
                except (ValueError,TypeError,KeyError,IndexError,ArithmeticError):
                    parser.break_session('invalid_record');invalid+=1;row=None
                if row:
                    result=model.predict(row,time.time_ns()//1000000)
                    if result is None: stale+=1
                    elif store.add(result):count+=1
            else:time.sleep(.1)
            if time.monotonic()-last_save>=5:
                age=None if last_record is None else time.time_ns()//1000000-last_record
                status.update(status='OBSERVING' if count and age is not None and 0<=age<=10000 and not parser.broken else 'WARMUP_OR_STALE',predictions=count,stale_rejected=stale,invalid_records=invalid,parser=dict(parser.stats),last_record_ms=last_record,updated_ms=time.time_ns()//1000000)
                store.status(status);last_save=time.monotonic()
        status.update(status='COMPLETED' if count else 'BLOCKED_NO_FRESH_OBSERVATIONS',predictions=count,stale_rejected=stale,invalid_records=invalid,parser=dict(parser.stats),updated_ms=time.time_ns()//1000000)
        store.status(status)
        return 0 if count else 2
    except BaseException as exc:
        status.update(status='FAILED',error_type=type(exc).__name__,updated_ms=time.time_ns()//1000000)
        store.status(status);raise
    finally:store.close()

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--source',required=True);p.add_argument('--model',required=True);p.add_argument('--output',required=True);p.add_argument('--manifest-sha',required=True);p.add_argument('--model-sha',required=True);p.add_argument('--duration',type=int,default=600);a=p.parse_args()
    if not 1<=a.duration<=3600:p.error('bounded duration1..3600seconds required')
    raise SystemExit(run(a))
