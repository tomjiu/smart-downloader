import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM
import re

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 从已知代码段 0x580000 开始搜索
start = 0x580000
end = 0x581000
print(f'=== 搜索 {start:#x}..{end:#x} 的 ldr x3, [x8, #0xb0] ===')

for insn in md.disasm(blob[start:end], start):
    if insn.mnemonic == 'ldr' and 'x3, [x8, #0xb0]' in insn.op_str:
        print(f'  {insn.address:#x}: {insn.op_str}')

# 同时打印 0x580380..0x5803a0 的确切字节（验证指令编码）
print(f'\n=== 0x580380..0x5803a0 hex ===')
print(f'  {blob[0x580380:0x5803a0].hex()}')
