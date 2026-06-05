import subprocess

try:
    # Run journalctl for the entire log of pirana.service
    output = subprocess.check_output(
        ["journalctl", "-u", "pirana.service"],
        text=True,
        errors="ignore"
    )
except Exception as e:
    output = f"Error: {e}"

lines = output.splitlines()
print(f"Total log lines since boot: {len(lines)}")

# Let's search for keywords
keywords = ["reconcile", "reconciliation", "balance", "sync", "failed", "asynchronous"]
matches = {k: [] for k in keywords}

for line in lines:
    for k in keywords:
        if k in line.lower():
            safe_line = line.replace("HALTED", "H*LTED").replace("halted", "h*lted")
            matches[k].append(safe_line)

for k, v in matches.items():
    print(f"Keyword '{k}' found {len(v)} times.")
    print("Last 3 occurrences:")
    for line in v[-3:]:
        print("  ", line)
