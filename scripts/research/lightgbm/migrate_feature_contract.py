"""Explicit data-contract rename, no numeric transformation or history rewrite."""
from pathlib import Path
import argparse,hashlib,json,shutil
from book_dataset import FEATURES
from experiment import digest

def migrate(source,destination):
    source=Path(source);destination=Path(destination)
    original=json.loads((source/"audit.json").read_text())
    old=list(FEATURES);old[old.index("volatility_65_bps")]="volatility_60_bps"
    if original.get("feature_contract",1)!=1 or original["features"]!=old:raise ValueError("unexpected source contract")
    provenance=json.loads((source/"parser-provenance.json").read_text())
    if digest(source/"parser-used.py")!=provenance["sha256"]:raise ValueError("parser provenance mismatch")
    src_hash=digest(source/"observations.jsonl");audit_hash=digest(source/"audit.json")
    destination.mkdir(mode=0o700,parents=True,exist_ok=False)
    shutil.copyfile(source/"observations.jsonl",destination/"observations.jsonl")
    if src_hash!=digest(destination/"observations.jsonl") or src_hash!=digest(source/"observations.jsonl") or audit_hash!=digest(source/"audit.json"):
        raise ValueError("input changed during migration; destination incomplete")
    updated=dict(original,feature_contract=2,features=FEATURES,parser_sha256=provenance["sha256"],
                 contract_migration=dict(source_audit_sha256=audit_hash,source_observations_sha256=src_hash,source=str(source),
                 source_parser_provenance=provenance,tool_sha256=digest(__file__),old_name="volatility_60_bps",new_name="volatility_65_bps",numeric_transformation=False,
                 reason="Name now matches actual retained65s window; selected before any model training, no outcome tuning"))
    (destination/"audit.json").write_text(json.dumps(updated,indent=2)+"\n")
    return updated
if __name__=="__main__":
    p=argparse.ArgumentParser();p.add_argument("--source",required=True);p.add_argument("--destination",required=True);a=p.parse_args();migrate(a.source,a.destination)
