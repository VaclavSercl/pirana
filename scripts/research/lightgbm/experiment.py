"""Offline long-entry markout experiment. No exchange API and no trading execution.
Not a strategy backtest: overlapping hypothetical markouts are not account PnL.
"""
from pathlib import Path
from collections import defaultdict
import argparse,bisect,hashlib,json,math,sys
from book_dataset import FEATURES

SCHEMA=1
CONFIG=dict(horizon_ms=60000,max_label_lateness_ms=5000,max_observation_gap_ms=15000,
            purge_ms=300000,fee_bps_per_side=0.,split_fractions=[.6,.2,.2],
            seed=20260930,rounds=100,threads=1,production_eligible=False)

def digest(path):
    h=hashlib.sha256()
    with Path(path).open("rb") as f:
        for block in iter(lambda:f.read(1048576),b""):h.update(block)
    return h.hexdigest()

def make_labels(rows, horizon_ms=60000):
    """No forced closing trades: each candidate needs a valid future executable bid.
    Rows across session/sequence/coverage boundaries never share a label.
    """
    result=[];segments=[];segment=[];last=None
    for row in rows:
        if row.get("schema")!=SCHEMA or len(row["features"])!=len(FEATURES):raise ValueError("feature schema")
        if not all(math.isfinite(v) for v in row["features"]):raise ValueError("nonfinite feature")
        if type(row["observed_ms"]) is not int or row["observed_ms"]<0:raise ValueError("observation timestamp")
        for field in ["ask_vwap","bid_vwap","quantity_btc"]:
            value=row[field]
            if isinstance(value,bool) or not isinstance(value,(int,float)) or not math.isfinite(value) or value<=0:raise ValueError("invalid executable benchmark")
        if last and row["observed_ms"]<=last["observed_ms"]:raise ValueError("nonmonotonic observations")
        if last and (row["session"]!=last["session"] or row["observed_ms"]-last["observed_ms"]>CONFIG["max_observation_gap_ms"]):
            segments.append(segment);segment=[]
        segment.append(row);last=row
    if segment:segments.append(segment)
    for segment in segments:
        times=[x["observed_ms"] for x in segment]
        for i,row in enumerate(segment):
            j=bisect.bisect_left(times,times[i]+horizon_ms,lo=i+1)
            if j>=len(segment) or times[j]-times[i]-horizon_ms>CONFIG["max_label_lateness_ms"]:continue
            later=segment[j]
            if row["quantity_btc"]!=later["quantity_btc"] or row["ask_vwap"]<=0 or later["bid_vwap"]<=0:raise ValueError("execution benchmark")
            label=10000*(later["bid_vwap"]/row["ask_vwap"]-1)-2*CONFIG["fee_bps_per_side"]
            result.append(dict(observed_ms=times[i],label_end_ms=times[j],session=row["session"],features=row["features"],markout_bps=label))
    return result

def split(labels):
    days=sorted(set(x["observed_ms"]//86400000 for x in labels))
    if len(days)<5:raise ValueError("BLOCKED: fewer than five observed UTC dates; not enough even for exploratory chronological split")
    train_end=max(1,int(len(days)*.6));valid_end=max(train_end+1,int(len(days)*.8))
    boundaries=[days[train_end]*86400000,days[valid_end]*86400000]
    train=[x for x in labels if x["label_end_ms"]<boundaries[0]-CONFIG["purge_ms"]]
    valid=[x for x in labels if boundaries[0]<=x["observed_ms"] and x["label_end_ms"]<boundaries[1]-CONFIG["purge_ms"]]
    test=[x for x in labels if boundaries[1]<=x["observed_ms"]]
    if any(len(x)<200 for x in [train,valid,test]):raise ValueError("BLOCKED: insufficient chronological split samples")
    return train,valid,test,boundaries

def evaluate(y,pred,threshold,rows,np):
    selected=pred>threshold
    weighted=np.where(selected,y,0.)
    daily=defaultdict(list)
    for row,value in zip(rows,weighted):daily[row["observed_ms"]//86400000].append(float(value))
    averages=np.array([np.mean(v) for v in daily.values()]);ci=None
    if len(averages)>=7:
        rng=np.random.default_rng(CONFIG["seed"])
        samples=np.mean(rng.choice(averages,(1000,len(averages)),replace=True),axis=1)
        ci=np.quantile(samples,[.025,.975]).tolist()
    return dict(candidates=len(y),selected=int(selected.sum()),coverage=float(selected.mean()),
                average_markout_bps_selected=float(np.mean(y[selected])) if selected.any() else None,
                average_markout_bps_per_opportunity=float(np.mean(weighted)),
                daily_block_bootstrap_95pct=ci,observed_dates=len(daily),
                costs_stress_bps_roundtrip={str(cost):float(np.mean(np.where(selected,y-cost,0.))) for cost in [0,2,5,10]},
                actual_pnl="NOT_MEASURED",drawdown="NOT_MEASURED_REQUIRES_INVENTORY_REPLAY")

def train(audit_dir,output):
    data=Path(audit_dir)
    input_hashes={name:digest(data/name) for name in ["audit.json","observations.jsonl"]}
    audit=json.loads((data/"audit.json").read_text())
    if audit.get("feature_contract")!=2 or audit["features"]!=FEATURES:raise ValueError("audit feature contract mismatch")
    view=audit.get("chronological_view",{})
    if view.get("observations_sha256")!=input_hashes["observations.jsonl"]:
        raise ValueError("BLOCKED: chronological dataset digest mismatch or absent provenance")
    rows=[]
    with (data/"observations.jsonl").open() as source:
        for line in source:
            if len(rows)>=300000:raise ValueError("BLOCKED: bounded exploratory dataset limit300000rows; explicit partition or streaming design required")
            rows.append(json.loads(line))
    labels=make_labels(rows,horizon_ms=CONFIG["horizon_ms"]);train_rows,valid_rows,test_rows,boundaries=split(labels)
    try:
        import numpy as np
        import scipy
        import lightgbm as lgb
    except ImportError as exc:raise RuntimeError("BLOCKED: approved isolated ML dependencies are not installed") from exc
    if (lgb.__version__,np.__version__,scipy.__version__) != ("4.7.0","2.5.3","1.18.1"):
        raise RuntimeError("BLOCKED: runtime versions differ from pinned experiment")
    out=Path(output);out.mkdir(mode=0o700,parents=True,exist_ok=False)
    def xy(rows):return np.asarray([x["features"] for x in rows],dtype=float),np.asarray([x["markout_bps"] for x in rows])
    xt,yt=xy(train_rows);xv,yv=xy(valid_rows);xs,ys=xy(test_rows)
    # Simple classification benchmark; preprocessing fits training only.
    mean=xt.mean(axis=0);scale=xt.std(axis=0);scale=np.where(scale>1e-12,scale,1.)
    z=np.clip((xt-mean)/scale,-10,10);weights=np.zeros(z.shape[1]);intercept=0.
    for _ in range(400):
        probs=1/(1+np.exp(-np.clip(z@weights+intercept,-30,30)));err=probs-(yt>0)
        weights-=.05*((z.T@err)/len(yt)+.001*weights);intercept-=.05*float(err.mean())
    def logistic(x):return 1/(1+np.exp(-np.clip(np.clip((x-mean)/scale,-10,10)@weights+intercept,-30,30)))
    params=dict(objective="regression",metric="l2",num_leaves=7,max_depth=3,min_data_in_leaf=100,
                learning_rate=.05,num_threads=1,seed=CONFIG["seed"],deterministic=True,
                force_col_wise=True,verbosity=-1,lambda_l2=1.,feature_fraction=1.,bagging_fraction=1.)
    model=lgb.train(params,lgb.Dataset(xt,label=yt,feature_name=FEATURES),num_boost_round=100)
    model.save_model(str(out/"model.txt"))
    metrics={}
    for name,x,y,rs in [("validation",xv,yv,valid_rows),("test",xs,ys,test_rows)]:
        metrics[name]={"all_public_opportunities":evaluate(y,np.ones(len(y)),0,rs,np),
                       "logistic":evaluate(y,logistic(x),.5,rs,np),
                       "lightgbm":evaluate(y,model.predict(x,num_threads=1),0,rs,np)}
    benchmark=dict(mean=mean.tolist(),scale=scale.tolist(),weights=weights.tolist(),intercept=intercept)
    (out/"logistic.json").write_text(json.dumps(benchmark))
    manifest=dict(schema=SCHEMA,config=CONFIG,features=FEATURES,lightgbm_version=lgb.__version__,numpy_version=np.__version__,scipy_version=scipy.__version__,
                  parameters=params,experiment_source_sha256=digest(__file__),
                  model_sha256=digest(out/"model.txt"),audit_sha256=input_hashes["audit.json"],observations_sha256=input_hashes["observations.jsonl"],
                  train_label_end_ms=max(x["label_end_ms"] for x in train_rows),evaluation_end_ms=max(x["label_end_ms"] for x in test_rows),boundaries=boundaries,
                  counts=[len(train_rows),len(valid_rows),len(test_rows)],metrics=metrics,status="RESEARCH_ONLY_NOT_PRODUCTION_QUALIFIED",
                  limitations=["Public opportunities are not actual Pirana candidate signals", "No comparison to full existing strategy without faithful inventory replay", "Overlapping markouts are not independent trades, realized PnL or drawdown", "Observed dates are not proof of complete data days", "No hyperparameter search or live order authority"])
    if any(digest(data/name)!=value for name,value in input_hashes.items()):
        raise RuntimeError("BLOCKED: dataset mutated during experiment; partial artifacts are not eligible")
    (out/"manifest.json").write_text(json.dumps(manifest,indent=2)+"\n")
    return manifest

if __name__=="__main__":
    p=argparse.ArgumentParser();p.add_argument("--audit-dir",required=True);p.add_argument("--output",required=True);a=p.parse_args()
    try:result=train(a.audit_dir,a.output);print(json.dumps({k:result[k] for k in ["status","counts","metrics"]}))
    except (ValueError,RuntimeError) as exc:print(str(exc),file=sys.stderr);raise SystemExit(2)
