#!/usr/bin/env python3
"""
unlock_btc_reserve.py — Operator authorization script to unlock 51,000 satoshi reserve
and place it into active trading as an open position with cost basis 81,500 USD.
"""

import sys
import os
import json
import time
import sqlite3
import shutil
from pathlib import Path

# Add scripts directory to path for pirana_accounting module
SCRIPTS_DIR = Path(__file__).resolve().parent
sys.path.append(str(SCRIPTS_DIR))
import pirana_accounting

DB_PATH = Path(os.environ.get("PIRANA_ACCOUNTING_DB", "/var/lib/pirana/accounting.sqlite3"))
POSITIONS_PATH = Path(os.environ.get("PIRANA_POSITION_SNAPSHOT_PATH", "/var/lib/pirana/positions.json"))

def run_migration(dry_run=False):
    print(f"=== PIRANA RESERVE UNLOCK MIGRATION (dry_run={dry_run}) ===")
    if not DB_PATH.exists():
        raise FileNotFoundError(f"Database not found: {DB_PATH}")
    if not POSITIONS_PATH.exists():
        raise FileNotFoundError(f"Positions file not found: {POSITIONS_PATH}")

    now_ms = int(time.time() * 1000)
    canonical_trade_id = 1978200001
    canonical_order_id = 244505000001
    canonical_cid = "28638000000001"
    entry_price = "81500"
    exec_amount = "0.00051"

    # 1. Connect to database
    if dry_run:
        source_con = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)
        con = sqlite3.connect(":memory:")
        source_con.backup(con)
        source_con.close()
    else:
        # Create disk backup first
        backup_db = DB_PATH.parent / f"backup_accounting_{now_ms}.sqlite3"
        print(f"Creating database backup: {backup_db}")
        source_con = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)
        bck_con = sqlite3.connect(str(backup_db))
        source_con.backup(bck_con)
        bck_con.close()
        source_con.close()

        backup_pos = POSITIONS_PATH.parent / f"backup_positions_{now_ms}.json"
        print(f"Creating positions backup: {backup_pos}")
        shutil.copy2(POSITIONS_PATH, backup_pos)

        con = sqlite3.connect(str(DB_PATH), isolation_level=None)

    # 2. Inspect current epoch
    cur = con.cursor()
    epoch_row = cur.execute("SELECT id, name, start_ms, opening_reserved_btc FROM trading_epoch").fetchone()
    print(f"Current trading epoch: {epoch_row}")
    if not epoch_row:
        raise ValueError("trading_epoch table is empty")

    # 3. Perform atomic update on database
    print("Updating trading_epoch to 0 reserved BTC...")
    cur.execute("BEGIN IMMEDIATE")
    try:
        cur.execute("UPDATE trading_epoch SET opening_reserved_btc='0' WHERE id=1")
        
        fill = {
            "cid": canonical_cid,
            "exec_amount": exec_amount,
            "exec_price": entry_price,
            "fee": "0",
            "fee_currency": "USD",
            "mts": now_ms,
            "order_id": canonical_order_id,
            "symbol": "tBTCUSD",
            "trade_id": canonical_trade_id
        }
        payload = json.dumps(fill, sort_keys=True, separators=(',', ':'))
        print(f"Inserting authorized buy fill: trade_id={canonical_trade_id}, order_id={canonical_order_id}")
        cur.execute("INSERT OR REPLACE INTO fills VALUES (?, ?, ?)", (canonical_trade_id, canonical_order_id, payload))
        cur.execute("UPDATE sync SET cursor_ms=? WHERE id=1", (now_ms,))
        cur.execute("COMMIT")
    except Exception as e:
        cur.execute("ROLLBACK")
        raise e

    # 4. Verify snapshot projection from accounting engine
    print("Verifying accounting projection...")
    rep = pirana_accounting.snapshot(con)
    op = rep.get("operational")
    if not op:
        raise ValueError("operational accounting projection is missing")
    if op.get("status") != "complete":
        raise ValueError(f"operational projection status is not complete: {op.get('status')}, issues: {op.get('issues')}")
    if op.get("reserved_btc") != "0":
        raise ValueError(f"reserved_btc is not 0: {op.get('reserved_btc')}")
    if len(op.get("open_lots", [])) != 1:
        raise ValueError(f"Expected 1 open lot, got {len(op.get('open_lots', []))}: {op.get('open_lots')}")
    
    lot = op["open_lots"][0]
    print(f"Verified open lot: remaining_btc={lot['remaining_btc']}, entry_price={lot['entry_price']}, cost_basis_usd={lot['cost_basis_usd']}")
    assert lot["remaining_btc"] == exec_amount
    assert lot["entry_price"] == entry_price

    con.close()

    # 5. Update positions.json
    print(f"Updating positions file: {POSITIONS_PATH}")
    with open(POSITIONS_PATH, "r") as f:
        pos_data = json.load(f)

    # Max position id
    existing_ids = [p["position_id"] for p in pos_data.get("positions", [])] + [p["position_id"] for p in pos_data.get("recovery_candidates", [])]
    next_pos_id = (max(existing_ids) + 1) if existing_ids else 84

    new_position = {
        "position_id": next_pos_id,
        "exchange_order_id": canonical_order_id,
        "entry_mts": now_ms,
        "entry_price": float(entry_price),
        "quantity": float(exec_amount),
        "side": "Buy",
        "tp_price": float(entry_price) + 50.0,
        "sl_price": float(entry_price) - 100.0,
        "exposure_size": 0.10,
        "is_paper": False,
        "highest_price_seen": float(entry_price),
        "lowest_price_seen": float(entry_price),
        "is_breakeven": False,
        "trailing_active": False,
        "is_rebalance": False,
        "is_shadow": False
    }

    print(f"New active position: ID={next_pos_id}, order={canonical_order_id}, qty={exec_amount}, entry={entry_price}")
    pos_data["positions"] = [new_position]
    if "recovery_candidates" not in pos_data:
        pos_data["recovery_candidates"] = []
    pos_data["recovery_candidates"].append(new_position)

    if not dry_run:
        tmp_pos = POSITIONS_PATH.parent / f"positions.json.tmp.{now_ms}"
        with open(tmp_pos, "w") as f:
            json.dump(pos_data, f, indent=2)
        os.replace(tmp_pos, POSITIONS_PATH)
        print("Positions file updated successfully.")
    else:
        print("[DRY RUN] Positions file not written to disk.")

    print("=== MIGRATION COMPLETE & VERIFIED CLEAN ===")

if __name__ == "__main__":
    dry_run = "--dry-run" in sys.argv
    run_migration(dry_run=dry_run)
