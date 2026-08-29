import struct
import sys
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit')
blob = BIN.read_bytes()

nfat = struct.unpack_from('>I', blob, 4)[0]
cputypes = {0x01000007:'x86_64', 0x0100000c:'arm64', 0x7:'i386', 0xc:'arm'}
print(f'FAT 二进制, {nfat} 个架构:')
for i in range(nfat):
    cputype, cpusubtype, offset, size, align = struct.unpack_from('>IIIII', blob, 8 + i*20)
    print(f'  [{i}] {cputypes.get(cputype, hex(cputype))} @ file offset {offset:#x}, size {size:#x}')

# 提取 arm64 架构（或第一个）做进一步分析
# 通常下载引擎的核心在 arm64（Apple Silicon）
cputype0, cpusubtype0, offset0, size0, align0 = struct.unpack_from('>IIIII', blob, 8 + 0*20)

# 找 arm64
for i in range(nfat):
    cputype, cpusubtype, offset, size, align = struct.unpack_from('>IIIII', blob, 8 + i*20)
    if cputype == 0x0100000c:  # arm64
        arm64_blob = blob[offset:offset+size]
        out = BIN.parent / 'DownloadKit_arm64.bin'
        out.write_bytes(arm64_blob)
        print(f'\n提取 arm64 -> {out} ({len(arm64_blob):,} bytes)')
        break
