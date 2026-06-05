import re

with open('/tmp/fees.html', 'r', encoding='utf-8', errors='ignore') as f:
    html = f.read()

# Strip HTML tags simply to search text
def strip_tags(text):
    return re.sub(r'<[^>]+>', '\n', text)

text = strip_tags(html)
lines = [l.strip() for l in text.split('\n') if l.strip()]

for i, line in enumerate(lines):
    if any(k in line.lower() for k in ['maker', 'taker', 'spot', 'margin']):
        context = lines[max(0, i-4): min(len(lines), i+5)]
        print(f"--- Line {i} ---")
        for cl in context:
            print("  ", cl)
