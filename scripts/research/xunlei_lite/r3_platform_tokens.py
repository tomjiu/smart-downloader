#!/usr/bin/env python3
"""R3: extract every PLATFORM=<name>:<hex32> detection rule from the binary.
These are the per-platform detect rules that feed platformdetect.PlatformConfig.
Also capture the envconfig/env var naming and any 'client_id'/'client_secret'
coupled to a platform name in the rule set.
"""
import os, re

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(OUT, exist_ok=True)
d = open(BIN, "rb").read()

# PLATFORM=<name>:<32hex>  (from startup log)
pat = re.compile(rb"PLATFORM=([a-zA-Z0-9_]+):([0-9a-fA-F]{32})")
tokens = {}
for m in pat.finditer(d):
    name = m.group(1).decode()
    hexv = m.group(2).decode()
    tokens.setdefault(name, set()).add(hexv)

print(f"[*] {len(tokens)} distinct platform names in PLATFORM= rules")
fn = os.path.join(OUT, "platform_rules.txt")
with open(fn, "w", encoding="utf-8") as g:
    for name in sorted(tokens):
        g.write(f"{name}\t{','.join(sorted(tokens[name]))}\n")
    g.write(f"\n# total platforms: {len(tokens)}\n")

# Also search for a paired config that maps platform -> client_id/secret.
# Look for JSON/YAML blobs with 'client_id' AND a platform-name key nearby.
print("[*] search for platform->credential mapping blobs ...")
# generic: a 32-hex immediately followed (within 200 bytes) by a 16-char mixed id
hits = []
for m in re.finditer(rb"([0-9a-fA-F]{32})", d):
    seg = d[m.start():m.start()+200]
    ids = re.findall(rb"[A-Za-z0-9_\-]{16}", seg)
    if ids:
        hits.append((m.group(1).decode(), [x.decode() for x in ids[:4]]))
print(f"[*] {len(hits)} 32hex->id proximities (sample 10):")
for h in hits[:10]:
    print("   ", h)
with open(os.path.join(OUT, "hex_to_id_proximity.txt"), "w", encoding="utf-8") as g:
    for h in hits:
        g.write(f"{h[0]}\t{','.join(h[1])}\n")
print(f"[done] -> out/platform_rules.txt, out/hex_to_id_proximity.txt")
