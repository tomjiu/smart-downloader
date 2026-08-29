import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM
import re

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 搜索所有 ldr x3, [x8, #0xb0] 出现的位置
print('=== 所有 ldr x3, [x8, #0xb0] ===')
for insn in md.disasm(blob, 0):
    if insn.mnemonic == 'ldr' and 'x3, [x8, #0xb0]' in insn.op_str:
        print(f'  {insn.address:#x}: {insn.op_str}')

# 也搜索 ldr x3, [x8, #imm] 其中 imm 接近 0xb0
print('\n=== 所有 ldr x3, [x8, #imm]（imm >= 0xa0）===')
for insn in md.disasm(blob, 0):
    if insn.mnemonic == 'ldr':
        m = re.match(r'ldr\s+x3,\s*\[x8,\s*#(0x[0-9a-fA-F]+|\d+)\]', insn.op_str)
        if m:
            imm_str = m.group(1)
            imm = int(imm_str, 16) if imm_str.startswith('0x') else int(imm_str)
            if imm >= 0xa0:
                print(f'  {insn.address:#x}: {insn.op_str}')
