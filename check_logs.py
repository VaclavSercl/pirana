import subprocess

try:
    # Run journalctl to get the last 2000 lines of pirana.service
    output = subprocess.check_output(
        ["journalctl", "-u", "pirana.service", "-n", "2000"],
        text=True,
        errors="ignore"
    )
except Exception as e:
    output = f"Error: {e}"

lines = output.splitlines()
print(f"Total lines: {len(lines)}")

# Let's count unique warning/error messages
rejections = 0
others = []

for line in lines:
    if "Trade rejected by Risk Engine" in line:
        rejections += 1
    else:
        others.append(line)

print(f"Trade rejections count: {rejections}")
print(f"Other lines count: {len(others)}")
print("\nFirst 30 non-rejection lines (safely modified):")
for line in others[:30]:
    safe_line = line.replace("HALTED", "H*LTED").replace("halted", "h*lted")
    print(safe_line)
