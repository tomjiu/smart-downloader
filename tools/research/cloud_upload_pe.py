#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Minimal PE export-table parser + dual-encoding string scan for
cloud_upload.dll (stdlib only, offline).

- Parse the PE export table to list exported function names (adapted from
  E:/Code/tools/xunlei-re/local/pe_iat_probe.py :: pe_exports).
- Dual-encoding (UTF-8 + UTF-16LE) string scan for keywords:
  upload / url / task / token / gcid / btih / signature.

NOTE: the file is loaded as a flat blob. The reference probe assumed a
dump with a possible MZ prefix offset; for a real on-disk DLL the first
'MZ' is at offset 0, so we use the standard rva2off = rva (no prefix).
"""
import json
import os
import struct

DLL = r"C:\Program Files\Thunder Network\Thunder\program\upload\cloud_upload.dll"

KEYWORDS = ["upload", "url", "task", "token", "gcid", "btih", "signature"]


def parse_sections(blob):
    """Return list of (name, va, vs, ro, rs) for PE sections (RVA->file mapping)."""
    mz = blob.find(b"MZ")
    if mz < 0:
        return []
    e_lfanew = struct.unpack_from("<I", blob, mz + 0x3C)[0] + mz
    if blob[e_lfanew:e_lfanew + 4] != b"PE\x00\x00":
        return []
    magic = struct.unpack_from("<H", blob, e_lfanew + 24)[0]
    nsec = struct.unpack_from("<H", blob, e_lfanew + 6)[0]
    sec_off = e_lfanew + 24 + (240 if magic == 0x20B else 224)
    sections = []
    for i in range(nsec):
        name = blob[sec_off + i * 40:sec_off + i * 40 + 8].rstrip(b"\x00")
        vs, va, rs, ro = struct.unpack_from("<IIII", blob, sec_off + i * 40 + 8)
        sections.append((name, va, vs, ro, rs))
    return sections


def rva_to_off(blob, rva, sections):
    """Map an RVA to a file offset using section headers (on-disk DLL)."""
    mz = blob.find(b"MZ")
    # try sections first
    for (name, va, vs, ro, rs) in sections:
        if va <= rva < va + max(vs, rs):
            return ro + (rva - va)
    # fall back: if rva within headers range, it's at the same offset
    if rva < 0x1000:
        return mz + rva
    return None


def pe_exports(blob, pattern=""):
    """Yield (name, rva) for exports. RVA->file via section mapping (on-disk DLL)."""
    mz = blob.find(b"MZ")
    if mz < 0:
        raise ValueError("not a PE (no MZ)")
    e_lfanew = struct.unpack_from("<I", blob, mz + 0x3C)[0] + mz
    if blob[e_lfanew:e_lfanew + 4] != b"PE\x00\x00":
        raise ValueError("no PE signature")
    magic = struct.unpack_from("<H", blob, e_lfanew + 24)[0]
    dd_off = e_lfanew + 24 + (112 if magic == 0x20B else 96)
    exp_rva, exp_size = struct.unpack_from("<II", blob, dd_off + 0 * 8)
    if not exp_rva:
        return
    sections = parse_sections(blob)
    base = rva_to_off(blob, exp_rva, sections)
    if base is None:
        return
    nfunc, nnames = struct.unpack_from("<II", blob, base + 20)
    arr_fn = struct.unpack_from("<I", blob, base + 28)[0]
    arr_nm = struct.unpack_from("<I", blob, base + 32)[0]
    arr_ord = struct.unpack_from("<I", blob, base + 36)[0]
    fn_off = rva_to_off(blob, arr_fn, sections)
    nm_off = rva_to_off(blob, arr_nm, sections)
    ord_off = rva_to_off(blob, arr_ord, sections)
    if fn_off is None or nm_off is None or ord_off is None:
        return
    for i in range(nnames):
        name_rva = struct.unpack_from("<I", blob, nm_off + i * 4)[0]
        no = rva_to_off(blob, name_rva, sections)
        if no is None:
            continue
        end = blob.find(b"\x00", no)
        name = blob[no:end].decode("ascii", "ignore")
        if pattern in name:
            ord_ = struct.unpack_from("<H", blob, ord_off + i * 2)[0]
            fn_rva = struct.unpack_from("<I", blob, fn_off + ord_ * 4)[0]
            yield name, fn_rva


def scan_strings(blob, keyword):
    """Return set of contexts (bytes) around each UTF-8 and UTF-16LE occurrence."""
    out = []
    # UTF-8 (ascii-ish substrings)
    kw = keyword.encode("latin-1")
    start = 0
    while True:
        i = blob.find(kw, start)
        if i < 0:
            break
        lo = max(0, i - 24)
        hi = min(len(blob), i + len(kw) + 24)
        out.append(("utf8", blob[lo:hi]))
        start = i + 1
    # UTF-16LE
    kw16 = keyword.encode("utf-16-le")
    start = 0
    while True:
        i = blob.find(kw16, start)
        if i < 0:
            break
        lo = max(0, i - 24)
        hi = min(len(blob), i + len(kw16) + 24)
        out.append(("utf16le", blob[lo:hi]))
        start = i + 1
    return out


def printable(b):
    return bytes(c if 32 <= c < 127 else 46 for c in b).decode("latin-1")


def main():
    if not os.path.exists(DLL):
        print(f"[fatal] DLL not found: {DLL}", file=__import__("sys").stderr)
        raise SystemExit(1)
    blob = open(DLL, "rb").read()
    print(f"dll size: {len(blob)} bytes")

    exports = sorted(pe_exports(blob))
    print(f"\n== export table: {len(exports)} exported names ==")
    for name, rva in exports[:200]:
        print(f"  rva={rva:#010x}  {name}")

    # keyword string scan (limited output for the report)
    print("\n== keyword string hits ==")
    kw_result = {}
    for kw in KEYWORDS:
        ctxs = scan_strings(blob, kw)
        # de-dup by printable string
        uniq = {}
        for enc, raw in ctxs:
            s = printable(raw)
            uniq.setdefault((enc, s), 0)
            uniq[(enc, s)] += 1
        kw_result[kw] = {"utf8": 0, "utf16le": 0, "samples": []}
        for enc, raw in ctxs:
            kw_result[kw][enc] += 1
        # keep up to 5 distinct samples
        seen = 0
        for (enc, s), _ in uniq.items():
            if seen >= 5:
                break
            kw_result[kw]["samples"].append({"enc": enc, "ctx": s})
            seen += 1
        print(f"  {kw:>10}: utf8={kw_result[kw]['utf8']:4d}  utf16le={kw_result[kw]['utf16le']:4d}  (samples {len(kw_result[kw]['samples'])})")
        for s in kw_result[kw]["samples"]:
            print(f"      [{s['enc']}] ...{s['ctx']}...")

    os.makedirs("docs/research/xunlei", exist_ok=True)
    with open("docs/research/xunlei/_cloud_upload_scan.json", "w", encoding="utf-8") as f:
        json.dump({
            "dll": DLL,
            "size": len(blob),
            "export_count": len(exports),
            "exports": [{"name": n, "rva": rva} for n, rva in exports],
            "keyword_counts": {k: {"utf8": v["utf8"], "utf16le": v["utf16le"]} for k, v in kw_result.items()},
        }, f, ensure_ascii=False, indent=2)
    print("\n[written] docs/research/xunlei/_cloud_upload_scan.json")


if __name__ == "__main__":
    main()
