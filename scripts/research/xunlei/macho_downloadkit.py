import struct
import sys
import re
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit')
blob = BIN.read_bytes()
print(f'DownloadKit: {len(blob):,} bytes ({len(blob)/1024/1024:.2f} MB)')

magic = struct.unpack_from('<I', blob, 0)[0]
print(f'Mach-O magic (LE): {magic:#010x}')

# 判断是否 fat binary
if magic == 0xfeedfacf:
    print('  MH_MAGIC_64 (64-bit 单架构)')
    cputype = struct.unpack_from('<I', blob, 4)[0]
    cputypes = {0x01000007:'x86_64', 0x0100000c:'arm64'}
    print(f'  cputype: {cputypes.get(cputype, hex(cputype))}')
elif magic == 0xfeedface:
    print('  MH_MAGIC (32-bit)')
elif magic in (0xcafebabe, 0xcafebabf):
    nfat = struct.unpack_from('>I', blob, 4)[0]
    print(f'  FAT 通用二进制, {nfat} 个架构:')
    cputypes = {0x01000007:'x86_64', 0x0100000c:'arm64'}
    for i in range(nfat):
        cputype, cpusubtype, offset, size, align = struct.unpack_from('>IIIII', blob, 8 + i*20)
        print(f'    [{i}] {cputypes.get(cputype, hex(cputype))} @ {offset} size {size}')
else:
    # 可能是大端 fat
    magic_be = struct.unpack_from('>I', blob, 0)[0]
    print(f'  magic (BE): {magic_be:#010x}')
    if magic_be in (0xcafebabe, 0xcafebabf):
        nfat = struct.unpack_from('>I', blob, 4)[0]
        print(f'  FAT (大端) {nfat} 架构')
