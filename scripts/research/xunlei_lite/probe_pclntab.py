#!/usr/bin/env python3
"""Probe all pclntab magic variants and validate by decoding function name 0."""
import struct

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
data = open(BIN, "rb").read()
print("size", len(data))

def decode_name(name_pos):
    end = data.find(b"\x00", name_pos)
    if end < 0:
        end = name_pos + 80
    return data[name_pos:end].decode("utf-8", "replace")

def try_go118(off, ptrsize):
    p = off + 4
    pad1, pad2, min_lc, ps = data[p], data[p+1], data[p+2], data[p+3]
    p += 4
    if ps != ptrsize:
        return None
    if pad1 != 0 or pad2 != 0:
        return None
    U = "<Q" if ptrsize == 8 else "<I"
    nfunc = struct.unpack_from("<q" if ptrsize == 8 else "<i", data, p)[0]; p += ptrsize
    nfiles = struct.unpack_from("<Q" if ptrsize == 8 else "<I", data, p)[0]; p += ptrsize
    if not (0 < nfunc < 5_000_000 and 0 < nfiles < 200_000):
        return None
    text_start = struct.unpack_from(U, data, p)[0]; p += ptrsize
    funcname_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    cu_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    filetab_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    pctab_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    pcln_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    # validate func 0 name
    func_tab_base = off + pcln_offset
    try:
        entry_off, name_off = struct.unpack_from(U+U, data, func_tab_base)
    except Exception:
        return None
    nm = decode_name(off + funcname_offset + name_off)
    # name should look like a funcsym
    if not ('.' in nm or 'runtime' in nm or 'main' in nm or len(nm) < 4):
        return None
    return dict(ptrsize=ptrsize, nfunc=nfunc, nfiles=nfiles, text_start=text_start,
                funcname_offset=funcname_offset, pcln_offset=pcln_offset,
                func0_name=nm, func_tab_base=func_tab_base)

def scan_magic(magic, label):
    print(f"\n##### {label} ({magic!r}) count={data.count(magic)} #####")
    start = 0
    while True:
        pos = data.find(magic, start)
        if pos < 0:
            break
        start = pos + 1
        for ps in (4, 8):
            r = try_go118(pos, ps)
            if r:
                print(f"  VALID @ {hex(pos)} ptrsize={ps}: nfunc={r['nfunc']} text_start={hex(r['text_start'])} func0={r['func0_name']!r}")
                return pos, r
    return None, None

for magic, label in [(b"\xf1\xff\xff\xff","go1.18+"), (b"\xfb\xff\xff\xff","classic fb"),
                     (b"\xfa\xff\xff\xff","fa"), (b"\xf0\xff\xff\xff","f0")]:
    scan_magic(magic, label)
