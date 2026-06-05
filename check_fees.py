import re
import sys

def check_fees():
    try:
        with open('/tmp/fees.html', 'r', encoding='utf-8') as f:
            content = f.read()
    except Exception as e:
        print(f"Error reading fees file: {e}")
        sys.exit(1)

    # Search for Spot and Margin trades section and extract Maker and Taker fees
    # We look for the structure containing "Spot and Margin trades" and "Maker fees" / "Taker Fees"
    # Find the block for "Spot and Margin trades"
    pattern = r'Spot and Margin trades.*?Maker fees.*?<div[^>]*>([^<]+)</div>.*?Taker Fees.*?<div[^>]*>([^<]+)</div>'
    match = re.search(pattern, content, re.DOTALL | re.IGNORECASE)
    
    if match:
        maker_fee = match.group(1).strip()
        taker_fee = match.group(2).strip()
        print(f"Detected Spot/Margin Maker Fee: {maker_fee}")
        print(f"Detected Spot/Margin Taker Fee: {taker_fee}")
        
        is_maker_zero = maker_fee.lower() == 'zero'
        is_taker_zero = taker_fee.lower() == 'zero'
        
        if is_maker_zero and is_taker_zero:
            print("STATUS: ZERO_FEE_CONFIRMED")
            sys.exit(0)
        else:
            print("STATUS: FEES_CHANGED")
            sys.exit(2)
    else:
        print("Error: Could not parse Spot and Margin trades fee structure from HTML.")
        sys.exit(1)

if __name__ == "__main__":
    check_fees()
