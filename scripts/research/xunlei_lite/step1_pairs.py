#!/usr/bin/env python3
"""Step 1b: recover the credential map layout by scanning data sections for
Go (ptr,len) string pairs. A string literal "..." is represented as a pair
(4-byte VA of bytes, 4-byte length) in .data/.rdata. The credential map
likely stores, per platform entry, a sequence of (clientID ptr,len),
(clientSecret ptr,len), (clientVersion ptr,len) pairs. We locate each known
candidate string's byte-VA, then find those VAs referenced as 4-byte
immediates in .data/.rdata, and print the surrounding pair-group layout.
"""
import struct, pefile, os

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(OUT, exist_ok=True)
pe = pefile.PE(BIN, fast_load=False)
data = open(BIN, "rb").read()
ib = pe.OPTIONAL_HEADER.ImageBase

def off2va(off):
    for s in pe.sections:
        if s.PointerToRawData <= off < s.PointerToRawData + s.SizeOfRawData:
            return ib + s.VirtualAddress + (off - s.PointerToRawData)
    return None

# candidate strings: client ids and secrets
CAND = [
    "X9ibISwpIp8jQ4Ya", "XW-G4v1H72tgfJym", "XVJVzaJv8vKHzVCk",
    "XW5SkOhLDjnOZP7J", "Xqp0kJBXWhwaTpB6", "YGQTOphnGIuyiAxH",
    "XoL5lqbDWNW0e7QA", "Xp6vsxz_7IYVw2BB", "Yd0uSVGrNJhCC2oE",
    "Yd00NFGrNJhCC2oP", "Yd0zTVGrNJhCC2oL", "Yd0zylGrNJhCC2oN",
    "Yd0yklGrNJhCC2oH", "Yd0y91GrNJhCC2oJ", "Yd00e1GrNJhCC2oR",
]
# also include a few known-ish secret candidates by scanning near ids later
strva = {}
for c in CAND:
    idx = data.find(c.encode())
    if idx >= 0:
        v = off2va(idx)
        if v:
            strva[c] = v

print("[*] candidate string byte-VAs:")
for k, v in sorted(strva.items(), key=lambda kv: kv[1]):
    print(f"    {k} -> {v:#010x}")

# Build reverse map VA -> candidate
va2c = {v: k for k, v in strva.items()}

# Scan .data and .rdata for 4-byte values equal to a known string VA,
# then capture (len) and following pairs.
def section_bytes(name):
    for s in pe.sections:
        if s.Name.decode().rstrip("\0") == name:
            return s.PointerToRawData, s.SizeOfRawData
    return None, 0

results = []
for secname in [".data", ".rdata"]:
    ro, rs = section_bytes(secname)
    if ro is None:
        continue
    print(f"[*] scanning {secname} (file {ro:#x} len {rs})")
    # iterate 4-byte aligned
    end = ro + rs - 8
    i = ro
    while i <= end:
        val = struct.unpack_from("<I", data, i)[0]
        if val in va2c:
            # candidate pair start: ptr at i, len at i+4
            ln = struct.unpack_from("<I", data, i+4)[0]
            cand = va2c[val]
            # capture next 3 (ptr,len) pairs
            group = [(cand, ln)]
            ok = True
            j = i + 8
            for _ in range(3):
                v2 = struct.unpack_from("<I", data, j)[0]
                l2 = struct.unpack_from("<I", data, j+4)[0]
                nm2 = va2c.get(v2, f"VA={v2:#x}")
                if l2 > 0 and l2 < 256:
                    group.append((nm2, l2))
                else:
                    group.append((nm2, f"len?{l2}"))
                j += 8
            results.append((secname, i, group))
        i += 4

print(f"\n[*] found {len(results)} pair references")
with open(os.path.join(OUT, "string_pairs.txt"), "w", encoding="utf-8") as g:
    for secname, off, group in results:
        line = f"{secname} fileoff={off:#x}: " + " | ".join(f"{n}=len{l}" for n, l in group)
        print("   ", line)
        g.write(line + "\n")
pe.close()
