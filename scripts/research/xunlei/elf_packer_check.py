import struct
from pathlib import Path

SO = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_android\libxl_thunder_sdk.so')
blob = SO.read_bytes()

# PH[3] type=0x6474E550，dump 它的内容看是什么
e_phoff = struct.unpack_from('<Q', blob, 32)[0]
e_phentsize = struct.unpack_from('<H', blob, 54)[0]

# PH[3] 的 offset = 0x730580, filesz = 0x219d4
ph3_off = 0x730580
ph3_size = 0x219d4
data = blob[ph3_off:ph3_off+ph3_size]

print(f'PH[3] 内容（offset 0x730580, size {ph3_size:#x}）:')
print(f'  前 64 字节 hex: {data[:64].hex()}')

# 0x6474E550 的 ASCII
t = struct.pack('<I', 0x6474E550)
print(f'  type 0x6474E550 小端 ASCII: {t}')

# 找 ELF 头位置（如果有嵌套 ELF 说明是加壳）
print(f'\n=== 搜索嵌套 ELF 头 ===')
elf_positions = []
i = 0
while True:
    pos = blob.find(b'\x7fELF', i)
    if pos == -1:
        break
    elf_positions.append(pos)
    i = pos + 1
print(f'找到 {len(elf_positions)} 个 ELF magic: {[hex(p) for p in elf_positions]}')

# 搜索其他常见壳特征
print(f'\n=== 壳特征 ===')
for marker in [b'UPX', b'ASPack', b'thempida', b'libjiagu', b'libDexHelper', b'libbangcle', b'libsecexe', b'jiagu', b'360', b'Tencent', b'tencent']:
    if marker in blob:
        print(f'  找到: {marker}')
