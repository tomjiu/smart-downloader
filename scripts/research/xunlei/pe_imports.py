import struct
from pathlib import Path

DLL = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted\resource_1288_1304_unpacked\DownloadSDKProxy.dll')
blob = DLL.read_bytes()

e_lfanew = struct.unpack_from('<I', blob, 0x3C)[0]
magic = struct.unpack_from('<H', blob, e_lfanew + 24)[0]
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

# 数据目录：可选头 data directory（PE32+ 从 24+112 开始）
dd_base = e_lfanew + 24 + (112 if magic == 0x20b else 96)
# 目录索引 1 = import, 13 = delay import
for idx, name in [(1, 'IMPORT'), (13, 'DELAY_IMPORT')]:
    rva, size = struct.unpack_from('<II', blob, dd_base + idx*8)
    print(f'{name} directory: RVA=0x{rva:x} size={size}')
    if rva == 0 or size == 0:
        continue
    off = rva2off(rva)
    if off is None:
        print('  (RVA 无法映射)')
        continue
    # import directory 是 IMAGE_IMPORT_DESCRIPTOR 数组
    i = 0
    while i < size // 20:
        oft, _, _, name_rva, _ = struct.unpack_from('<IIIII', blob, off + i*20)
        if oft == 0 and name_rva == 0:
            break
        if name_rva:
            noff = rva2off(name_rva)
            if noff:
                try:
                    name = blob[noff:blob.index(b'\x00', noff)].decode('ascii','ignore')
                    print('   ', name)
                except Exception:
                    pass
        i += 1
