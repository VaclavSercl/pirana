import os
import re
import sys
import subprocess

STRATEGY_PATH = "/home/wwwenda/workspace/pirana/strategy.toml"
FEES_HTML_PATH = "/tmp/fees.html"

def get_fees_status():
    try:
        with open(FEES_HTML_PATH, 'r', encoding='utf-8') as f:
            content = f.read()
    except Exception as e:
        print(f"Error reading fees file: {e}")
        return "ERROR", "N/A", "N/A"

    # Search for Spot and Margin trades section and extract Maker and Taker fees
    pattern = r'Spot and Margin trades.*?Maker fees.*?<div[^>]*>([^<]+)</div>.*?Taker Fees.*?<div[^>]*>([^<]+)</div>'
    match = re.search(pattern, content, re.DOTALL | re.IGNORECASE)
    
    if match:
        maker_fee = match.group(1).strip()
        taker_fee = match.group(2).strip()
        is_maker_zero = maker_fee.lower() == 'zero'
        is_taker_zero = taker_fee.lower() == 'zero'
        
        if is_maker_zero and is_taker_zero:
            return "ZERO_FEE_CONFIRMED", maker_fee, taker_fee
        else:
            return "FEES_CHANGED", maker_fee, taker_fee
    else:
        return "PARSE_ERROR", "N/A", "N/A"

def update_strategy_file(tp_val, sl_val):
    if not os.path.exists(STRATEGY_PATH):
        print(f"Error: {STRATEGY_PATH} does not exist.")
        return False
        
    with open(STRATEGY_PATH, 'r', encoding='utf-8') as f:
        content = f.read()
        
    # Replace take_profit_distance_usd
    new_content = re.sub(
        r'(take_profit_distance_usd\s*=\s*)\d+\.\d+',
        f'\\g<1>{tp_val:.1f}',
        content
    )
    # Replace stop_loss_distance_usd
    new_content = re.sub(
        r'(stop_loss_distance_usd\s*=\s*)\d+\.\d+',
        f'\\g<1>{sl_val:.1f}',
        new_content
    )
    
    if new_content != content:
        with open(STRATEGY_PATH, 'w', encoding='utf-8') as f:
            f.write(new_content)
        print(f"Updated strategy.toml: TP = {tp_val}, SL = {sl_val}")
        return True
    else:
        print(f"strategy.toml is already up-to-date with: TP = {tp_val}, SL = {sl_val}")
        return False

def run_git_push():
    try:
        # Run git commands in the workspace
        workdir = "/home/wwwenda/workspace/pirana"
        print("Running git commit and push...")
        subprocess.run(["git", "add", "strategy.toml"], cwd=workdir, check=True)
        subprocess.run(["git", "commit", "-m", "chore(strategy): auto-adjust TP/SL for Bitfinex fee change"], cwd=workdir, check=True)
        subprocess.run(["git", "push", "origin", "main"], cwd=workdir, check=True)
        print("Git push completed successfully.")
        return True
    except Exception as e:
        print(f"Error during Git push: {e}")
        return False

def main():
    status, maker_fee, taker_fee = get_fees_status()
    print(f"Fee status: {status} (Maker: {maker_fee}, Taker: {taker_fee})")
    
    if status == "ZERO_FEE_CONFIRMED":
        # Ensure fast Zero-Fee strategy (TP 50.0, SL 25.0)
        updated = update_strategy_file(50.0, 25.0)
        if updated:
            print("Sentinel adjusted strategy to Zero-Fee (TP $50 / SL $25).")
        print("RESULT: KEEP_ZERO_FEE")
        
    elif status == "FEES_CHANGED":
        # Ensure safe wide zone strategy (TP 600.0, SL 300.0)
        updated = update_strategy_file(600.0, 300.0)
        if updated:
            print("Sentinel adjusted strategy to wide zone (TP $600 / SL $300) due to fee changes.")
            git_ok = run_git_push()
            if git_ok:
                print("RESULT: FEES_CHANGED_COMMIT_OK")
            else:
                print("RESULT: FEES_CHANGED_COMMIT_FAILED")
        else:
            print("RESULT: FEES_CHANGED_ALREADY_UPDATED")
            
    else:
        print(f"Unknown status or error: {status}")
        sys.exit(1)

if __name__ == "__main__":
    main()
