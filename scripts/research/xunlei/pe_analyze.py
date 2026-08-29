import struct
from pathlib import Path

DLL = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted\resource_1288_1304_unpacked\DownloadSDKProxy.dll')
blob = DLL.read_bytes()

e_lfanew = struct.unpack_from('<I', blob, 0x3C)[0]
sig = blob[e_lfanew:e_lfanew+4]
machine = struct.unpack_from('<H', blob, e_lfanew + 4)[0]
machines = {0x8664:'AMD64 (x86-64)', 0x14c:'I386 (x86)', 0xaa64:'ARM64', 0x1c0:'ARM', 0x1c4:'ARM64EC'}
print('PE signature:', sig)
print('Machine:', hex(machine), '=', machines.get(machine, 'unknown'))

magic = struct.unpack_from('<H', blob, e_lfanew + 24)[0]
print('PE format:', 'PE32+ (64-bit)' if magic == 0x20b else 'PE32 (32-bit)')
print()

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

dd_off = e_lfanew + (24 + 112 if magic == 0x20b else 24 + 96)
imp_rva, _ = struct.unpack_from('<II', blob, dd_off)
imp_off = rva2off(imp_rva)

print('=== Imported DLLs (dependencies) ===')
i = 0
while True:
    oft = struct.unpack_from('<I', blob, imp_off + i*20)[0]
    if oft == 0:
        break
    name_rva = struct.unpack_from('<I', blob, imp_off + i*20 + 12)[0]
    noff = rva2off(name_rva)
    if noff:
        name = blob[noff:blob.index(b'\x00', noff)].decode('ascii','ignore')
        print('  ', name)
    i += 1
