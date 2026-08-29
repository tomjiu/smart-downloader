#!/usr/bin/env python3
"""R1: static string deep-dive on xllite.exe.

Scan the binary for known anchors and dump context windows (UTF-8 lossy)
so we can manually read the PlatformConfig source-derived material:
struct tags, embedded yaml assets (2ev2.yaml/2ev3.yaml), the JSON routing
rule blocks, and isolation of candidate 16-char client_id / client_secret
constants.

Outputs into scripts/research/xunlei_lite/out/
"""
import os
import re
import sys

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(OUT, exist_ok=True)

print("[*] reading binary (this may take a moment)...")
with open(BIN, "rb") as f:
    data = f.read()
print(f"[*] read {len(data)} bytes")

# Decode lossy as utf-8 to find ascii-ish anchors; raw bytes also kept
try:
    text = data.decode("utf-8", "replace")
except Exception as e:
    text = data.decode("latin-1")
print(f"[*] decoded text length {len(text)}")

ANCHORS = [
    "platformdetect",
    "GetRawConfig",
    "initNasId",
    "2ev3.yaml",
    "2ev2.yaml",
    "client_secret",
    "client_id",
    "client_version",
    "device_id",
    "x-client-id",
    "x-device-id",
    "x-client-version",
    "PlatformConfig",
    "GetRunnerType",
    "detectFile",
    "yaml:",
    "With(",
    "func (",
]

def ctx(window_text, pos, before=300, after=300):
    s = max(0, pos - before)
    e = min(len(window_text), pos + after)
    return window_text[s:e]

def dump_anchor(name):
    fn = os.path.join(OUT, f"anchor_{name}.txt")
    count = 0
    with open(fn, "w", encoding="utf-8") as g:
        g.write(f"=== anchor: {name!r} ===\n")
        start = 0
        while True:
            idx = text.find(name, start)
            if idx < 0:
                break
            count += 1
            g.write(f"\n--- occurrence #{count} @ file_offset={idx} ---\n")
            g.write(ctx(text, idx, 300, 300))
            g.write("\n")
            start = idx + 1
            if count >= 40:
                g.write(f"\n[truncated after {count} occurrences]\n")
                break
    print(f"[anchor] {name!r}: {count} occurrences -> {fn}")

for a in ANCHORS:
    dump_anchor(a)

# Isolated candidate 16-char mixed-case+digit constants
print("[*] scanning for 16-char candidate client ids/secrets...")
pat = re.compile(r"[A-Za-z0-9_\-]{16}")
known = set([
    "X9ibISwpIp8jQ4Ya", "XW-G4v1H72tgfJym", "XVJVzaJv8vKHzVCk",
    "XW5SkOhLDjnOZP7J", "Xqp0kJBXWhwaTpB6", "YGQTOphnGIuyiAxH",
])
# a token is a candidate if it has upper, lower, and digit somewhere, length>=16 and <=24
def is_candidate(tok):
    if not (16 <= len(tok) <= 26):
        return False
    if "_" in tok or "-" in tok:
        # allow but require mixed
        pass
    has_up = any(c.isupper() for c in tok)
    has_lo = any(c.islower() for c in tok)
    has_dg = any(c.isdigit() for c in tok)
    return has_up and has_lo and has_dg

cands = {}
for m in pat.finditer(text):
    tok = m.group(0)
    if is_candidate(tok) and tok not in known:
        cands.setdefault(tok, 0)
        cands[tok] += 1

with open(os.path.join(OUT, "candidate_constants.txt"), "w", encoding="utf-8") as g:
    g.write("=== 16-26 char mixed-case+digit candidate constants ===\n")
    for tok, c in sorted(cands.items(), key=lambda kv: -kv[1]):
        g.write(f"{c:4d}  {tok}\n")
print(f"[candidate] {len(cands)} unique candidate constants -> out/candidate_constants.txt")

print("[done] R1 anchor dumps complete. See scripts/research/xunlei_lite/out/")
