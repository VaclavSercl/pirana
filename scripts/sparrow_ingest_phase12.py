#!/usr/bin/env python3
"""
Ingest Pirana Phase 1&2 remediation facts into the SparrowDB semantic graph
(caslav_global_brain.db) via the sparrowdb-mcp stdio JSON-RPC interface.

SparrowDB dialect constraints discovered empirically (scripts/sparrow_probe.py):
  * write clauses (CREATE / SET / MERGE) must NOT be followed by RETURN
  * the property name `key` is reserved and reads back as Null -> use `ident`
  * the official neo4j Bolt drivers reject the server handshake, so MCP stdio
    is the supported write path

Usage:
    python3 scripts/sparrow_ingest_phase12.py
"""

import json
import subprocess
import sys

MCP_BIN = "/home/wwwenda/.cargo/bin/sparrowdb-mcp"
DB_PATH = "/home/wwwenda/workspace/caslav_global_brain.db"
COMMIT = "c624472"
DATE = "2026-08-23"


def q(value):
    """Render a Python scalar as a Cypher literal."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    escaped = str(value).replace("\\", "\\\\").replace("'", "\\'")
    return f"'{escaped}'"


def props_literal(props):
    return "{" + ", ".join(f"{k}: {q(v)}" for k, v in props.items()) + "}"


# (label, ident, properties)  -- `ident` is the stable identity property
NODES = [
    ("System", "PIRANA", {
        "name": "PIRANA",
        "kind": "Institutional Hybrid AI Quantitative Trading System",
        "exchange": "Bitfinex",
        "symbol": "tBTCUSD",
        "language": "Rust",
        "host": "caslav",
        "repo": "github.com/VaclavSercl/pirana",
    }),
    ("Remediation", "PHASE_1_2", {
        "name": "Phase 1 and 2 Remediation",
        "commit": COMMIT,
        "date": DATE,
        "tests_passing": 41,
        "tests_added": 7,
        "lines_added": 408,
        "lines_removed": 125,
        "deployed": True,
    }),
    ("Finding", "P0-1", {
        "severity": "P0",
        "title": "Win-rate metric distorted by counting BUY entries as trades",
        "file": "src/main.rs",
        "impact": "win-rate understated ~2x, corrupted DynamicSizer position sizing",
        "status": "FIXED",
    }),
    ("Finding", "P0-2", {
        "severity": "P0",
        "title": "Realized PnL computed from ticker price, not real exchange fill",
        "file": "crates/pirana-execution/src/bitfinex_client.rs",
        "impact": "systematic PnL error, slippage invisible in accounting",
        "status": "FIXED",
    }),
    ("Finding", "P0-3", {
        "severity": "P0",
        "title": "Risk engine equity anchor not synced after TWR re-anchoring",
        "file": "crates/pirana-risk-engine/src/engine.rs",
        "impact": "false drawdown after deposit or withdrawal could trigger Halt",
        "status": "FIXED",
    }),
    ("Finding", "P1-4", {
        "severity": "P1",
        "title": "GovernanceEngine instantiated but never invoked (dead safety gate)",
        "file": "crates/pirana-signal-validator/src/governance.rs",
        "impact": "Defensive and Halted mode policy not enforced on signals",
        "status": "FIXED",
    }),
    ("Finding", "P1-5", {
        "severity": "P1",
        "title": "Exposure tracking unit consistency (fraction vs percent)",
        "file": "crates/pirana-risk-engine/src/exposure.rs",
        "impact": "potential exposure drift",
        "status": "VERIFIED_CONSISTENT",
        "note": "signals emit position_size_pct/100.0; invariant test added",
    }),
    ("Finding", "P1-7", {
        "severity": "P1",
        "title": "OrderRouter active_orders never drained (max_open_orders deadlock)",
        "file": "crates/pirana-execution/src/order_router.rs",
        "impact": "after 10 orders create_order permanently rejects",
        "status": "FIXED",
    }),
    ("Finding", "P2-10", {
        "severity": "P2",
        "title": "Markout tracker double-recorded each close with ticker price",
        "file": "src/main.rs",
        "impact": "inflated and skewed markout drift statistics",
        "status": "FIXED",
    }),
    ("Component", "bitfinex_client", {
        "name": "BitfinexClient",
        "crate_name": "pirana-execution",
        "path": "crates/pirana-execution/src/bitfinex_client.rs",
    }),
    ("Component", "risk_engine", {
        "name": "RiskEngine",
        "crate_name": "pirana-risk-engine",
        "path": "crates/pirana-risk-engine/src/engine.rs",
    }),
    ("Component", "governance_engine", {
        "name": "GovernanceEngine",
        "crate_name": "pirana-signal-validator",
        "path": "crates/pirana-signal-validator/src/governance.rs",
    }),
    ("Component", "order_router", {
        "name": "OrderRouter",
        "crate_name": "pirana-execution",
        "path": "crates/pirana-execution/src/order_router.rs",
    }),
    ("Component", "markout_tracker", {
        "name": "MarkoutTracker",
        "crate_name": "pirana-telemetry",
        "path": "crates/pirana-telemetry/src/markout.rs",
    }),
    ("Component", "main_loop", {
        "name": "process_ws_message",
        "crate_name": "pirana",
        "path": "src/main.rs",
    }),
    ("Capability", "OrderExecutionResult", {
        "name": "OrderExecutionResult",
        "description": "Parsed Bitfinex on-req payload exposing avg_fill_price (index 16), filled_qty (index 6), exchange_order_id (index 0)",
        "fallback": "requested price when ACK precedes fill registration",
        "introduced_in": COMMIT,
    }),
    ("Capability", "reanchor_equity", {
        "name": "RiskEngine reanchor_equity",
        "description": "Atomically re-anchors daily_start_balance and weekly_start_balance after external capital flow",
        "introduced_in": COMMIT,
    }),
    ("Capability", "governance_gate", {
        "name": "Governance gate in hot loop",
        "description": "Halted blocks all signals; Defensive permits only Hold and DefensiveHalt. Wired into BUY and SELL branches between validator and risk engine",
        "introduced_in": COMMIT,
    }),
    ("Capability", "fill_price_accounting", {
        "name": "Fill-price accounting",
        "description": "All four execution paths compute PnL, balances, win-rate and profit skimmer from the real exchange fill price",
        "introduced_in": COMMIT,
    }),
    ("Evidence", "prod_slippage_2026_08_23", {
        "name": "Live slippage observed post-deploy",
        "date": DATE,
        "samples": "-10.00, +3.00, 0.00, -2.00 USD vs signal price",
        "source": "journalctl -u pirana.service",
        "conclusion": "fill-price parser confirmed working in production",
    }),
    ("Doctrine", "fill_accuracy", {
        "name": "Fill Accuracy Doctrine",
        "rule": "PnL, markout, win-rate and profit skimming MUST derive from the exchange-reported fill price, never from the ticker snapshot at submission time",
        "established": DATE,
    }),
    ("Doctrine", "closed_trade_counting", {
        "name": "Closed-Trade Counting Doctrine",
        "rule": "trades_today counts completed round-trips only; entries never increment it",
        "established": DATE,
    }),
]

EDGES = [
    ("Remediation", "PHASE_1_2", "APPLIED_TO", "System", "PIRANA"),
    *[("Remediation", "PHASE_1_2", "RESOLVES", "Finding", f)
      for f in ["P0-1", "P0-2", "P0-3", "P1-4", "P1-5", "P1-7", "P2-10"]],
    ("Finding", "P0-1", "AFFECTS", "Component", "main_loop"),
    ("Finding", "P0-2", "AFFECTS", "Component", "bitfinex_client"),
    ("Finding", "P0-3", "AFFECTS", "Component", "risk_engine"),
    ("Finding", "P1-4", "AFFECTS", "Component", "governance_engine"),
    ("Finding", "P1-5", "AFFECTS", "Component", "risk_engine"),
    ("Finding", "P1-7", "AFFECTS", "Component", "order_router"),
    ("Finding", "P2-10", "AFFECTS", "Component", "markout_tracker"),
    ("Capability", "OrderExecutionResult", "IMPLEMENTED_IN", "Component", "bitfinex_client"),
    ("Capability", "reanchor_equity", "IMPLEMENTED_IN", "Component", "risk_engine"),
    ("Capability", "governance_gate", "IMPLEMENTED_IN", "Component", "main_loop"),
    ("Capability", "fill_price_accounting", "IMPLEMENTED_IN", "Component", "main_loop"),
    ("Capability", "OrderExecutionResult", "MITIGATES", "Finding", "P0-2"),
    ("Capability", "reanchor_equity", "MITIGATES", "Finding", "P0-3"),
    ("Capability", "governance_gate", "MITIGATES", "Finding", "P1-4"),
    ("Capability", "fill_price_accounting", "MITIGATES", "Finding", "P0-1"),
    ("Capability", "fill_price_accounting", "MITIGATES", "Finding", "P2-10"),
    ("Doctrine", "fill_accuracy", "GOVERNS", "System", "PIRANA"),
    ("Doctrine", "closed_trade_counting", "GOVERNS", "System", "PIRANA"),
    ("Capability", "fill_price_accounting", "ENFORCES", "Doctrine", "fill_accuracy"),
    ("Evidence", "prod_slippage_2026_08_23", "VALIDATES", "Capability", "OrderExecutionResult"),
    ("Evidence", "prod_slippage_2026_08_23", "OBSERVED_ON", "System", "PIRANA"),
    *[("Component", c, "PART_OF", "System", "PIRANA")
      for c in ["bitfinex_client", "risk_engine", "governance_engine",
                "order_router", "markout_tracker", "main_loop"]],
]

CLEANUP = [
    "MATCH (n:Probe) DETACH DELETE n",
    "MATCH (n:Probe2) DETACH DELETE n",
]


def cypher_statements():
    """Full ordered list of Cypher statements for the ingest."""
    stmts = []

    # purge previous probe/ingest artifacts so re-runs stay idempotent
    stmts.extend(CLEANUP)
    for label, ident, _ in NODES:
        stmts.append(f"MATCH (n:{label} {{ident: {q(ident)}}}) DETACH DELETE n")

    # nodes
    for label, ident, props in NODES:
        payload = {"ident": ident}
        payload.update(props)
        stmts.append(f"CREATE (n:{label} {props_literal(payload)})")

    # edges
    for slabel, sident, rel, tlabel, tident in EDGES:
        stmts.append(
            f"MATCH (a:{slabel} {{ident: {q(sident)}}}), "
            f"(b:{tlabel} {{ident: {q(tident)}}}) "
            f"CREATE (a)-[r:{rel}]->(b)"
        )

    return stmts


VERIFY = [
    ("total nodes", "MATCH (n) RETURN count(n) AS c"),
    ("total rels", "MATCH ()-[r]->() RETURN count(r) AS c"),
    ("findings", "MATCH (f:Finding) RETURN f.ident AS id, f.severity AS sev, f.status AS st"),
    ("capabilities", "MATCH (c:Capability) RETURN c.ident AS id"),
    ("doctrines", "MATCH (d:Doctrine) RETURN d.ident AS id"),
]


def run_batch(statements):
    """Send all statements through one MCP process; return (ok, errors)."""
    reqs = [{
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                   "clientInfo": {"name": "caslav-ingest", "version": "1.0"}},
    }]
    for i, stmt in enumerate(statements, start=2):
        reqs.append({
            "jsonrpc": "2.0", "id": i, "method": "tools/call",
            "params": {"name": "execute_cypher",
                       "arguments": {"db_path": DB_PATH, "query": stmt}},
        })
    reqs.append({
        "jsonrpc": "2.0", "id": len(reqs) + 1, "method": "tools/call",
        "params": {"name": "checkpoint", "arguments": {"db_path": DB_PATH}},
    })

    blob = "\n".join(json.dumps(r) for r in reqs) + "\n"
    proc = subprocess.run([MCP_BIN], input=blob, capture_output=True,
                          text=True, timeout=600)

    ok, errors = 0, []
    for line in proc.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        res = msg.get("result")
        if msg.get("error"):
            errors.append(msg["error"])
        elif isinstance(res, dict) and res.get("isError"):
            idx = msg.get("id", 0) - 2
            stmt = statements[idx] if 0 <= idx < len(statements) else "?"
            errors.append({"stmt": stmt[:120],
                           "err": res.get("content", [{}])[0].get("text", "")[:160]})
        else:
            ok += 1
    return ok, errors, proc.stderr


def read_query(query):
    reqs = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize",
         "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                    "clientInfo": {"name": "verify", "version": "1"}}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
         "params": {"name": "execute_cypher",
                    "arguments": {"db_path": DB_PATH, "query": query}}},
    ]
    blob = "\n".join(json.dumps(r) for r in reqs) + "\n"
    p = subprocess.run([MCP_BIN], input=blob, capture_output=True,
                       text=True, timeout=120)
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
    return "(no response)"


def main():
    stmts = cypher_statements()
    ok, errors, stderr = run_batch(stmts)

    print(f"statements sent : {len(stmts)}")
    print(f"ok responses    : {ok}")
    print(f"errors          : {len(errors)}")
    for e in errors[:12]:
        print(f"  ERR {e.get('stmt','')} -> {e.get('err', e)}")
    if stderr.strip():
        print("stderr:", stderr.strip()[:300])

    print("\n--- verification read-back ---")
    for name, query in VERIFY:
        print(f"{name:14}: {read_query(query)[:400]}")

    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
