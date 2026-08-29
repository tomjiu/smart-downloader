import struct
import sys
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

# 验证 vtable 地址
vtable = 0x7258e8
print(f'=== vtable @ {vtable:#x} 附近 hex ===')
print(f'  {blob[vtable:vtable+0x20].hex()}')

# 逐个读取前 32 个 qword
print(f'\n=== vtable 前 32 个 qword ===')
for i in range(32):
    off = vtable + i*8
    q = struct.unpack_from('<Q', blob, off)[0]
    print(f'  +{i*8:#04x}: {q:#018x} ({q})')
