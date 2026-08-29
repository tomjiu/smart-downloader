#!/usr/bin/env python3
"""Step 1e: decisive checks for pclntab/moduledata presence.
1) Find funcnametab absVA, scan ENTIRE file for a 4-byte LE pointer to it (moduledata.funcnametab).
2) For each known platform name, find standalone Go-string objects (ptr,len) and dump neighbors
   to look for an embedded credential map (key=platform, value struct with client_id/secret strings).
"""
import struct, pefile, os, re

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(OUT, exist_ok=True)
pe = pefile.PE(BIN, fast_load=False)
data = open(BIN, "rb").read()
IB = pe.OPTIONAL_HEADER.ImageBase

def absVA(off):
    for s in pe.sections:
        RO=s.PointerToRawData; RS=s.SizeOfRawData; VA=s.VirtualAddress
        if RO<=off<RO+RS:
            return IB + VA + (off-RO)
    return None

# 1) funcnametab pointer in moduledata
nt_off = 0x140e0e2
nt_va = absVA(nt_off)
target = struct.pack("<I", nt_va)
hits = [m.start() for m in __import__("re").finditer(re.escape(target), data)]
print(f"[*] funcnametab VA = {nt_va:#x}; whole-file 4-byte refs = {len(hits)}")
for h in hits:
    print(f"     ref at file {h:#x} (sec-relative VA region)")

# 2) platform-name string objects
PLATFORMS = [b"pcxllite", b"synology", b"qnap", b"terramaster", b"raspbian", b"tv", b"pc", b"linux", b"docker", b"nas"]
found_map = {}
for plat in PLATFORMS:
    # find ALL occurrences of the platform name (exact, word-ish)
    occ = []
    p = 0
    while True:
        i = data.find(plat, p)
        if i < 0: break
        occ.append(i)
        p = i + 1
    for o in occ:
        va = absVA(o)
        tv = struct.pack("<I", va)
        refs = [m.start() for m in __import__("re").finditer(re.escape(tv), data)]
        if refs:
            found_map.setdefault(plat.decode(), []).append((o, va, refs))

print(f"\n[*] platform names with standalone Go-string object refs:")
with open(os.path.join(OUT, "platform_stringobjs.txt"), "w", encoding="utf-8") as g:
    for plat, lst in found_map.items():
        for o, va, refs in lst:
            # dump 64 bytes at the ref (the (ptr,len) pair) and 64 bytes before
            line = f"{plat}: bytedata@file {o:#x} VA {va:#x} ref@file {refs[0]:#x}"
            g.write(line + "\n")
            print("   ", line)
            # show the pair
            seg = data[refs[0]-8:refs[0]+16]
            print("        pair bytes:", " ".join(f"{b:02x}" for b in seg))
pe.close()
