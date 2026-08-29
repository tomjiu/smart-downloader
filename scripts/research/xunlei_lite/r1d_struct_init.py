#!/usr/bin/env python3
"""R1d: find Go struct-initializer literals carrying client credentials.
Look for patterns like:
  ClientID:"X9ib..."  /  ClientID: "X9ib..."
  ClientSecret:"..."  /  ClientSecret: "..."
  ClientVersion:"..."  /  ClientVersion: "..."
  DeviceID:"..." (rare; usually computed)
Also generic `Key:"value"` adjacent to client id strings.
"""
import os, re

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(OUT, exist_ok=True)
with open(BIN, "rb") as f:
    data = f.read()

fields = ["ClientID", "ClientSecret", "ClientVersion", "DeviceID",
          "client_id", "client_secret", "client_version", "device_id",
          "ClientId", "ClientSecret_", "clientSecret"]

def decode(b):
    try:
        return b.decode("utf-8")
    except Exception:
        return b.decode("latin-1")

fn = os.path.join(OUT, "struct_init.txt")
total = 0
with open(fn, "w", encoding="utf-8") as g:
    for fld in fields:
        # Go string literal: Field:"value" or Field: "value"
        # value: up to ~64 chars, not containing " or newline
        rx = re.compile(re.escape(fld.encode()) + rb'\s*:\s*"(.{2,80})"')
        hits = list(rx.finditer(data))
        g.write(f"\n##### field {fld!r}: {len(hits)} hits #####\n")
        for m in hits[:60]:
            s = max(0, m.start() - 80)
            e = min(len(data), m.end() + 10)
            g.write(f"\n--- @ off={m.start()} ---\n")
            g.write(decode(data[s:e]))
            g.write("\n")
            total += 1
print(f"[done] {total} field-value hits -> out/struct_init.txt")
