import os
import re
import hmac
import hashlib
import json
import time
import urllib.request

def load_env():
    env_path = "/home/wwwenda/workspace/pirana/.env"
    env_vars = {}
    if not os.path.exists(env_path):
        env_path = ".env"
    
    if os.path.exists(env_path):
        with open(env_path, "r") as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#"):
                    parts = line.split("=", 1)
                    if len(parts) == 2:
                        k, v = parts
                        v = v.strip("'\"")
                        env_vars[k] = v
    return env_vars

def bfx_post(api_key, api_secret, endpoint, body={}):
    nonce = str(int(time.time() * 1000000))
    body_str = json.dumps(body)
    # The signature payload requires the '/api' prefix
    payload = f"{endpoint}{nonce}{body_str}"
    
    signature = hmac.new(
        api_secret.encode('utf-8'),
        payload.encode('utf-8'),
        hashlib.sha384
    ).hexdigest()
    
    # The actual HTTP request path must NOT have the '/api' prefix
    url_path = endpoint.replace("/api", "")
    url = f"https://api.bitfinex.com{url_path}"
    
    req = urllib.request.Request(url, data=body_str.encode('utf-8'), method="POST")
    req.add_header("bfx-apikey", api_key)
    req.add_header("bfx-nonce", nonce)
    req.add_header("bfx-signature", signature)
    req.add_header("Content-Type", "application/json")
    
    try:
        with urllib.request.urlopen(req, timeout=10) as res:
            return json.loads(res.read().decode('utf-8'))
    except Exception as e:
        return {"error": str(e)}

def main():
    env = load_env()
    api_key = env.get("BITFINEX_API_KEY")
    api_secret = env.get("BITFINEX_API_SECRET")
    
    if not api_key or not api_secret:
        print("API keys not found in .env")
        return
        
    print("Querying Bitfinex data...")
    wallets = bfx_post(api_key, api_secret, "/api/v2/auth/r/wallets")
    positions = bfx_post(api_key, api_secret, "/api/v2/auth/r/positions")
    orders = bfx_post(api_key, api_secret, "/api/v2/auth/r/orders")
    
    result = {
        "wallets": wallets,
        "positions": positions,
        "orders": orders
    }
    
    print("---BITFINEX_STATUS_START---")
    print(json.dumps(result, indent=2))
    print("---BITFINEX_STATUS_END---")

if __name__ == "__main__":
    main()
