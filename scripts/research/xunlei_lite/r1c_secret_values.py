#!/usr/bin/env python3
"""R1c: find actual client_secret/client_id DATA values (not struct tags).

We look for the data forms:
  "client_secret":"<value>"   (JSON data)
  'client_secret': '<value>'  (YAML data)
  client_secret: <value>      (YAML, word-boundary)
and print context. Also dump any occurrence of "client_version":"<...>" with value.
"""
import os, re

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(OUT, exist_ok=True)
with open(BIN, "rb") as f:
    data = f.read()

patterns = [
    rb'"client_secret"\s*:\s*"',          # JSON data
    rb"'client_secret'\s*:\s*'",          # YAML single-quote
    rb'client_secret\s*:\s*\S',           # YAML bare
    rb'"client_id"\s*:\s*"',              # JSON data id
    rb'"client_version"\s*:\s*"',         # JSON data version
    rb'client_version\s*:\s*[\d.]',       # YAML version numeric
    rb'"device_id"\s*:\s*"',              # JSON data device
]

def decode(b):
    try:
        return b.decode("utf-8")
    except Exception:
        return b.decode("latin-1")

fn = os.path.join(OUT, "secret_values.txt")
total = 0
with open(fn, "w", encoding="utf-8") as g:
    for pat in patterns:
        rx = re.compile(pat)
        hits = list(rx.finditer(data))
        g.write(f"\n##### pattern {pat!r}: {len(hits)} hits #####\n")
        for m in hits[:40]:
            s = max(0, m.start() - 120)
            e = min(len(data), m.end() + 200)
            g.write(f"\n--- @ off={m.start()} ---\n")
            g.write(decode(data[s:e]))
            g.write("\n")
            total += 1
print(f"[done] {total} value hits -> out/secret_values.txt")
