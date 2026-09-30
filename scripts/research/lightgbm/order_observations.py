"""Chronological derived view; never modify raw archives or feature values."""
from pathlib import Path
import argparse,json,sqlite3
from experiment import digest,make_labels
from book_dataset import FEATURES

def order(source,destination):
    source=Path(source);destination=Path(destination)
    hashes={n:digest(source/n) for n in ["audit.json","observations.jsonl"]}
    audit=json.loads((source/"audit.json").read_text())
    if audit.get("feature_contract")!=2 or audit["features"]!=FEATURES:raise ValueError("feature contract")
    destination.mkdir(mode=0o700,parents=True,exist_ok=False)
    # Disk-backed sort bounds Python memory; a duplicated timestamp blocks rather than guessing ownership.
    db=sqlite3.connect(destination/"ordering.sqlite3")
    try:
        db.execute("CREATE TABLE rows (mts INTEGER PRIMARY KEY, session TEXT NOT NULL, raw TEXT NOT NULL)")
        count=0;previous=None;reversals=0
        with (source/"observations.jsonl").open() as f:
            while True:
                line=f.readline(1048577)
                if not line:break
                if len(line)>1048576 or not line.endswith("\n"):raise ValueError("invalid observation line")
                row=json.loads(line);make_labels([row])
                if not isinstance(row.get("session"),str) or not row["session"]:raise ValueError("invalid session")
                count+=1
                if count>300000:raise ValueError("dataset row limit")
                if previous is not None and row["observed_ms"]<previous:reversals+=1
                previous=row["observed_ms"]
                try:db.execute("INSERT INTO rows VALUES (?,?,?)",(row["observed_ms"],row["session"],line))
                except sqlite3.IntegrityError as exc:raise ValueError("duplicate observation timestamp; reconciliation required") from exc
        db.commit()
        with (destination/"observations.jsonl").open("x") as f:
            for (raw,) in db.execute("SELECT raw FROM rows ORDER BY mts"):f.write(raw)
        if any(digest(source/n)!=h for n,h in hashes.items()):raise ValueError("source changed during ordering")
        updated=dict(audit,chronological_view=dict(source=str(source),source_sha256=hashes,
                     observations_sha256=digest(destination/"observations.jsonl"),tool_sha256=digest(__file__),
                     row_count=count,reversed_boundaries=reversals,duplicate_policy="fail_closed",
                     numeric_transformation=False,operation="stable timestamp ordering of checksum-valid session-scoped rows; labels still cannot cross sessions"))
        (destination/"audit.json").write_text(json.dumps(updated,indent=2)+"\n")
        return updated
    finally:db.close()

if __name__=="__main__":
    p=argparse.ArgumentParser();p.add_argument("--source",required=True);p.add_argument("--destination",required=True);a=p.parse_args();order(a.source,a.destination)
