import zipfile
from pathlib import Path
import struct

APK = Path(r'C:\Users\yezi6\Downloads\x-player-guanwang.apk')
OUT = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_android')
OUT.mkdir(parents=True, exist_ok=True)

z = zipfile.ZipFile(APK)
# 提取 arm64 的 thunder sdk
targets = [
    'lib/arm64-v8a/libxl_thunder_sdk.so',
    'lib/arm64-v8a/libscrape.so',
]
for t in targets:
    data = z.read(t)
    out_path = OUT / Path(t).name
    out_path.write_bytes(data)
    print(f'提取 {t} -> {out_path} ({len(data):,} bytes)')

# 解析 ELF 头，确认架构
so = OUT / 'libxl_thunder_sdk.so'
blob = so.read_bytes()
magic = blob[:4]
print(f'\nELF magic: {magic}')
if magic == b'\x7fELF':
    elf_class = blob[4]
    elf_data = blob[5]
    e_machine = struct.unpack_from('<H', blob, 18)[0]
    print(f'  ELF class: {"64位" if elf_class == 2 else "32位"}')
    print(f'  endian: {"小端" if elf_data == 1 else "大端"}')
    machines = {0xB7:'AArch64', 0x28:'ARM', 0x3E:'x86-64', 0x3:'x86'}
    print(f'  machine: {machines.get(e_machine, hex(e_machine))}')
