#!/usr/bin/env python3
"""
Verify the Pirana Phase 1&2 knowledge graph inside SparrowDB.

SparrowDB dialect notes (empirically established):
  * `RETURN n.prop` projection is broken and yields Null -> use `RETURN n`
    and read the property Map, or filter with `MATCH (n:L {prop: 'v'})`.
  * DELETE / DETACH DELETE are not honoured, so the ingest is NOT idempotent.
"""
import json
import subprocess
from collections import Counter

MCP = "/home/wwwenda/.cargo/bin/sparrowdb-mcp"
DB = "/home/wwwenda/workspace/caslav_global_brain.db"

LABELS = ["System", "Remediation", "Finding", "Component",
          "Capability", "Evidence", "Doctrine"]

FINDINGS = ["P0-1", "P0-2", "P0-3", "P1-4", "P1-5", "P1-7", "P2-10"]


def call(query):
    reqs = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize",
         "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                    "clientInfo": {"name": "verify", "version": "1"}}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
         "params": {"name": "execute_cypher",
                    "arguments": {"db_path": DB, "query": query}}},
    ]
    blob = "\n".join(json.dumps(r) for r in reqs) + "\n"
    p = subprocess.run([MCP], input=blob, capture_output=True, text=True, timeout=90)
    for line in p.stdout.splitlines():
        try:
            m = json.loads(line)
        except json.JSONDecodeError:
            continue
        if m.get("id") == 2:
            try:
                return m["result"]["content"][0]["text"]
            except Exception:
                return json.dumps(m)[:300]
    return "(none)"


def count(query):
    txt = call(query)
    # QueryResult { columns: ["c"], rows: [[Int64(7)]] }
    if "Int64(" in txt:
        return int(txt.split("Int64(")[1].split(")")[0])
    return -1


print("=== SparrowDB: caslav_global_brain.db ===\n")

total_n = count("MATCH (n) RETURN count(n) AS c")
total_r = count("MATCH ()-[r]->() RETURN count(r) AS c")
print(f"total nodes        : {total_n}")
print(f"total relationships: {total_r}\n")

print("nodes per label:")
for lab in LABELS:
    c = count(f"MATCH (n:{lab}) RETURN count(n) AS c")
    print(f"  {lab:14} {c}")

print("\nfinding nodes present (matched by ident filter):")
for f in FINDINGS:
    c = count(f"MATCH (n:Finding {{ident: '{f}'}}) RETURN count(n) AS c")
    mark = "OK " if c >= 1 else "MISS"
    print(f"  {mark} {f:7} (count={c})")

print("\nsample node payload (System:PIRANA):")
print(" ", call("MATCH (n:System {ident: 'PIRANA'}) RETURN n")[:500])

print("\nsample node payload (Doctrine:fill_accuracy):")
print(" ", call("MATCH (n:Doctrine {ident: 'fill_accuracy'}) RETURN n")[:600])

print("\nrelationship spot-checks:")
checks = [
    ("Remediation-RESOLVES->Finding",
     "MATCH (:Remediation {ident:'PHASE_1_2'})-[r:RESOLVES]->(:Finding) RETURN count(r) AS c"),
    ("Capability-MITIGATES->Finding",
     "MATCH (:Capability)-[r:MITIGATES]->(:Finding) RETURN count(r) AS c"),
    ("Component-PART_OF->System",
     "MATCH (:Component)-[r:PART_OF]->(:System) RETURN count(r) AS c"),
    ("Doctrine-GOVERNS->System",
     "MATCH (:Doctrine)-[r:GOVERNS]->(:System) RETURN count(r) AS c"),
    ("Evidence-VALIDATES->Capability",
     "MATCH (:Evidence)-[r:VALIDATES]->(:Capability) RETURN count(r) AS c"),
]
for name, qry in checks:
    print(f"  {name:32} {count(qry)}")
