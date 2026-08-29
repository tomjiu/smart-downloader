import struct
import sys
import re
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

# 检查已知函数地址附近的字节
known_funcs = [
    (0x58e390, '_XLStartTask'),
    (0x58e4d0, '_XLStopTask'),
    (0x58e268, '_XLReleaseTask'),
    (0x58f7f8, '_XLGetTaskInfo'),
    (0x596154, '_XLGetGlobalDownloadSpeed'),
    (0x5f86ac, '_XL_InitDownloadLib'),
]

for addr, name in known_funcs:
    print(f'\n=== {name} @ {addr:#x} ===')
    print(f'  hex: {blob[addr:addr+16].hex()}')
    
    # 搜索前面 32 字节的函数入口
    for i in range(addr - 32, addr):
        b = blob[i:i+4]
        if b == b'\xff\x0f\x00\xb0' or b == b'\xfd\x7b\x00\xa9' or b == b'\xfd\x7b\x01\xa9':
            print(f'  函数入口 @ {i:#x}: {b.hex()}')
            break
    else:
        print(f'  未找到标准函数入口模式')

# 也搜索 _XLStartTask 字符串附近
print('\n=== 搜索 _XLStartTask 字符串 ===')
for m in re.finditer(rb'_XLStartTask', blob):
    pos = m.start()
    print(f'  字符串 @ {pos:#x}')
    print(f'  前面 32 字节: {blob[pos-32:pos].hex()}')
    
    # 看前面是否有函数入口
    for i in range(pos - 32, pos):
        b = blob[i:i+4]
        if b == b'\xff\x0f\x00\xb0' or b == b'\xfd\x7b\x00\xa9' or b == b'\xfd\x7b\x01\xa9':
            print(f'  函数入口 @ {i:#x}: {b.hex()}')
            break
