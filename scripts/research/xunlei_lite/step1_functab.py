#!/usr/bin/env python3
"""Step 1c: recover the Go functab by heuristic, using the funcnametab at
file 0x140e0e2. In go1.18+ the functab is a sequence of (funcOffset, nameOffset)
where nameOffset is an offset INTO the funcnametab (small) and funcOffset is an
offset from textStart (small, < text size). We find the nametab extent, then
scan candidate regions for aligned 32-bit pairs matching that profile, and
resolve names to confirm.
"""
import struct
data = open(r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe", "rb").read()

NAMETAB_START = 0x140e0e2
TEXT_START_VA = 0x1000
TEXT_SIZE = 0x12fedb5

# find nametab end: last NUL-terminated string before a long zero gap
pos = NAMETAB_START
last = NAMETAB_START
scan = NAMETAB_START
while scan < NAMETAB_START + 8_000_000:
    nxt = data.find(b"\x00", scan)
    if nxt < 0:
        break
    # name is [scan, nxt)
    if nxt - scan > 3 and b"." in data[scan:nxt] or b"/" in data[scan:nxt]:
        last = nxt
    scan = nxt + 1
NAMETAB_END = last + 1
NAMETAB_SIZE = NAMETAB_END - NAMETAB_START
print(f"[*] funcnametab approx: {NAMETAB_START:#x} .. {NAMETAB_END:#x} size={NAMETAB_SIZE:#x}")

def name_at(off):
    e = data.find(b"\x00", off)
    return data[off:e].decode("utf-8", "replace")

# Heuristic: scan .rdata for aligned pairs (funcOff, nameOff) where
#  nameOff in [0, NAMETAB_SIZE]  and  funcOff in [0, TEXT_SIZE]
# and resolving nameOff -> a string that looks like a funcsym (contains '.').
# Also require the pair to be in a contiguous run (functab is dense).
candidates = []
SCAN_LO = 0x1200000
SCAN_HI = 0x3a00000
for base in range(SCAN_LO, SCAN_HI, 4):
    if base + 8 > len(data):
        break
    fo, no = struct.unpack_from("<II", data, base)
    if 0 < no <= NAMETAB_SIZE and 0 < fo <= TEXT_SIZE:
        nm = name_at(NAMETAB_START + no)
        if len(nm) > 6 and (".(" in nm or nm.count(".") >= 1) and (" " not in nm):
            candidates.append((base, fo, no, nm))
    # stop if we've clearly left the candidate zone (too sparse) - but keep scanning

print(f"[*] candidate pairs (fo,no) found: {len(candidates)}")
# cluster: look for dense runs
if candidates:
    # print first 30
    for b, fo, no, nm in candidates[:30]:
        print(f"   off={b:#x} funcOff={fo:#x} nameOff={no:#x} -> {nm}")
    # find longest run of consecutive 8-byte-aligned entries
    # group by proximity
    runs = []
    cur = []
    prev = None
    for b, fo, no, nm in candidates:
        if prev is None or b - prev == 8:
            cur.append((b, fo, no, nm))
        else:
            if len(cur) > 5:
                runs.append(cur)
            cur = [(b, fo, no, nm)]
        prev = b
    if cur and len(cur) > 5:
        runs.append(cur)
    runs.sort(key=len, reverse=True)
    print(f"[*] longest dense runs: {[len(r) for r in runs[:5]]}")
    if runs:
        best = runs[0]
        print(f"[*] best run start off={best[0][0]:#x} count={len(best)}")
        for b, fo, no, nm in best[:20]:
            print(f"     funcVA={TEXT_START_VA+fo:#010x} {nm}")
