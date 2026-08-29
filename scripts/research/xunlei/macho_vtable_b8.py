import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 搜索虚函数表构造：str x*, [x8, #0xb8]（把函数指针写到 vtable+0xb8）
print('=== 搜索 vtable+0xb8 构造（str x*, [x8, #0xb8]）===')
for insn in md.disasm(blob, 0):
    if insn.mnemonic == 'str' and '[x8, #0xb8]' in insn.op_str:
        print(f'  {insn.address:#x}: {insn.op_str}')

# 也搜索 str x*, [x*, #0xb8]（一般 vtable 构造）
print('\n=== 搜索一般 vtable 构造（str x*, [x*, #0xb8]）===')
for insn in md.disasm(blob, 0):
    if insn.mnemonic == 'str' and '#0xb8]' in insn.op_str and 'x8' in insn.op_str:
        print(f'  {insn.address:#x}: {insn.op_str}')
