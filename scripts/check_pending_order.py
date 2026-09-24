#!/usr/bin/env python3
import json
import time
import hmac
import hashlib
import urllib.request
import sys
import os

def main():
    key_path = "/etc/pirana/credentials/bitfinex_api_key"
    sec_path = "/etc/pirana/credentials/bitfinex_api_secret"
    
    if not os.path.exists(key_path) or not os.path.exists(sec_path):
        print("Credentials not found", file=sys.stderr)
        sys.exit(1)
        
    with open(key_path, "r") as f:
        api_key = f.read().strip()
    with open(sec_path, "r") as f:
        api_secret = f.read().strip()
        
    def query(path, payload_dict):
        nonce = str(int(time.time() * 1000000))
        body = json.dumps(payload_dict)
        sig_payload = f"{path}{nonce}{body}"
        sig = hmac.new(api_secret.encode(), sig_payload.encode(), hashlib.sha384).hexdigest()
        
        req = urllib.request.Request(
            f"https://api.bitfinex.com{path}",
            data=body.encode(),
            headers={
                "bfx-nonce": nonce,
                "bfx-apikey": api_key,
                "bfx-signature": sig,
                "Content-Type": "application/json"
            }
        )
        with urllib.request.urlopen(req, timeout=15) as resp:
            return json.loads(resp.read().decode())

    # 1. Check order history for tBTCUSD around 1789953973563
    start_ms = 1789953973563 - 60000
    end_ms = 1789953973563 + 60000
    print(f"Querying order hist for tBTCUSD [{start_ms} - {end_ms}]...")
    try:
        hist = query("/api/v2/auth/r/orders/tBTCUSD/hist", {"start": start_ms, "end": end_ms, "limit": 25})
        print(f"Found {len(hist)} orders in window:")
        for o in hist:
            print(f"  ID={o[0]}, CID={o[2]}, Created={o[4]}, Amount={o[7]}, Status={o[13]}")
    except Exception as e:
        print(f"Error querying order hist: {e}")

    # 2. Check specific order ID 244611273982 trades
    print("\nQuerying trades for order 244611273982...")
    try:
        trades = query("/api/v2/auth/r/order/tBTCUSD:244611273982/trades", {})
        print(f"Order trades: {trades}")
    except Exception as e:
        print(f"Error querying order trades: {e}")

    # 3. Check active orders
    print("\nQuerying active orders...")
    try:
        active = query("/api/v2/auth/r/orders/tBTCUSD", {})
        print(f"Active orders: {len(active)}")
        for o in active:
            print(f"  ID={o[0]}, CID={o[2]}, Created={o[4]}, Amount={o[7]}, Status={o[13]}")
    except Exception as e:
        print(f"Error querying active orders: {e}")

    # 4. Check wallets
    print("\nQuerying wallets...")
    try:
        wallets = query("/api/v2/auth/r/wallets", {})
        for w in wallets:
            if w[1] in ("BTC", "USD"):
                print(f"  Type={w[0]}, Asset={w[1]}, Balance={w[2]}, Available={w[4]}")
    except Exception as e:
        print(f"Error querying wallets: {e}")

if __name__ == "__main__":
    main()
