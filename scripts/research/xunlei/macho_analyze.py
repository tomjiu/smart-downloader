import struct
import sys
import re
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Frameworks\MacXLSDKs.framework\Versions\A\MacXLSDKs')
blob = BIN.read_bytes()
print(f'文件: {BIN.name}')
print(f'大小: {len(blob):,} bytes ({len(blob)/1024/1024:.2f} MB)')

# Mach-O magic
magic = struct.unpack_from('<I', blob, 0)[0]
print(f'\nMach-O magic: {magic:#010x}')
magics = {
    0xfeedface: 'MH_MAGIC (32-bit)',
    0xfeedfacf: 'MH_MAGIC_64 (64-bit)',
    0xcafebabe: 'FAT (通用二进制)',
    0xcafebabf: 'FAT_64',
}
print(f'  = {magics.get(magic, "未知")}')

if magic in (0xcafebabe, 0xcafebabf):
    # FAT 二进制，列出架构
    nfat = struct.unpack_from('>I', blob, 4)[0]
    print(f'  通用二进制，{nfat} 个架构:')
    cputypes = {0x01000007:'x86_64', 0x0100000c:'arm64', 0x7:'i386', 0xc:'arm'}
    for i in range(nfat):
        cputype, cpusubtype, offset, size, align = struct.unpack_from('>IIIII', blob, 8 + i*20)
        print(f'    [{i}] {cputypes.get(cputype, hex(cputype))} offset={offset} size={size}')

# 提取字符串，找 XL 函数名
print('\n=== 找 XL 函数名（字符串表）===')
strings = re.findall(rb'[\x20-\x7e]{4,}', blob)
xl_funcs = set()
for s in strings:
    t = s.decode('ascii', 'ignore')
    m = re.match(r'^(XL[A-Za-z0-9]+)$', t)
    if m:
        xl_funcs.add(m.group(1))

print(f'识别到 {len(xl_funcs)} 个 XL 函数名:')
for s in sorted(xl_funcs):
    print(f'  {s}')
