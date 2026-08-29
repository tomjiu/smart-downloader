import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM
import re

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 快速扫描：找所有对 x1 指针的写入（str/stp [x1, #imm]）
# 这些很可能是填充 out 结构体的函数
print('=== 对 x1 指针的字段写入（str/stp [x1, #imm]）===')
print('（imm 在 0..0x200 内，按函数地址分组）')

current_func = None
writes = []

for insn in md.disasm(blob, 0):
    if insn.mnemonic in ('str', 'stp', 'strb', 'strh', ' stur', 'sturq', 'sturw'):
        # 检查是否写 [x1, #imm]
        m = re.match(r'(str[a-z]*|stur[bhwq]?)\s+[xz](\d+),\s*\[x1,\s*#(-?0x[0-9a-fA-F]+|\d+)\]', insn.op_str)
        if m:
            imm_str = m.group(3)
            if imm_str.startswith('0x'):
                imm = int(imm_str, 16)
            else:
                imm = int(imm_str)
            if 0 <= imm <= 0x200:
                writes.append((insn.address, insn.mnemonic, insn.op_str, imm))

# 按地址排序，找函数边界（简单的：看 ret 指令）
print(f'找到 {len(writes)} 处写入')

# 只显示前 80 处
for addr, mnem, op, imm in writes[:80]:
    print(f'  {addr:#x}: {mnem:<8} {op:<40} imm={imm:#x}')
