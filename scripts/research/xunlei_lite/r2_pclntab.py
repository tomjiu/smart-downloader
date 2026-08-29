#!/usr/bin/env python3
"""R2: locate Go pclntab in xllite.exe and dump function symbols.

Go binaries embed a pclntab with a magic and a function name table.
 - go <1.20 magic: 0xfffffffb (LE uint32) preceded by 0xfffffffa header
 - go >=1.20 magic: b'\xf1\xff\xff\xff' (0xfffffff1)
We parse the symtab to map function name -> start RVA so we can disassemble
GetRawConfig / Init / GetClientSecret around their address.

Outputs function table to out/pclntab_funcs.txt and prints offset of targets.
"""
import os
import struct
import sys

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(OUT, exist_ok=True)

with open(BIN, "rb") as f:
    data = f.read()

# ---- minimal PE section map (for RVA<->file offset) ----
def parse_sections(data):
    e_lfanew = struct.unpack_from("<I", data, 0x3C)[0]
    opt = e_lfanew + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    nsec = struct.unpack_from("<H", data, e_lfanew + 6)[0]
    if magic == 0x20B:
        sec_off = e_lfanew + 24 + 240
    else:
        sec_off = e_lfanew + 24 + 224
    sections = []
    for i in range(nsec):
        base = sec_off + i * 40
        name = data[base:base + 8].rstrip(b"\0").decode("ascii", "replace")
        va, vs, ro, rs = struct.unpack_from("<IIII", data, base + 8)
        sections.append((name, va, vs, ro, rs))
    return sections, magic

sections, magic = parse_sections(data)
print(f"[*] sections ({len(sections)}):")
for nm, va, vs, ro, rs in sections:
    print(f"    {nm:<10} VA={va:#010x} VS={vs:#x} RO={ro:#x} RS={rs:#x}")

def rva2off(rva):
    for nm, va, vs, ro, rs in sections:
        if va <= rva < va + vs:
            return ro + (rva - va)
    return None

# ---- find pclntab magic ----
FOUND = []
# go1.20+: \xf1\xff\xff\xff
m = data.find(b"\xf1\xff\xff\xff")
if m >= 0:
    FOUND.append(("go1.20+", m))
# classic: 0xfffffffb as LE uint32 occurs as bytes fb ff ff ff
m2 = data.find(b"\xfb\xff\xff\xff")
if m2 >= 0:
    FOUND.append(("classic", m2))
print(f"[*] pclntab magic candidates: {FOUND}")

def parse_pclntab_classic(off):
    # header at off: magic(4)=fbffffff, then 0xfffffffa(4), ptrsize(1), ...
    pass

def parse_pclntab_new(off):
    # off points at magic \xf1\xff\xff\xff
    # go1.20+ pcHeader:
    #  magic      uint32  (0xfffffff1)
    #  pad1       uint8   (0x00)
    #  pad2       uint8   (0x00)
    #  minLC      uint8   (quantum, e.g. 1)
    #  ptrSize    uint8
    #  nfunc      int     (4 bytes, signed)
    #  nfiles     uint     (4 bytes)
    #  textStart  uintptr  (ptrSize)
    #  funcnameOffset  uintptr (ptrSize)   <- offset from pcHeader start
    #  cuOffset       uintptr (ptrSize)
    #  filetabOffset  uintptr (ptrSize)
    #  pctabOffset    uintptr (ptrSize)
    #  pclnOffset     uintptr (ptrSize)
    # After header: func table of nfunc entries, each = 2 x ptrSize:
    #   entryOffset (ptrSize), nameOffset (ptrSize)
    p = off + 4
    pad1 = data[p]; pad2 = data[p+1]; min_lc = data[p+2]; ptrsize = data[p+3]
    p += 4
    if ptrsize not in (4, 8):
        print(f"[ERR] unexpected ptrsize {ptrsize} at pcHeader; pad={pad1},{pad2},minLC={min_lc}")
        return None
    U = "<Q" if ptrsize == 8 else "<I"
    nfunc = struct.unpack_from("<i", data, p)[0]; p += 4
    nfiles = struct.unpack_from("<I", data, p)[0]; p += 4
    text_start = struct.unpack_from(U, data, p)[0]; p += ptrsize
    funcname_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    cu_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    filetab_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    pctab_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    pcln_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    print(f"[*] pcHeader @ {off:#x}: ptrsize={ptsize} minLC={min_lc} nfunc={nfunc} nfiles={nfiles} textStart={text_start:#x}")
    print(f"    funcnameOffset={funcname_offset} cuOffset={cu_offset} pclnOffset={pcln_offset}")
    # func table is at pcHeader + pclnOffset (in go1.18+ the func table is at pclnOffset)
    func_tab_base = off + pcln_offset
    entries = []
    for i in range(nfunc):
        ep = func_tab_base + i * 2 * ptrsize
        if ep + 2 * ptrsize > len(data):
            break
        entry_off, name_off = struct.unpack_from(U + U, data, ep)
        name_pos = off + funcname_offset + name_off
        end = data.find(b"\x00", name_pos)
        if end < 0:
            end = name_pos + 64
        fname = data[name_pos:end].decode("utf-8", "replace")
        func_va = text_start + entry_off
        entries.append((func_va, fname))
    return entries

entries = None
for tag, off in FOUND:
    if tag == "go1.20+":
        entries = parse_pclntab_new(off)
        break
if entries is None and FOUND:
    # fallback: classic
    pass

if entries is None:
    print("[!] could not parse pclntab; dumping raw nearby strings instead")
    sys.exit(0)

targets = ["GetRawConfig", "GetClientSecret", "GetClientID", "Init", "With", "initNasId", "GetRunnerType", "detectFile", "platformdetect"]
hits = []
for func_va, fname in entries:
    for t in targets:
        if t in fname:
            hits.append((func_va, fname))
            break

with open(os.path.join(OUT, "pclntab_funcs.txt"), "w", encoding="utf-8") as g:
    g.write(f"total functions: {len(entries)}\n")
    for func_va, fname in sorted(entries, key=lambda x: x[0]):
        g.write(f"{func_va:#010x}  {fname}\n")

print(f"[*] wrote {len(entries)} functions -> out/pclntab_funcs.txt")
print("[*] target symbol hits:")
for func_va, fname in sorted(hits, key=lambda x: x[0]):
    fo = rva2off(func_va)
    print(f"    VA={func_va:#010x} RVA={func_va - sections[0][1] if sections else 0:#x} fileoff={fo}  {fname}")
