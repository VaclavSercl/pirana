import json, socket, os
from pathlib import Path
checks={}
for name,path in [('home_hidden','/home/wwwenda/.ssh'),('account_hidden','/var/lib/pirana'),('config_hidden','/etc/pirana'),('runtime_hidden','/opt/pirana'),('processes_hidden','/proc/1/environ')]:
 try:
  p=Path(path); assert not p.exists() or not os.access(p,os.R_OK); checks[name]=True
 except (PermissionError,FileNotFoundError): checks[name]=True
try:
 socket.socket(socket.AF_INET,socket.SOCK_STREAM)
 raise AssertionError('socket unexpectedly allowed')
except (PermissionError,OSError):checks['network_denied']=True
checks['public_readable']=any(Path('/data').glob('*.jsonl'))
try:
 fd=os.open('/data/SHADOW_PROBE_MUST_NOT_CREATE',os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600)
 os.close(fd);raise AssertionError('public archive writable')
except (PermissionError,OSError): checks['archive_readonly']=True
assert all(checks.values())
Path('/output/isolation.json').write_text(json.dumps(checks,indent=2))

import lightgbm, numpy, scipy
assert lightgbm.__version__=="4.7.0"
from shadow import Predictor,sha
model=Predictor("/model",sha("/model/manifest.json"),sha("/model/model.txt"))
checks["native_model_loaded"]=True
Path("/output/isolation.json").write_text(json.dumps(checks,indent=2))
