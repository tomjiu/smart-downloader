#!/usr/bin/env python3
"""disasm_xl_structs.py - 反汇编 DownloadSDKProxy.dll 的 XL_* 函数，还原 versioned struct 布局。

目标：从 XL_Init / XL_CreateBTTask_V2 / XL_AddServer / XL_QueryTaskInfo / XL_AddPeer
的完整反汇编中，提取结构体字段访问偏移（[reg+off]），精确还原 C 侧 struct 逐字段布局，
以修复 xunlei-ffi 的 ABI size 偏差。

用法:
    python disasm_xl_structs.py [--len 0x600] [--func XL_Init]
"""
import argparse
import struct
import sys
from pathlib import Path

try:
    from capstone import Cs, CS_ARCH_X86, CS_MODE_64
except ImportError:
    sys.exit('capstone not installed (pip install capstone)')

DLL = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted\resource_1288_1304_unpacked\DownloadSDKProxy.dll')

# 关注的函数
TARGETS = [
    'XL_Init',
    'XL_CreateBTTask_V2',
    'XL_CreateBTTask',
    'XL_CreateMagnetTask',
    'XL_AddServer',
    'XL_AddPeer',
    'XL_QueryTaskInfo',
    'XL_QueryTaskFlow',
]


def rva_to_off(sections, rva):
    for va, vs, raw_off, raw_size in sections:
        if va <= rva < va + vs:
            return raw_off + (rva - va)
    return None


def parse_pe_exports(blob):
    """返回 {name: rva}，解析 PE 导出表（复用 dump_exports.py 思路，免 pefile）。"""
    mz = struct.unpack_from('<H', blob, 0)[0]
    assert mz == 0x5A4D, 'not MZ'
    e_lfanew = struct.unpack_from('<I', blob, 0x3C)[0]
    magic = struct.unpack_from('<H', blob, e_lfanew + 24)[0]
    # PE32+ (0x20b) 数据目录在 opt header 偏移 112；PE32 (0x10b) 偏移 96
    dd_off = e_lfanew + (24 + 112 if magic == 0x20b else 24 + 96)
    exp_rva, exp_size = struct.unpack_from('<II', blob, dd_off)
    if exp_rva == 0:
        return {}
    nsec = struct.unpack_from('<H', blob, e_lfanew + 6)[0]
    sec_off = e_lfanew + (24 + (112 + 16 * 8 if magic == 0x20b else 96 + 16 * 8))
    # 简化：用 OptionalHeader 的 NumberOfRvaAndSizes 定位，实际 section 表在 dd 之后
    nrv = struct.unpack_from('<I', blob, e_lfanew + 24 + (108 if magic == 0x20b else 92))[0]
    sec_off = dd_off + nrv * 8
    sections = []
    for i in range(nsec):
        va, vs, raw_off, raw_size = struct.unpack_from('<IIII', blob, sec_off + i * 40 + 12)
        sections.append((va, vs, raw_off, raw_size))
    off = rva_to_off(sections, exp_rva)
    if off is None:
        return {}
    base = off
    nfunc, nnames = struct.unpack_from('<II', blob, base + 20)
    arr_fn_rva = struct.unpack_from('<I', blob, base + 28)[0]
    arr_nm_rva = struct.unpack_from('<I', blob, base + 32)[0]
    arr_ord_rva = struct.unpack_from('<I', blob, base + 36)[0]
    exports = {}
    for i in range(nnames):
        nm_rva = struct.unpack_from('<I', blob, rva_to_off(sections, arr_nm_rva) + i * 4)[0]
        ord_ = struct.unpack_from('<H', blob, rva_to_off(sections, arr_ord_rva) + i * 2)[0]
        fn_rva = struct.unpack_from('<I', blob, rva_to_off(sections, arr_fn_rva) + ord_ * 4)[0]
        noff = rva_to_off(sections, nm_rva)
        name = blob[noff:blob.index(b'\x00', noff)].decode('ascii', 'ignore')
        exports[name] = fn_rva
    return exports


def get_sections(blob):
    e_lfanew = struct.unpack_from('<I', blob, 0x3C)[0]
    magic = struct.unpack_from('<H', blob, e_lfanew + 24)[0]
    nsec = struct.unpack_from('<H', blob, e_lfanew + 6)[0]
    size_opt = struct.unpack_from('<H', blob, e_lfanew + 20)[0]
    sec_off = e_lfanew + 24 + size_opt
    sections = []
    for i in range(nsec):
        vs, va, rs, ro = struct.unpack_from('<IIII', blob, sec_off + i * 40 + 8)
        sections.append((va, vs, ro, rs))
    return sections


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--func', default=None, help='只反汇编指定函数')
    ap.add_argument('--len', default='0x400', help='反汇编长度（hex）')
    ap.add_argument('--all', action='store_true', help='反汇编所有目标函数')
    args = ap.parse_args()

    blob = DLL.read_bytes()
    exports = parse_pe_exports(blob)
    sections = get_sections(blob)

    funcs = [args.func] if args.func else (TARGETS if args.all else TARGETS)
    length = int(args.len, 16)
    md = Cs(CS_ARCH_X86, CS_MODE_64)
    md.skipdata = True

    for fn in funcs:
        if fn not in exports:
            print(f'[miss] {fn}')
            continue
        rva = exports[fn]
        off = rva_to_off(sections, rva)
        if off is None:
            print(f'[ERR] {fn} RVA {rva:#x} 不在任何节内')
            continue
        print(f'\n===== {fn}  @ RVA {rva:#x} (file off {off:#x}) =====')
        code = blob[off:off + length]
        va = 0x180000000 + rva  # 假定镜像基址 0x180000000（x64 DLL 典型）
        for insn in md.disasm(code, va):
            print(f'  {insn.address:#x}: {insn.mnemonic:<8} {insn.op_str}')


if __name__ == '__main__':
    main()
