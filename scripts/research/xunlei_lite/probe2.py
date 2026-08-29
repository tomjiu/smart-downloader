#!/usr/bin/env python3
import struct
data = open(r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe","rb").read()
print("size", len(data))

def name_at(pos):
    e = data.find(b"\x00", pos)
    return data[pos:e].decode("utf-8","replace")

# Go classic (<1.18) pcHeader magic is 0xfffffffb (LE: fb ff ff ff), preceded by 0xfffffffa
# Layout (go1.2 - go1.17):
#  magic uint32 = 0xfffffffb  (then 0xfffffffa follows? actually order: first 0xfffffffa then 0xfffffffb)
# Standard: the symtab header starts with 0xfffffffa then 0xfffffffb
# After fb: pc (uintptr? actually) ... let's just dump contexts.

for magic in [b"\xfb\xff\xff\xff", b"\xfa\xff\xff\xff", b"\xfe\xff\xff\xff"]:
    print(f"\n==== magic {magic!r} count {data.count(magic)} ====")
    start=0; shown=0
    while shown < 6:
        pos = data.find(magic, start)
        if pos<0: break
        start=pos+1
        # dump 64 bytes
        print(hex(pos), ' '.join(f'{b:02x}' for b in data[pos:pos+64]))
        shown+=1
