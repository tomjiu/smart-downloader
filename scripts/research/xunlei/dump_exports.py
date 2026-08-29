#!/usr/bin/env python3
"""dump_exports.py - dump the full export table of a PE DLL (no pefile)."""
import sys
import struct
import os


def _parse_sections(blob, mz, e_lfanew, magic):
    nsec = struct.unpack_from('<H', blob, e_lfanew + 6)[0]
    sec_off = e_lfanew + 24 + (240 if magic == 0x20B else 224)
    sections = []
    for i in range(nsec):
        s = sec_off + i * 40
        name = blob[s:s + 8].rstrip(b'\0').decode('latin1', 'ignore')
        vs, va, rs, ro = struct.unpack_from('<IIII', blob, s + 8)
        sections.append((name, va, vs, ro, rs))
    return sections


def _rva2off(sections, rva):
    for name, va, vs, ro, rs in sections:
        if va <= rva < va + max(vs, rs):
            return ro + (rva - va)
    return None


def pe_exports_full(blob):
    """Return (dll_name, [(name_or_ord, rva)]) for every export."""
    mz = blob.find(b'MZ')
    if mz < 0:
        raise ValueError('not PE')
    e_lfanew = struct.unpack_from('<I', blob, mz + 0x3C)[0] + mz
    if blob[e_lfanew:e_lfanew + 4] != b'PE\0\0':
        raise ValueError('no PE signature')
    magic = struct.unpack_from('<H', blob, e_lfanew + 24)[0]
    dd_off = e_lfanew + 24 + (112 if magic == 0x20B else 96)
    exp_rva, exp_size = struct.unpack_from('<II', blob, dd_off + 0 * 8)
    if not exp_rva:
        return '', []
    sections = _parse_sections(blob, mz, e_lfanew, magic)
    base_off = _rva2off(sections, exp_rva)
    if base_off is None:
        raise ValueError('export dir not in any section')
    base = base_off
    name_rva = struct.unpack_from('<I', blob, base + 12)[0]
    nfunc = struct.unpack_from('<I', blob, base + 20)[0]
    nnames = struct.unpack_from('<I', blob, base + 24)[0]
    eat_rva = struct.unpack_from('<I', blob, base + 28)[0]
    ent_rva = struct.unpack_from('<I', blob, base + 32)[0]
    oat_rva = struct.unpack_from('<I', blob, base + 36)[0]

    dll_name = ''
    if name_rva:
        off = _rva2off(sections, name_rva)
        if off is not None:
            end = blob.find(b'\0', off)
            dll_name = blob[off:end].decode('ascii', 'ignore')

    exports = []
    name_entries = []
    for i in range(nnames):
        nm_rva = struct.unpack_from('<I', blob, _rva2off(sections, ent_rva) + i * 4)[0]
        noff = _rva2off(sections, nm_rva)
        if noff is None:
            continue
        nend = blob.find(b'\0', noff)
        nm = blob[noff:nend].decode('ascii', 'ignore')
        ord_ = struct.unpack_from('<H', blob, _rva2off(sections, oat_rva) + i * 2)[0]
        name_entries.append((ord_, nm))

    for i in range(nfunc):
        fn_rva = struct.unpack_from('<I', blob, _rva2off(sections, eat_rva) + i * 4)[0]
        exports.append((f'ORD:{i}', fn_rva))
    named = {}
    for ord_, nm in name_entries:
        named[ord_] = nm
    result = []
    for ord_, fn_rva in exports:
        o = int(ord_.split(':')[1])
        result.append((named.get(o, ord_), fn_rva))
    return dll_name, result


def main():
    paths = sys.argv[1:]
    for path in paths:
        blob = open(path, 'rb').read()
        dll_name, exports = pe_exports_full(blob)
        print(f'\n=== {os.path.basename(path)} (internal name: {dll_name}) ===')
        print(f'total exports: {len(exports)}')
        # sort by name
        named = [e for e in exports if not e[0].startswith('ORD:')]
        ords = [e for e in exports if e[0].startswith('ORD:')]
        named.sort(key=lambda x: x[0].lower())
        ords.sort(key=lambda x: int(x[0].split(':')[1]))
        for name, rva in named:
            print(f'  {name}')
        for name, rva in ords:
            print(f'  {name} (rva={rva:#x})')


if __name__ == '__main__':
    main()
