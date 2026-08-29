#!/usr/bin/env python3
"""R1e: final static sweep for any secret-bearing literal form."""
import os, re

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(OUT, exist_ok=True)
d = open(BIN, "rb").read()

tests = {
    "literal Secret:": rb"[A-Za-z]*Secret\s*:\s*\"",
    "client_secret= (form)": rb"client_secret=",
    "client_secret%3D (urlenc)": rb"client_secret%3D",
    "client_id= (form)": rb"client_id=",
    "client_secret: bare": rb"client_secret\s*:\s*\S",
    "Secret struct field": rb"Secret\b[^;]{0,60}",
}
for name, pat in tests.items():
    hits = re.findall(pat, d)
    print(f"{name}: {len(hits)}")

# dump context for every literal Secret: occurrence
fn = os.path.join(OUT, "secret_literal_ctx.txt")
with open(fn, "w", encoding="utf-8") as g:
    for m in re.finditer(rb"[A-Za-z]*Secret\s*:\s*\"", d):
        seg = d[m.start():m.start()+160]
        try:
            s = seg.decode("utf-8")
        except Exception:
            s = seg.decode("latin-1")
        g.write(f"@ {m.start()}: {s}\n")
print(f"[done] -> out/secret_literal_ctx.txt")
