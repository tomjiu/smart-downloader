import struct
from pathlib import Path

base = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted\resource_1288_1304_unpacked')

def parse_pe(path):
    blob = path.read_bytes()
    e_lfanew = struct.unpack_from('<I', blob, 0x3C)[0]
    machine = struct.unpack_from('<H', blob, e_lfanew + 4)[0]
    magic = struct.unpack_from('<H', blob, e_lfanew + 24)[0]
    machines = {0x8664:'AMD64', 0x14c:'I386', 0xaa64:'ARM64'}
    fmt = 'PE32+(64bit)' if magic == 0x20b else 'PE32(32bit)'
    
    size_opt = struct.unpack_from('<H', blob, e_lfanew + 20)[0]
    nsec = struct.unpack_from('<H', blob, e_lfanew + 6)[0]
    sec_off = e_lfanew + 24 + size_opt
    secs = []
    for i in range(nsec):
        vs, va, rs, ro = struct.unpack_from('<IIII', blob, sec_off + i*40 + 8)
        secs.append((va, vs, ro, rs))
    def rva2off(rva):
        for va, vs, ro, rs in secs:
            if va <= rva < va + vs:
                return ro + (rva - va)
        return None
    
    dd_base = e_lfanew + 24 + (112 if magic == 0x20b else 96)
    rva, size = struct.unpack_from('<II', blob, dd_base + 1*8)  # import
    imports = []
    if rva and size:
        off = rva2off(rva)
        if off:
            i = 0
            while i < size // 20:
                oft, _, _, name_rva, _ = struct.unpack_from('<IIIII', blob, off + i*20)
                if oft == 0 and name_rva == 0:
                    break
                if name_rva:
                    noff = rva2off(name_rva)
                    if noff:
                        try:
                            imports.append(blob[noff:blob.index(b'\x00', noff)].decode('ascii','ignore'))
                        except Exception:
                            pass
                i += 1
    return machines.get(machine, hex(machine)), fmt, imports

for name in ['DownloadSDKProxy.dll', 'DownloadSDK.dll', 'DownloadSDKServer.exe', 'P2PBase.dll']:
    p = base / name
    if p.exists():
        machine, fmt, imports = parse_pe(p)
        print(f'{name}: {machine} {fmt}')
        print(f'   imports: {imports}')
        print()
