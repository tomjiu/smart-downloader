import struct
import sys
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

vtable = 0x7258e8

# 直接打印 vtable 附近的确切字节
print(f'=== vtable @ {vtable:#x} 附近 64 字节 ===')
raw = blob[vtable:vtable+64]
print(f'hex: {raw.hex()}')
print(f'len: {len(raw)} bytes')

# 逐 qword 打印
print(f'\n=== qwords ===')
for i in range(8):
    off = i * 8
    q = struct.unpack_from('<Q', raw, off)[0]
    print(f'  +{off:#04x}: {q:#018x} ({q})')
