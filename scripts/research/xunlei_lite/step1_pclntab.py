#!/usr/bin/env python3
"""Step 1: robust Go pclntab parse for xllite.exe.

Fixes:
 - Use pefile for reliable section->file-offset mapping (handles PE32 vs PE32+).
 - Locate ALL pclntab magic candidates (0xfffffff1 = go>=1.18) and validate each
   by checking ptrSize in {4,8} and pad bytes.
 - Parse pcHeader + functab. On amd64 nfunc/nfiles are 8 bytes (Go 'int'/'uint').
 - Map each function VA -> file offset.
 - Extract target platformdetect symbols.
"""
import os, json, struct
import pefile

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "out")
os.makedirs(OUT, exist_ok=True)

with open(BIN, "rb") as f:
    data = f.read()
print(f"[*] read {len(data)} bytes")

pe = pefile.PE(BIN, fast_load=False)
print(f"[*] PE Machine={hex(pe.FILE_HEADER.Machine)} OptionalMagic={hex(pe.OPTIONAL_HEADER.Magic)}")
imagebase = pe.OPTIONAL_HEADER.ImageBase
print(f"[*] ImageBase = {imagebase:#x}")

def rva_to_off(rva):
    for s in pe.sections:
        if s.VirtualAddress <= rva < s.VirtualAddress + max(s.Misc_VirtualSize, s.SizeOfRawData):
            return s.PointerToRawData + (rva - s.VirtualAddress)
    return None

def off_to_rva(off):
    for s in pe.sections:
        if s.PointerToRawData <= off < s.PointerToRawData + s.SizeOfRawData:
            return s.VirtualAddress + (off - s.PointerToRawData)
    return None

# ---- find all pclntab magic candidates ----
MAGIC = b"\xf1\xff\xff\xff"
candidates = []
start = 0
while True:
    pos = data.find(MAGIC, start)
    if pos < 0:
        break
    candidates.append(pos)
    start = pos + 1
print(f"[*] found {len(candidates)} magic(f1ffffff) candidates")

def try_parse(off):
    # pcHeader: magic(4) pad1(1) pad2(1) minLC(1) ptrSize(1) nfunc(int) nfiles(uint)
    #           textStart ptrSize funcnameOffset ptrSize cuOffset ptrSize
    #           filetabOffset ptrSize pctabOffset ptrSize pclnOffset ptrSize
    p = off + 4
    pad1, pad2, min_lc, ptrsize = data[p], data[p+1], data[p+2], data[p+3]
    p += 4
    if ptrsize not in (4, 8):
        return None
    if pad1 != 0 or pad2 != 0:
        return None
    if not (1 <= min_lc <= 16):
        return None
    U = "<Q" if ptrsize == 8 else "<I"
    # nfunc is int (signed), nfiles is uint
    nfunc = struct.unpack_from("<q" if ptrsize == 8 else "<i", data, p)[0]; p += ptrsize
    nfiles = struct.unpack_from("<Q" if ptrsize == 8 else "<I", data, p)[0]; p += ptrsize
    if nfunc <= 0 or nfunc > 5_000_000:
        return None
    if nfiles <= 0 or nfiles > 200_000:
        return None
    text_start = struct.unpack_from(U, data, p)[0]; p += ptrsize
    funcname_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    cu_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    filetab_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    pctab_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    pcln_offset = struct.unpack_from(U, data, p)[0]; p += ptrsize
    return {
        "off": off, "ptrsize": ptrsize, "min_lc": min_lc,
        "nfunc": nfunc, "nfiles": nfiles, "text_start": text_start,
        "funcname_offset": funcname_offset, "cu_offset": cu_offset,
        "filetab_offset": filetab_offset, "pctab_offset": pctab_offset,
        "pcln_offset": pcln_offset,
    }

best = None
for c in candidates:
    info = try_parse(c)
    if info:
        print(f"[*] VALID pcHeader @ {c}: {info}")
        best = (c, info)
        break

if best is None:
    print("[!] no valid pcHeader found among f1ffffff candidates")
    pe.close()
    raise SystemExit(1)

off, info = best
ptrsize = info["ptrsize"]
U = "<Q" if ptrsize == 8 else "<I"
func_tab_base = off + info["pcln_offset"]
print(f"[*] func table base file offset = {func_tab_base:#x}")

entries = []
for i in range(info["nfunc"]):
    ep = func_tab_base + i * 2 * ptrsize
    if ep + 2*ptrsize > len(data):
        break
    entry_off, name_off = struct.unpack_from(U+U, data, ep)
    name_pos = off + info["funcname_offset"] + name_off
    end = data.find(b"\x00", name_pos)
    if end < 0:
        end = name_pos + 80
    try:
        fname = data[name_pos:end].decode("utf-8", "replace")
    except Exception:
        fname = "?"
    func_va = info["text_start"] + entry_off
    entries.append((func_va, fname))
print(f"[*] parsed {len(entries)} functions")

with open(os.path.join(OUT, "pclntab_funcs.txt"), "w", encoding="utf-8") as g:
    g.write(f"total functions: {len(entries)}\n")
    for func_va, fname in sorted(entries, key=lambda x: x[0]):
        g.write(f"{func_va:#010x}  {fname}\n")

target_funcs = {}
base = "gitlab.xunlei.cn/xlppc/pan-cli/pkg/platformdetect"
wanted = ["GetClientSecret", "GetClientID", "GetRawConfig", "Init", "With",
          "initNasId", "GetConfig", "GetRunnerType", "detectFile", "PlatformConfig"]
for func_va, fname in entries:
    if not fname.startswith(base):
        continue
    rva = func_va - imagebase
    fo = rva_to_off(rva)
    for t in wanted:
        if fname.endswith("." + t) or fname.endswith(t):
            target_funcs[fname] = {
                "va": hex(func_va), "rva": hex(rva),
                "fileoff": hex(fo) if fo is not None else None,
            }
            break

print("[*] target platformdetect symbols:")
for k, v in sorted(target_funcs.items()):
    print(f"    {k}: va={v['va']} rva={v['rva']} fileoff={v['fileoff']}")

with open(os.path.join(OUT, "target_funcs.json"), "w", encoding="utf-8") as g:
    json.dump(target_funcs, g, indent=2)
print(f"[*] wrote out/target_funcs.json ({len(target_funcs)} targets)")
pe.close()
